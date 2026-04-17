//! `ZellijMux` — ark's concrete integration with the zellij CLI.
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
//! `ensure_session(&self, name: &str) -> Result<()>` has no return channel
//! for a mutated name. For v1 we therefore:
//!
//! 1. Run `zellij list-sessions`.
//! 2. If `name` is present in the output, emit a `warn!` log and proceed.
//! 3. Rely on zellij's own rejection for truly hard collisions at
//!    `create_tab` time.
//!
//! A proper short-ULID rename would require a signature change; that is
//! tracked as a TODO.
//!
//! # Test seam — `ZellijMux::for_test`
//!
//! Downstream crates that test code calling `ZellijMux` methods should enable
//! the `test-support` cargo feature in their `[dev-dependencies]` entry for
//! `ark-mux-zellij` and construct the mux via
//! [`ZellijMux::for_test`](Self::for_test). The returned mux is backed by a
//! [`StubExecutor`] that replays a scripted sequence of canned
//! [`CommandOutput`](crate::executor::CommandOutput) responses and records
//! every call for later argv assertion. See the constructor docs for an
//! example.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::anyhow;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::executor::{CommandExecutor, RealExecutor};

/// Stable handle for a multiplexer tab.
///
/// Lives in the mux crate because the multiplex concern owns tab
/// addressing (see cavekit-soul-phase-1-types.md T-008 — `ark-types`
/// must not carry mux-shaped state). Carries the multiplexer session
/// name, a 0-based tab index, and a human-friendly label used for
/// tracing / tests.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TabHandle {
    /// Multiplexer session name (e.g. `"ark-cavekit-auth-..."`).
    pub session: String,
    /// 0-based tab index inside the session.
    pub tab_index: u32,
    /// Human-friendly tab label (e.g. `"builder"`).
    pub name: String,
}

impl TabHandle {
    /// Construct a new `TabHandle`.
    pub fn new(session: impl Into<String>, tab_index: u32, name: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            tab_index,
            name: name.into(),
        }
    }
}

impl fmt::Display for TabHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}({})", self.session, self.tab_index, self.name)
    }
}

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
    /// `ark doctor` and bare-ark launch.
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

/// Core method surface. See cavekit-mux-zellij.md for the contract each
/// method fulfils.
impl ZellijMux {
    pub fn kind(&self) -> &'static str {
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
    ///   with `--layout` (R2) because `ensure_session` takes no layout
    ///   argument.
    pub async fn ensure_session(&self, name: &str) -> anyhow::Result<()> {
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
    pub async fn create_tab(
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
                // F-731: outside-zellij first-tab spawn. The earlier
                // `setsid zellij ...` invocation was double-broken:
                // (1) macOS doesn't ship `setsid(1)` so the call failed
                // with "no such file or directory" before zellij even
                // exec'd, and (2) even on Linux, `setsid + null stdio`
                // strips the controlling TTY that zellij's TUI client
                // requires to boot — same pattern F-730 fixed in
                // `crates/cli/src/commands/spawn.rs`.
                //
                // The fix is the shared pty helper from
                // [`crate::pty::spawn_zellij_with_pty`]: allocate a
                // pty pair, spawn zellij with the slave as its
                // controlling TTY, run the 500 ms startup grace poll,
                // then drop the pair so the master closes and the
                // server daemon (already forked) lives on
                // independently.
                //
                // Note: we deliberately do NOT route this through
                // `self.executor`. The executor trait is
                // `Output`-style (captures stdout/stderr); pty spawn
                // is structurally different. The call site here is
                // the only outside-zellij first-tab spawn in the
                // codebase, so a one-off direct call is simpler than
                // growing the trait.
                //
                // Cwd is the layout's parent dir as a placeholder —
                // the real per-agent cwd is baked into the rendered
                // layout. v1 layouts ignore the client's cwd.
                let cwd = layout_path.parent().unwrap_or(layout_path);
                let mut handle = crate::pty::spawn_zellij_with_pty(session, layout_path, cwd)
                    .map_err(|e| anyhow!("failed to spawn zellij in pty: {e}"))?;
                if let Err(e) = crate::pty::pty_child_startup_failure(handle.child.as_mut()) {
                    return Err(anyhow!("zellij failed to start for session {session}: {e}",));
                }
                // Drop the handle here — the pty pair's lifetime ends
                // and the master fd closes, sending SIGHUP to the
                // zellij client. The server daemon has already
                // forked at this point (the 500 ms grace covered
                // that), so the SIGHUP only kills the now-redundant
                // client; the session keeps running.
                drop(handle);
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
    pub async fn close_tab(&self, handle: &TabHandle) -> anyhow::Result<()> {
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
    pub async fn rename_tab(&self, handle: &TabHandle, name: &str) -> anyhow::Result<()> {
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
    pub async fn pipe(&self, target_name: &str, payload: &str) -> anyhow::Result<()> {
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

// --------------------------------------------------------------------------
// Test-support surface (`test-support` cargo feature or `cfg(test)`).
//
// This is the canonical downstream test seam: consumers add
// `ark-mux-zellij = { path = "...", features = ["test-support"] }` to their
// `[dev-dependencies]` and call `ZellijMux::for_test(scripted)` to build a
// mux backed by a [`StubExecutor`] seeded with a scripted response queue.
// Tests then call the mux's public methods and, if needed, introspect the
// captured argv via the returned `Arc<StubExecutor>` handle.
// --------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
mod test_support {
    use crate::executor::{CommandExecutor, CommandOutput, StubExecutor};
    use std::sync::Arc;

    /// Adapter that lets the mux delegate to a shared `Arc<StubExecutor>`
    /// so callers can both drive the mux and read back recorded calls.
    pub(super) struct ArcStubExecutor(pub Arc<StubExecutor>);

    #[async_trait::async_trait]
    impl CommandExecutor for ArcStubExecutor {
        async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            self.0.run(program, args).await
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ZellijMux {
    /// Build a `ZellijMux` backed by a [`StubExecutor`] seeded with
    /// `scripted` command outputs. Returned alongside the `Arc<StubExecutor>`
    /// so callers can inspect `recorded_calls()` after driving the mux.
    ///
    /// Responses are served in FIFO order; any additional call once the
    /// queue is drained will return an `io::Error` (exactly the existing
    /// `StubExecutor` contract). This is intentional — it surfaces "test
    /// made more calls than it scripted" as a loud failure instead of a
    /// silent fallthrough.
    ///
    /// Pass an empty `Vec` when the scenario is expected to not route
    /// through the executor at all (e.g. the outside-zellij first-tab pty
    /// path) — the returned recorder will stay empty and the test can
    /// assert that.
    ///
    /// ```no_run
    /// # use ark_mux_zellij::{ZellijMux, executor::{CommandOutput, StubExecutor}};
    /// # use std::sync::Arc;
    /// # async fn demo() {
    /// let (mux, stub): (ZellijMux, Arc<StubExecutor>) = ZellijMux::for_test(Vec::new());
    /// // ... drive mux, then inspect stub.recorded_calls() ...
    /// # let _ = (mux, stub);
    /// # }
    /// ```
    pub fn for_test(
        scripted: Vec<crate::executor::CommandOutput>,
    ) -> (Self, std::sync::Arc<crate::executor::StubExecutor>) {
        let stub = std::sync::Arc::new(crate::executor::StubExecutor::new());
        for output in scripted {
            stub.queue_response(output);
        }
        let mux = Self::with_executor(Box::new(test_support::ArcStubExecutor(stub.clone())))
            .with_in_zellij(false);
        (mux, stub)
    }

    /// Variant of [`Self::for_test`] that forces the `in_zellij` flag. Use
    /// when the scenario under test depends on the inside-zellij branch
    /// (e.g. `ensure_session` issuing `switch-session`).
    pub fn for_test_in_zellij(
        scripted: Vec<crate::executor::CommandOutput>,
    ) -> (Self, std::sync::Arc<crate::executor::StubExecutor>) {
        let stub = std::sync::Arc::new(crate::executor::StubExecutor::new());
        for output in scripted {
            stub.queue_response(output);
        }
        let mux = Self::with_executor(Box::new(test_support::ArcStubExecutor(stub.clone())))
            .with_in_zellij(true);
        (mux, stub)
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

    // F-731: the outside-zellij first-tab path used to shell out to
    // the external `setsid(1)` binary. macOS doesn't ship it, and even
    // on Linux the null-stdio + setsid combination strips zellij's
    // controlling TTY (the TUI client refuses to boot). The path now
    // calls `crate::pty::spawn_zellij_with_pty` directly — bypassing
    // the `executor` trait — so the prior argv-shape test is no longer
    // observable through the stub. The pty helper itself is covered
    // by `crate::pty::tests::*`. We keep this test name and assert the
    // OBSERVABLE consequence at the mux level: the executor records
    // ZERO calls on the outside-zellij first-tab path (because the
    // pty spawn does not route through `self.executor`), and the
    // returned `TabHandle` has the expected shape.
    //
    // The actual zellij spawn is gated on a real zellij binary being
    // present, so we cannot exercise it from a unit test without a
    // big infrastructure dependency. The W-8 e2e scenario provides
    // the integration coverage.
    #[tokio::test]
    async fn create_tab_first_outside_does_not_route_through_executor() {
        let (mux, stub) = mux_with_stub(false);
        // No queued executor response. The pty path may succeed (real
        // zellij on PATH) or fail (CI without zellij) — either is OK
        // for this test. The assertion is on the ROUTING: the
        // outside-zellij first-tab spawn must not call
        // `self.executor.run` because that path was the F-731 bug.
        // The pty helper bypasses the executor entirely.
        let _ = mux
            .create_tab("ark-build-demo", "builder", Path::new("/tmp/x.kdl"))
            .await;
        let calls = stub.recorded_calls();
        assert!(
            calls.is_empty(),
            "outside-zellij first-tab spawn must not route through self.executor: got calls {calls:?}"
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
        // F-731: first tab uses the pty path and does NOT consume a
        // queued executor response. Subsequent tabs use `action
        // new-tab` and DO consume responses.
        stub.queue_response(output(ok_status().await, b"", b""));
        stub.queue_response(output(ok_status().await, b"", b""));

        let layout = PathBuf::from("/tmp/x.kdl");
        let t0 = mux.create_tab("s", "builder", &layout).await.unwrap();
        let t1 = mux.create_tab("s", "review", &layout).await.unwrap();
        let t2 = mux.create_tab("s", "log", &layout).await.unwrap();

        assert_eq!(t0.tab_index, 0);
        assert_eq!(t1.tab_index, 1);
        assert_eq!(t2.tab_index, 2);

        let calls = stub.recorded_calls();
        assert_eq!(
            calls.len(),
            2,
            "first tab is pty (no executor); only the two `action new-tab` calls go through executor"
        );
        // First executor call is the first `action new-tab` (for `t1`).
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(
            calls[0].1,
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
    async fn create_tab_additional_reports_zellij_failure() {
        // F-731: this test was originally `create_tab_reports_zellij_failure`
        // and exercised first-tab spawn failure via the executor stub.
        // First-tab spawn now uses the pty path which doesn't go
        // through the executor. We test the SECOND tab (action new-tab)
        // failure-propagation instead — same code path, different
        // surface. First-tab pty failure is covered by W-8 e2e.
        let (mux, stub) = mux_with_stub(false);
        // First tab: uses pty (no queued response needed).
        let layout = PathBuf::from("/tmp/x.kdl");
        let _ = mux.create_tab("s", "builder", &layout).await;
        // Second tab: queue a failing response so `action new-tab`
        // bubbles the stderr text up.
        stub.queue_response(output(fail_status().await, b"", b"new-tab failed\n"));
        let err = mux
            .create_tab("s", "review", &layout)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("new-tab failed"), "got: {err}");
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

    // ------- T-122: additional argv shape guards -------

    /// Inside-zellij first-tab spawn also uses `switch-session` (plus
    /// `--layout`) and must NOT carry `--create`. That flag exists on
    /// `attach` only; smuggling it onto `switch-session` is a known
    /// regression (cavekit-mux-zellij.md R1 / Q5).
    #[tokio::test]
    async fn create_tab_inside_first_does_not_include_create_flag() {
        let (mux, stub) = mux_with_stub(true);
        stub.queue_response(output(ok_status().await, b"", b""));
        mux.create_tab("sess", "builder", Path::new("/tmp/x.kdl"))
            .await
            .unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.iter().any(|a| a == "switch-session"),
            "expected switch-session verb"
        );
        assert!(
            !calls[0].1.iter().any(|a| a == "--create"),
            "switch-session must NOT pass --create; argv: {:?}",
            calls[0].1
        );
    }

    /// F-731: the outside-zellij first-tab argv shape (`zellij -s <s>
    /// --layout <p>`) is now built by `crate::pty::spawn_zellij_with_pty`
    /// inside `portable_pty::CommandBuilder`. The argv is no longer
    /// observable through `self.executor`. The shape is implicitly
    /// guarded by the W-8 e2e test scenario which requires zellij to
    /// start a real session against a real pty; if the argv drifts,
    /// zellij refuses to boot and the e2e fails.
    ///
    /// We retain a guard for the inverse: the outside-zellij first-tab
    /// path MUST NOT route through `self.executor`. Routing through it
    /// would re-introduce the F-731 null-stdio failure mode.
    #[tokio::test]
    async fn create_tab_new_session_does_not_route_through_executor_outside_zellij() {
        let (mux, stub) = mux_with_stub(false);
        // No queued response — accidental executor.run() would panic
        // for lack of a canned reply, OR the call would Err from the
        // pty path (no zellij in test env). Either way: zero stub
        // calls.
        let _ = mux
            .create_tab("my-sess", "builder", Path::new("/tmp/layout.kdl"))
            .await;
        let calls = stub.recorded_calls();
        assert!(
            calls.is_empty(),
            "outside-zellij first-tab spawn must not route through self.executor: got calls {calls:?}"
        );
    }

    /// Guard on the `action new-tab` argv shape: must match the exact
    /// verb ordering `action new-tab --layout <path> --name <tab>`
    /// under a `--session <name>` prefix.
    #[tokio::test]
    async fn create_tab_additional_uses_action_new_tab_verb() {
        let (mux, stub) = mux_with_stub(false);
        // F-731: first tab uses pty (no executor). Second tab uses
        // executor for `action new-tab`. Queue ONE response for the
        // second tab.
        stub.queue_response(output(ok_status().await, b"", b""));
        let layout = PathBuf::from("/tmp/layout.kdl");
        let _ = mux.create_tab("ss", "one", &layout).await;
        mux.create_tab("ss", "two", &layout).await.unwrap();

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        let a = &calls[0].1;
        let act_pos = a
            .iter()
            .position(|t| t == "action")
            .expect("missing action verb");
        assert_eq!(
            a.get(act_pos + 1).map(String::as_str),
            Some("new-tab"),
            "action must be followed by new-tab"
        );
        let l_pos = a.iter().position(|t| t == "--layout").unwrap();
        assert_eq!(
            a.get(l_pos + 1).map(String::as_str),
            Some("/tmp/layout.kdl")
        );
        let n_pos = a.iter().position(|t| t == "--name").unwrap();
        assert_eq!(a.get(n_pos + 1).map(String::as_str), Some("two"));
        let sess_pos = a.iter().position(|t| t == "--session").unwrap();
        assert!(sess_pos < act_pos, "--session must precede action verb");
        assert_eq!(a.get(sess_pos + 1).map(String::as_str), Some("ss"));
    }
}
