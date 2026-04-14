//! `ZellijMux` — `Multiplexer` impl backed by the zellij CLI.
//!
//! Implements cavekit-mux-zellij.md R1/R2/R3/R4/R6 (tasks T-025/26/27/28/32).
//!
//! # Design
//!
//! - Owns a `Box<dyn CommandExecutor>` so unit tests can inject a
//!   [`StubExecutor`] and assert the exact command sequence without ever
//!   spawning real `zellij` processes.
//! - Reads `$ZELLIJ` once at construction (or via `with_in_zellij` in tests)
//!   to branch between the "outside zellij" and "inside zellij" spawn paths.
//! - Maintains an internal `BTreeMap<session, next_tab_index>` under a
//!   `tokio::sync::Mutex`. Zellij does not expose a CLI to query tab indices,
//!   so `create_tab` hands out 0, 1, 2, … per-session. This is best-effort —
//!   if tabs are added out-of-band the indices will drift; v1 accepts that.
//!
//! # Session-collision policy (R1)
//!
//! The `Multiplexer` trait signature of `ensure_session(&self, name: &str) ->
//! Result<()>` has no room to return a mutated name. For v1 we therefore:
//!
//! 1. Run `zellij list-sessions`.
//! 2. If `name` is present in the output, emit a `warn!` log and proceed.
//! 3. Rely on zellij's own rejection for truly hard collisions at
//!    `create_tab` time.
//!
//! A proper short-ULID rename would require extending the trait; that is
//! tracked as a TODO.
//!
//! # `setsid` prefix
//!
//! When outside zellij, the first spawn uses `setsid zellij -s … --layout …`
//! so zellij detaches from the supervisor's controlling terminal. `setsid` is
//! a POSIX binary present on Linux and available via `brew install
//! util-linux` on macOS; when missing, the real executor will surface an
//! `io::Error` at spawn time and the caller decides whether to retry
//! without the prefix. Tests assert the command shape, not platform
//! availability.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::anyhow;
use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use ark_core::Multiplexer;
use ark_types::TabHandle;

use crate::executor::{CommandExecutor, RealExecutor};

/// Minimum supported zellij version (R6).
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 44, 1);

/// Target name for the status-bar plugin pipe (R4).
pub const PIPE_TARGET_STATUS: &str = "ark-status";

/// Target name for the picker plugin pipe (R4).
pub const PIPE_TARGET_PICKER: &str = "ark-picker";

/// Zellij-backed multiplexer. See module docs.
pub struct ZellijMux {
    executor: Box<dyn CommandExecutor>,
    in_zellij: bool,
    state: Mutex<MuxState>,
}

#[derive(Default)]
struct MuxState {
    /// `session_name` → next `tab_index` to hand out.
    session_tabs: BTreeMap<String, u32>,
    /// Sessions we have already issued a spawn command for during this
    /// supervisor's lifetime. Used to decide between "first tab spawns
    /// session" and "additional tab via `action new-tab`".
    sessions_spawned: std::collections::BTreeSet<String>,
}

impl Default for ZellijMux {
    fn default() -> Self {
        Self::new()
    }
}

impl ZellijMux {
    /// Construct with the real executor and `$ZELLIJ` detection from the
    /// process environment.
    pub fn new() -> Self {
        Self::with_executor(Box::new(RealExecutor))
    }

    /// Construct with a caller-provided executor (typically a
    /// [`StubExecutor`] in tests). `in_zellij` is read from the live
    /// environment; override with [`Self::with_in_zellij`] for tests.
    pub fn with_executor(executor: Box<dyn CommandExecutor>) -> Self {
        let in_zellij = std::env::var_os("ZELLIJ").is_some();
        Self {
            executor,
            in_zellij,
            state: Mutex::new(MuxState::default()),
        }
    }

    /// Test hook: force the in-zellij flag regardless of `$ZELLIJ`.
    pub fn with_in_zellij(mut self, in_zellij: bool) -> Self {
        self.in_zellij = in_zellij;
        self
    }

    /// Whether this mux believes it is running inside an existing zellij
    /// client (i.e. `$ZELLIJ` was set at construction, or the test override
    /// forced it).
    pub fn is_in_zellij(&self) -> bool {
        self.in_zellij
    }

    /// R6 — fail fast when zellij is absent or too old. Called from
    /// `ark doctor` and `ark spawn`; not part of the `Multiplexer` trait.
    pub async fn preflight(&self) -> anyhow::Result<()> {
        let output = self.executor.run("zellij", &["--version"]).await.map_err(|e| {
            anyhow!(
                "zellij not found on PATH ({e}). Install it: `brew install zellij` on macOS, or `cargo install zellij --locked`."
            )
        })?;
        if !output.status.success() {
            return Err(anyhow!(
                "`zellij --version` exited non-zero: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let line = String::from_utf8_lossy(&output.stdout);
        let version = parse_zellij_version(&line)?;
        if version < MIN_ZELLIJ_VERSION {
            return Err(anyhow!(
                "zellij >= {}.{}.{} required (found {}.{}.{}). Update: `brew upgrade zellij` or `cargo install zellij --locked`.",
                MIN_ZELLIJ_VERSION.0,
                MIN_ZELLIJ_VERSION.1,
                MIN_ZELLIJ_VERSION.2,
                version.0,
                version.1,
                version.2,
            ));
        }
        Ok(())
    }
}

/// Parse a line like `zellij 0.44.1\n` into `(major, minor, patch)`.
pub(crate) fn parse_zellij_version(line: &str) -> anyhow::Result<(u32, u32, u32)> {
    // Find the first whitespace-separated token that looks like N.N.N.
    for token in line.split_whitespace() {
        let core = token.trim_start_matches(|c: char| !c.is_ascii_digit());
        if core.is_empty() {
            continue;
        }
        let parts: Vec<&str> = core
            .split('.')
            .take(3)
            .map(|p| p.trim_end_matches(|c: char| !c.is_ascii_digit()))
            .collect();
        if parts.len() != 3 {
            continue;
        }
        if let (Ok(a), Ok(b), Ok(c)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            return Ok((a, b, c));
        }
    }
    Err(anyhow!(
        "could not parse zellij version from output: {:?}",
        line
    ))
}

#[async_trait]
impl Multiplexer for ZellijMux {
    fn kind(&self) -> &'static str {
        "zellij"
    }

    /// R1 — idempotently ensure `name` is the session we will write to.
    ///
    /// Implementation notes:
    /// - When inside zellij we unconditionally `zellij action
    ///   switch-session {name}` (create-if-missing is the default; there is
    ///   no `--create` flag on `switch-session`). This both forbids nesting
    ///   (we never call `zellij attach`) and hands control to the new
    ///   session.
    /// - When outside zellij this is a best-effort collision check via
    ///   `zellij list-sessions`. The actual spawn happens in `create_tab`
    ///   with `--layout` (R2) because `ensure_session` has no layout
    ///   argument in the trait signature.
    async fn ensure_session(&self, name: &str) -> anyhow::Result<()> {
        if self.in_zellij {
            // Forbid nesting: switch-session replaces the current client,
            // which is the supported way to enter a new session from inside
            // zellij. `attach` would nest clients and is deliberately not
            // called here.
            let out = self
                .executor
                .run("zellij", &["action", "switch-session", name])
                .await
                .map_err(|e| anyhow!("failed to spawn `zellij action switch-session`: {e}"))?;
            if !out.status.success() {
                return Err(anyhow!(
                    "`zellij action switch-session {name}` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            // Record that the session is live so create_tab takes the
            // "additional tab" path.
            let mut state = self.state.lock().await;
            state.sessions_spawned.insert(name.to_string());
            return Ok(());
        }

        // Outside zellij: check for a colliding session and warn. We do NOT
        // spawn here because the trait gives us no layout; the first tab is
        // spawned by create_tab with `--layout`.
        let list = self
            .executor
            .run("zellij", &["list-sessions"])
            .await
            .map_err(|e| anyhow!("failed to run `zellij list-sessions`: {e}"))?;
        // `list-sessions` exits non-zero when there are zero sessions; treat
        // any failure as "no existing sessions" rather than bailing.
        if list.status.success() && session_present(&list.stdout, name) {
            warn!(
                session = %name,
                "zellij session name already exists; v1 proceeds without renaming (TODO: short-ulid suffix)"
            );
        }
        Ok(())
    }

    /// R2 — create a new tab named `name` in `session`, materializing
    /// `layout_path`.
    ///
    /// First call per session: spawn the session itself with `--layout`.
    /// Subsequent calls: `zellij --session {s} action new-tab --layout {p}
    /// --name {n}`.
    async fn create_tab(
        &self,
        session: &str,
        name: &str,
        layout_path: &Path,
    ) -> anyhow::Result<TabHandle> {
        let layout_str = layout_path
            .to_str()
            .ok_or_else(|| anyhow!("layout path is not valid UTF-8: {:?}", layout_path))?;

        let mut state = self.state.lock().await;
        let first_tab = !state.sessions_spawned.contains(session);

        if first_tab {
            if self.in_zellij {
                // Inside zellij: switch-session with layout creates the
                // session and lands the caller in it.
                let out = self
                    .executor
                    .run(
                        "zellij",
                        &["action", "switch-session", session, "--layout", layout_str],
                    )
                    .await
                    .map_err(|e| anyhow!("failed to spawn `zellij action switch-session`: {e}"))?;
                if !out.status.success() {
                    return Err(anyhow!(
                        "`zellij action switch-session {session} --layout {layout_str}` failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
            } else {
                // Outside zellij: detach via setsid so the supervisor isn't
                // tied to the zellij TTY.
                let out = self
                    .executor
                    .run("setsid", &["zellij", "-s", session, "--layout", layout_str])
                    .await
                    .map_err(|e| anyhow!("failed to spawn `setsid zellij`: {e}"))?;
                if !out.status.success() {
                    return Err(anyhow!(
                        "`setsid zellij -s {session} --layout {layout_str}` failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
            }
            state.sessions_spawned.insert(session.to_string());
            state.session_tabs.insert(session.to_string(), 1);
            return Ok(TabHandle::new(session, 0, name));
        }

        // Additional tab.
        let next_index = state.session_tabs.get(session).copied().unwrap_or(0);
        let out = self
            .executor
            .run(
                "zellij",
                &[
                    "--session",
                    session,
                    "action",
                    "new-tab",
                    "--layout",
                    layout_str,
                    "--name",
                    name,
                ],
            )
            .await
            .map_err(|e| anyhow!("failed to spawn `zellij action new-tab`: {e}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`zellij --session {session} action new-tab --layout {layout_str} --name {name}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        state
            .session_tabs
            .insert(session.to_string(), next_index + 1);
        Ok(TabHandle::new(session, next_index, name))
    }

    /// R3 — close is idempotent: an error from zellij (typically because the
    /// tab has already been closed) downgrades to a `debug!` and `Ok(())`.
    async fn close_tab(&self, handle: &TabHandle) -> anyhow::Result<()> {
        let index = handle.tab_index.to_string();
        let out = self
            .executor
            .run(
                "zellij",
                &[
                    "--session",
                    &handle.session,
                    "action",
                    "close-tab-at-index",
                    &index,
                ],
            )
            .await;
        match out {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                debug!(
                    session = %handle.session,
                    tab_index = handle.tab_index,
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "close_tab: zellij returned non-zero; treating as idempotent no-op"
                );
                Ok(())
            }
            Err(e) => {
                debug!(
                    session = %handle.session,
                    tab_index = handle.tab_index,
                    error = %e,
                    "close_tab: executor failed; treating as idempotent no-op"
                );
                Ok(())
            }
        }
    }

    /// R3 — rename the tab, used as the progress fallback when the
    /// status-bar plugin isn't available.
    async fn rename_tab(&self, handle: &TabHandle, name: &str) -> anyhow::Result<()> {
        let index = handle.tab_index.to_string();
        let out = self
            .executor
            .run(
                "zellij",
                &[
                    "--session",
                    &handle.session,
                    "action",
                    "rename-tab",
                    "--tab-index",
                    &index,
                    "--name",
                    name,
                ],
            )
            .await
            .map_err(|e| anyhow!("failed to spawn `zellij action rename-tab`: {e}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`zellij action rename-tab` for {}:{} failed: {}",
                handle.session,
                handle.tab_index,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// R4 — fire-and-forget pipe to a named plugin target. Non-fatal on
    /// failure: a missing plugin or a transport error is logged at `warn`
    /// and we still return `Ok(())` so the supervisor can fall back to
    /// tab-rename progress.
    async fn pipe(&self, target_name: &str, payload: &str) -> anyhow::Result<()> {
        let out = self
            .executor
            .run("zellij", &["pipe", "--name", target_name, "--", payload])
            .await;
        match out {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                warn!(
                    target = %target_name,
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "zellij pipe returned non-zero; degrading gracefully"
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    target = %target_name,
                    error = %e,
                    "zellij pipe failed to spawn; degrading gracefully"
                );
                Ok(())
            }
        }
    }
}

/// True if a session named `name` appears on its own on any line of the
/// `zellij list-sessions` stdout. Zellij prints one session per line; the
/// session name is the first whitespace-separated token, possibly followed
/// by a `[...]` status annotation.
fn session_present(stdout: &[u8], name: &str) -> bool {
    let text = String::from_utf8_lossy(stdout);
    // Zellij colorizes `list-sessions` output. Strip ANSI CSI sequences up
    // front, then check the first whitespace-separated token on each line.
    let cleaned = strip_ansi(&text);
    for line in cleaned.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        if first == name {
            return true;
        }
    }
    false
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip until an ASCII letter terminator.
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{CommandOutput, RealExecutor, StubExecutor};
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Build a successful `ExitStatus` by running `true`.
    async fn ok_status() -> std::process::ExitStatus {
        RealExecutor.run("true", &[]).await.unwrap().status
    }

    /// Build a failing `ExitStatus` by running `false`.
    async fn fail_status() -> std::process::ExitStatus {
        RealExecutor.run("false", &[]).await.unwrap().status
    }

    fn output(status: std::process::ExitStatus, stdout: &[u8], stderr: &[u8]) -> CommandOutput {
        CommandOutput {
            status,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[tokio::test]
    async fn parse_version_handles_clean_output() {
        assert_eq!(parse_zellij_version("zellij 0.44.1\n").unwrap(), (0, 44, 1));
        assert_eq!(parse_zellij_version("zellij 0.45.0").unwrap(), (0, 45, 0));
        assert_eq!(parse_zellij_version("zellij 1.2.3-dev").unwrap(), (1, 2, 3));
    }

    #[tokio::test]
    async fn parse_version_errors_on_garbage() {
        assert!(parse_zellij_version("not a version").is_err());
    }

    #[tokio::test]
    async fn preflight_ok_for_supported_version() {
        let stub = StubExecutor::new();
        stub.queue_response(output(ok_status().await, b"zellij 0.44.1\n", b""));
        let mux = ZellijMux::with_executor(Box::new(stub));
        mux.preflight().await.expect("0.44.1 must pass preflight");
    }

    #[tokio::test]
    async fn preflight_ok_for_newer_version() {
        let stub = StubExecutor::new();
        stub.queue_response(output(ok_status().await, b"zellij 0.45.0\n", b""));
        let mux = ZellijMux::with_executor(Box::new(stub));
        mux.preflight().await.expect("0.45.0 must pass preflight");
    }

    #[tokio::test]
    async fn preflight_rejects_old_version() {
        let stub = StubExecutor::new();
        stub.queue_response(output(ok_status().await, b"zellij 0.43.9\n", b""));
        let mux = ZellijMux::with_executor(Box::new(stub));
        let err = mux.preflight().await.unwrap_err().to_string();
        assert!(err.contains("0.44.1 required"), "got: {err}");
        assert!(
            err.contains("brew") || err.contains("cargo install"),
            "actionable hint missing: {err}"
        );
    }

    #[tokio::test]
    async fn preflight_io_error_reports_install_hint() {
        // StubExecutor with no queued response returns io::Error.
        let stub = StubExecutor::new();
        let mux = ZellijMux::with_executor(Box::new(stub));
        let err = mux.preflight().await.unwrap_err().to_string();
        assert!(err.contains("not found"), "got: {err}");
        assert!(
            err.contains("brew install zellij"),
            "missing macOS hint: {err}"
        );
        assert!(err.contains("cargo install"), "missing cargo hint: {err}");
    }

    #[tokio::test]
    async fn preflight_nonzero_exit_is_error() {
        let stub = StubExecutor::new();
        stub.queue_response(output(
            fail_status().await,
            b"",
            b"some zellij bootstrap error\n",
        ));
        let mux = ZellijMux::with_executor(Box::new(stub));
        let err = mux.preflight().await.unwrap_err().to_string();
        assert!(err.contains("exited non-zero"), "got: {err}");
    }

    /// Build a `ZellijMux` whose executor delegates to a shared
    /// `Arc<StubExecutor>` so the test can both drive the mux and read back
    /// the recorded call sequence without resorting to raw pointers.
    fn mux_with_stub(in_zellij: bool) -> (ZellijMux, Arc<StubExecutor>) {
        let stub = Arc::new(StubExecutor::new());

        struct ArcExec(Arc<StubExecutor>);
        #[async_trait::async_trait]
        impl CommandExecutor for ArcExec {
            async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
                self.0.run(program, args).await
            }
        }

        let mux =
            ZellijMux::with_executor(Box::new(ArcExec(stub.clone()))).with_in_zellij(in_zellij);
        (mux, stub)
    }

    #[tokio::test]
    async fn ensure_session_outside_runs_list_sessions_only() {
        let (mux, stub) = mux_with_stub(false);
        // Empty stdout means "no existing sessions" → no collision warn.
        stub.queue_response(output(ok_status().await, b"", b""));
        mux.ensure_session("ark-build-demo").await.unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(calls[0].1, vec!["list-sessions".to_string()]);
    }

    #[tokio::test]
    async fn ensure_session_inside_switches_without_create_flag() {
        let (mux, stub) = mux_with_stub(true);
        stub.queue_response(output(ok_status().await, b"", b""));
        mux.ensure_session("ark-cavekit-auth").await.unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
            vec![
                "action".to_string(),
                "switch-session".to_string(),
                "ark-cavekit-auth".to_string()
            ]
        );
        // Guard against regressions that re-add the `--create` flag.
        assert!(
            !calls[0].1.iter().any(|a| a == "--create"),
            "switch-session must NOT pass --create (that flag is on attach only)"
        );
    }

    #[tokio::test]
    async fn ensure_session_outside_warns_on_collision_but_ok() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(
            ok_status().await,
            b"ark-build-demo [Created N seconds ago]\nother-sess [Created 1m ago]\n",
            b"",
        ));
        // Collision → still returns Ok (warn-log only).
        mux.ensure_session("ark-build-demo").await.unwrap();
    }

    #[tokio::test]
    async fn create_tab_first_outside_uses_setsid_and_layout() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(ok_status().await, b"", b""));

        let handle = mux
            .create_tab("ark-build-demo", "builder", Path::new("/tmp/x.kdl"))
            .await
            .unwrap();
        assert_eq!(handle.tab_index, 0);
        assert_eq!(handle.session, "ark-build-demo");
        assert_eq!(handle.name, "builder");

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "setsid");
        assert_eq!(
            calls[0].1,
            vec![
                "zellij".to_string(),
                "-s".to_string(),
                "ark-build-demo".to_string(),
                "--layout".to_string(),
                "/tmp/x.kdl".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn create_tab_first_inside_uses_switch_session_with_layout() {
        let (mux, stub) = mux_with_stub(true);
        stub.queue_response(output(ok_status().await, b"", b""));

        let handle = mux
            .create_tab("ark-build-demo", "builder", Path::new("/tmp/x.kdl"))
            .await
            .unwrap();
        assert_eq!(handle.tab_index, 0);

        let calls = stub.recorded_calls();
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
            vec![
                "action".to_string(),
                "switch-session".to_string(),
                "ark-build-demo".to_string(),
                "--layout".to_string(),
                "/tmp/x.kdl".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn create_tab_additional_increments_index_and_uses_new_tab() {
        let (mux, stub) = mux_with_stub(false);
        // First tab: session spawn.
        stub.queue_response(output(ok_status().await, b"", b""));
        // Second tab: action new-tab.
        stub.queue_response(output(ok_status().await, b"", b""));
        // Third tab: action new-tab.
        stub.queue_response(output(ok_status().await, b"", b""));

        let layout = PathBuf::from("/tmp/x.kdl");
        let t0 = mux.create_tab("s", "builder", &layout).await.unwrap();
        let t1 = mux.create_tab("s", "review", &layout).await.unwrap();
        let t2 = mux.create_tab("s", "log", &layout).await.unwrap();

        assert_eq!(t0.tab_index, 0);
        assert_eq!(t1.tab_index, 1);
        assert_eq!(t2.tab_index, 2);

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 3);
        // Second call is the first `action new-tab`.
        assert_eq!(calls[1].0, "zellij");
        assert_eq!(
            calls[1].1,
            vec![
                "--session".to_string(),
                "s".to_string(),
                "action".to_string(),
                "new-tab".to_string(),
                "--layout".to_string(),
                "/tmp/x.kdl".to_string(),
                "--name".to_string(),
                "review".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn create_tab_reports_zellij_failure() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(fail_status().await, b"", b"layout not found\n"));
        let err = mux
            .create_tab("s", "builder", Path::new("/tmp/x.kdl"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("layout not found"), "got: {err}");
    }

    #[tokio::test]
    async fn close_tab_is_idempotent_on_failure() {
        let (mux, stub) = mux_with_stub(false);
        // Simulate zellij reporting the tab doesn't exist.
        stub.queue_response(output(fail_status().await, b"", b"no such tab\n"));

        let handle = TabHandle::new("s", 4, "builder");
        mux.close_tab(&handle)
            .await
            .expect("close_tab must succeed even on zellij failure");

        let calls = stub.recorded_calls();
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
            vec![
                "--session".to_string(),
                "s".to_string(),
                "action".to_string(),
                "close-tab-at-index".to_string(),
                "4".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn close_tab_swallows_executor_io_error() {
        // No queued response → StubExecutor returns io::Error.
        let (mux, _stub) = mux_with_stub(false);
        let handle = TabHandle::new("s", 0, "t");
        mux.close_tab(&handle)
            .await
            .expect("idempotent on IO error");
    }

    #[tokio::test]
    async fn rename_tab_invokes_expected_command() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(ok_status().await, b"", b""));
        let handle = TabHandle::new("s", 2, "builder");
        mux.rename_tab(&handle, "builder 5/8").await.unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
            vec![
                "--session".to_string(),
                "s".to_string(),
                "action".to_string(),
                "rename-tab".to_string(),
                "--tab-index".to_string(),
                "2".to_string(),
                "--name".to_string(),
                "builder 5/8".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn rename_tab_bubbles_zellij_failure() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(fail_status().await, b"", b"no tab\n"));
        let handle = TabHandle::new("s", 0, "t");
        let err = mux.rename_tab(&handle, "x").await.unwrap_err().to_string();
        assert!(err.contains("no tab"), "got: {err}");
    }

    #[tokio::test]
    async fn pipe_invokes_zellij_pipe_fire_and_forget() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(ok_status().await, b"", b""));
        mux.pipe(PIPE_TARGET_STATUS, "{\"k\":\"v\"}").await.unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
            vec![
                "pipe".to_string(),
                "--name".to_string(),
                "ark-status".to_string(),
                "--".to_string(),
                "{\"k\":\"v\"}".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn pipe_nonzero_exit_is_non_fatal() {
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(fail_status().await, b"", b"no such target\n"));
        mux.pipe(PIPE_TARGET_PICKER, "{}")
            .await
            .expect("pipe must not bubble errors");
    }

    #[tokio::test]
    async fn pipe_executor_io_error_is_non_fatal() {
        let (mux, _stub) = mux_with_stub(false);
        // No queued response → IO error.
        mux.pipe(PIPE_TARGET_STATUS, "{}").await.unwrap();
    }

    #[tokio::test]
    async fn kind_is_zellij() {
        let mux = ZellijMux::with_executor(Box::new(StubExecutor::new()));
        assert_eq!(mux.kind(), "zellij");
    }

    #[tokio::test]
    async fn session_present_parses_colorized_output() {
        // Zellij colorizes list-sessions. Make sure our parser finds the
        // session name even with ANSI escapes wrapping it.
        let out = b"\x1b[32mark-build-demo\x1b[0m [Created 3s ago]\n\x1b[32mother\x1b[0m [Created 5m ago]\n";
        assert!(session_present(out, "ark-build-demo"));
        assert!(session_present(out, "other"));
        assert!(!session_present(out, "missing"));
    }

    #[tokio::test]
    async fn ensure_session_outside_tolerates_list_sessions_failure() {
        // `zellij list-sessions` exits non-zero when zero sessions exist.
        // ensure_session must treat that as "no collision" and still succeed.
        let (mux, stub) = mux_with_stub(false);
        stub.queue_response(output(fail_status().await, b"", b"no active sessions\n"));
        mux.ensure_session("s").await.unwrap();
    }
}
