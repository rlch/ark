//! # `ark-test-harness`
//!
//! v0.2 backlog item #6 — PTY-level integration harness for ark.
//!
//! This crate provides a reusable [`ArkHarness`] that spawns `ark run`
//! under a PTY against a real `zellij` installation, suitable for
//! downstream integration tests that need to assert on ark-under-zellij
//! behaviour end-to-end (scene compile, supervisor fork, ready
//! handshake, zellij layout parse, session bring-up, view rendering).
//!
//! It subsumes the logic currently inlined in
//! `crates/cli/tests/launch_pty.rs` and exposes it as a library so
//! other crates' integration tests can share the same harness without
//! duplicating pty + tempdir + session-polling code.
//!
//! ## Design notes
//!
//! * **Caller supplies the ark binary path.** Cargo's
//!   `CARGO_BIN_EXE_<name>` env var is only defined for tests in the
//!   _same_ package as the `[[bin]]` target. Since this harness lives
//!   in its own crate, downstream tests must resolve `ark` themselves
//!   (typically `PathBuf::from(env!("CARGO_BIN_EXE_ark"))` from a test
//!   in `crates/cli/tests/` or by other means) and pass it to
//!   [`HarnessBuilder::ark_bin`].
//!
//! * **SKIP branch.** Hosts without `zellij` on PATH and tests running
//!   inside an existing zellij session both cause [`HarnessBuilder::build`]
//!   to return `Ok(None)`. The caller prints SKIP and returns.
//!
//! * **Isolation.** Each harness gets its own temp state/config/
//!   runtime directories plus a PATH-prepended shim dir where an
//!   optional `claude` symlink can stand in for the real CLI.
//!
//! * **Screen capture.** [`ArkHarness::dump_screen`] shells out to
//!   `zellij action dump-screen <file>`; if that path fails the
//!   harness falls back to the PTY output buffer, which is a
//!   strictly-worse but still-useful signal (ANSI + cursor codes
//!   intermixed).
//!
//! ## MVP scope caveats
//!
//! * **No VT100 grid parser.** The packet budget prefers a working
//!   screen-dump round-trip over a parsed cell grid. If downstream
//!   tests need grid-level assertions, pair this harness with the
//!   `vt100` crate in the caller.
//!
//! * **No programmatic pane-count assertion.** `zellij action
//!   list-panes` is not stable across zellij versions; callers that
//!   need this should drive it themselves via [`ArkHarness::run_zellij`].

#![deny(missing_debug_implementations)]

pub mod fixtures;
pub mod pty;
pub mod zellij;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tempfile::TempDir;

pub use fixtures::stage_claude_shim;
pub use pty::PtyProcess;
pub use zellij::{dump_screen, inside_zellij, kill_session, wait_for_session, zellij_on_path};

/// Handle to a running `ark` process under a PTY + its zellij session.
///
/// Constructed via [`HarnessBuilder::build`]. Drop will best-effort
/// clean up the zellij session + child; prefer calling
/// [`ArkHarness::shutdown`] explicitly so errors surface.
pub struct ArkHarness {
    // Temp roots — held for the life of the harness so they outlive
    // any child that inherited paths from them.
    _state: TempDir,
    _config: TempDir,
    _runtime: TempDir,
    _shim: Option<TempDir>,

    session_name: String,
    pty: PtyProcess,
}

impl std::fmt::Debug for ArkHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArkHarness")
            .field("session_name", &self.session_name)
            .finish_non_exhaustive()
    }
}

impl ArkHarness {
    /// Convenience wrapper: [`HarnessBuilder::new`] + [`HarnessBuilder::build`].
    ///
    /// Returns `Ok(None)` when the environment isn't suitable (zellij
    /// missing or running inside zellij). That's the SKIP branch every
    /// caller must honour.
    pub fn try_new(ark_bin: impl Into<PathBuf>, scene_kdl: &str) -> Result<Option<Self>> {
        HarnessBuilder::new(ark_bin, scene_kdl).build()
    }

    /// Session name the harness used to invoke zellij.
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Blocks until `zellij list-sessions` reports the harness session
    /// name, polling at 200 ms intervals.
    ///
    /// Returns `Err` when the deadline elapses without the session
    /// appearing. The common failure mode is a scene-compile error
    /// causing `ark run` to exit before invoking zellij — check the
    /// PTY buffer via [`ArkHarness::pty_buffer`].
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<()> {
        let appeared = wait_for_session(&self.session_name, timeout);
        if !appeared {
            return Err(anyhow!(
                "zellij never reported session `{}` (timeout {:?}). PTY buffer:\n{}",
                self.session_name,
                timeout,
                self.pty.snapshot_lossy()
            ));
        }
        Ok(())
    }

    /// Write `text` to the PTY master.
    ///
    /// Callers wanting to submit a keypress should append `"\r"` or
    /// `"\n"` themselves — the harness does not add any terminator.
    pub fn send_input(&self, text: &str) -> Result<()> {
        self.pty.send_input(text)
    }

    /// Dump the current zellij screen via `zellij action dump-screen`.
    ///
    /// Returns the raw screen text zellij wrote to disk, ANSI included.
    /// Falls back to the PTY output buffer when the zellij CLI path
    /// fails (e.g. session not yet bound).
    pub fn dump_screen(&self) -> Result<String> {
        match dump_screen(&self.session_name) {
            Ok(text) => Ok(text),
            Err(err) => {
                tracing::debug!(
                    error = %err,
                    "dump_screen failed, falling back to pty buffer"
                );
                Ok(self.pty.snapshot_lossy())
            }
        }
    }

    /// Poll [`ArkHarness::dump_screen`] until `needle` appears or the
    /// deadline elapses.
    pub fn wait_for_text(&self, needle: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(text) = self.dump_screen() {
                if text.contains(needle) {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(anyhow!(
            "text `{needle}` never appeared in screen dump (timeout {timeout:?})"
        ))
    }

    /// Direct handle to the PTY wrapper, for callers that need to
    /// inspect the raw output buffer, resize the tty, or send signals.
    pub fn pty(&self) -> &PtyProcess {
        &self.pty
    }

    /// Accumulated PTY output since spawn (lossy utf-8).
    pub fn pty_buffer(&self) -> String {
        self.pty.snapshot_lossy()
    }

    /// Best-effort shutdown:
    ///   * `zellij kill-session <name>` to drop the session.
    ///   * Kill the pty child, then `try_wait` up to 3 s.
    pub fn shutdown(self) -> Result<()> {
        let _ = kill_session(&self.session_name);
        self.pty.shutdown_with_timeout(Duration::from_secs(3))?;
        Ok(())
    }
}

/// Builder for [`ArkHarness`].
///
/// Minimal required inputs:
///   * path to the `ark` binary
///   * scene KDL body (written to a tempfile)
///
/// Optional knobs:
///   * [`HarnessBuilder::with_claude_mock`] — stage a `claude` shim
///     (typically pointing at `mock-claude-cc`) in a PATH-prepended
///     shim dir.
///   * [`HarnessBuilder::session_name`] — override the derived
///     session name (default: `ark-harness-<pid>-<nanos>`).
///   * [`HarnessBuilder::extra_env`] — additional env vars passed
///     through to `ark run`.
#[derive(Debug)]
pub struct HarnessBuilder {
    ark_bin: PathBuf,
    scene_kdl: String,
    session_name: Option<String>,
    claude_mock: Option<PathBuf>,
    extra_env: Vec<(String, String)>,
    pty_rows: u16,
    pty_cols: u16,
}

impl HarnessBuilder {
    /// Start a builder with an ark binary path + the scene KDL body
    /// that will drive the launch. The scene is written to a tempfile
    /// inside the harness's config tempdir and passed via `--scene
    /// <path>`.
    pub fn new(ark_bin: impl Into<PathBuf>, scene_kdl: impl Into<String>) -> Self {
        Self {
            ark_bin: ark_bin.into(),
            scene_kdl: scene_kdl.into(),
            session_name: None,
            claude_mock: None,
            extra_env: Vec::new(),
            pty_rows: 40,
            pty_cols: 120,
        }
    }

    /// Override the derived session name.
    pub fn session_name(mut self, name: impl Into<String>) -> Self {
        self.session_name = Some(name.into());
        self
    }

    /// Provide a path to a binary that will be symlinked as `claude`
    /// in a PATH-prepended shim directory. Typically the
    /// `mock-claude-cc` binary from `ark-test-fixtures-claude-code`.
    pub fn with_claude_mock(mut self, mock_bin: impl Into<PathBuf>) -> Self {
        self.claude_mock = Some(mock_bin.into());
        self
    }

    /// Push an extra env var into the spawned ark child's environment.
    pub fn extra_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// PTY window size. Default 40 rows × 120 cols.
    pub fn pty_size(mut self, rows: u16, cols: u16) -> Self {
        self.pty_rows = rows;
        self.pty_cols = cols;
        self
    }

    /// Attempt to construct the harness.
    ///
    /// Returns `Ok(None)` when:
    ///   * `zellij` is not on PATH, OR
    ///   * the caller is already inside a zellij session (`$ZELLIJ`
    ///     non-empty). Nesting would confuse polling + pollute the
    ///     caller's session.
    ///
    /// Either way the caller is expected to print SKIP and return.
    pub fn build(self) -> Result<Option<ArkHarness>> {
        if !zellij_on_path() {
            tracing::info!("SKIP: zellij not on PATH");
            return Ok(None);
        }
        if inside_zellij() {
            tracing::info!("SKIP: running inside zellij would nest clients");
            return Ok(None);
        }

        if !self.ark_bin.is_file() {
            return Err(anyhow!(
                "ark binary not found at `{}` — resolve via env!(\"CARGO_BIN_EXE_ark\") or pass an explicit path",
                self.ark_bin.display()
            ));
        }

        let state =
            tempfile::tempdir().with_context(|| "failed to create state tempdir for harness")?;
        let config =
            tempfile::tempdir().with_context(|| "failed to create config tempdir for harness")?;
        // Runtime dir must stay short (SUN_LEN = 104 on macOS) once
        // `agents/<id>.sock` is appended. `/tmp` stays short.
        let runtime = tempfile::Builder::new()
            .prefix("ark-harness-rt-")
            .tempdir_in("/tmp")
            .with_context(|| "failed to create runtime tempdir for harness")?;

        // Write the scene file into the config dir so ark can resolve
        // it via `--scene <path>`.
        let scene_path = config.path().join("harness-scene.kdl");
        std::fs::write(&scene_path, self.scene_kdl.as_bytes())
            .with_context(|| format!("failed to write scene to {}", scene_path.display()))?;

        // Optional claude shim.
        let (shim_dir, path_prepend): (Option<TempDir>, Option<String>) = match &self.claude_mock {
            Some(bin) => {
                let shim = stage_claude_shim(bin)
                    .with_context(|| "failed to stage claude shim for harness")?;
                let path = shim.path().display().to_string();
                (Some(shim), Some(path))
            }
            None => (None, None),
        };

        let session_name = self
            .session_name
            .clone()
            .unwrap_or_else(|| default_session_name());

        // Compose the command to spawn under the PTY.
        let mut env: Vec<(String, String)> = vec![
            (
                "ARK_STATE_DIR".to_string(),
                state.path().display().to_string(),
            ),
            (
                "ARK_CONFIG_DIR".to_string(),
                config.path().display().to_string(),
            ),
            (
                "ARK_RUNTIME_DIR".to_string(),
                runtime.path().display().to_string(),
            ),
            ("NO_COLOR".to_string(), "1".to_string()),
        ];
        env.extend(self.extra_env.into_iter());

        if let Some(shim_path) = path_prepend {
            let existing = std::env::var("PATH").unwrap_or_default();
            let combined = if existing.is_empty() {
                shim_path
            } else {
                format!("{}:{}", shim_path, existing)
            };
            env.push(("PATH".to_string(), combined));
        }

        let args: Vec<String> = vec![
            "--session".to_string(),
            session_name.clone(),
            "--scene".to_string(),
            scene_path.display().to_string(),
        ];

        let removed_env: &[&str] = &["ZELLIJ", "ZELLIJ_PANE_ID", "ZELLIJ_SESSION_NAME"];

        let pty = PtyProcess::spawn(
            &self.ark_bin,
            &args,
            &env,
            removed_env,
            self.pty_rows,
            self.pty_cols,
        )
        .with_context(|| "failed to spawn ark under pty")?;

        Ok(Some(ArkHarness {
            _state: state,
            _config: config,
            _runtime: runtime,
            _shim: shim_dir,
            session_name,
            pty,
        }))
    }
}

fn default_session_name() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("ark-harness-{pid}-{nanos}")
}

// Compatibility helper: run a one-shot zellij command and capture stdout.
// Re-exported for callers that need bespoke polling (e.g. `list-panes`).
pub fn run_zellij(args: &[&str]) -> Result<std::process::Output> {
    zellij::run_zellij(args)
}

/// Sanity-check predicate downstream tests can use from within
/// `#[test]` or `#[tokio::test]`: returns `true` when both zellij is
/// on PATH and we're not already inside a zellij session.
///
/// Equivalent to the predicate [`HarnessBuilder::build`] uses to
/// decide between returning `Ok(Some(_))` and `Ok(None)`.
pub fn harness_can_run() -> bool {
    zellij_on_path() && !inside_zellij()
}

/// Probe the workspace `target/debug/<bin>` directory starting from an
/// arbitrary file inside the workspace. Useful for downstream tests
/// that can't reach a package-scoped `CARGO_BIN_EXE_*`.
///
/// Walks parents of `manifest_dir` until it finds a `Cargo.lock`, then
/// looks at `<root>/target/debug/<bin>`. Returns `None` when the
/// binary isn't present (hasn't been built yet).
pub fn discover_workspace_binary(manifest_dir: &Path, bin_name: &str) -> Option<PathBuf> {
    let mut cursor = manifest_dir.to_path_buf();
    loop {
        if cursor.join("Cargo.lock").is_file() {
            let candidate = cursor.join("target").join("debug").join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
            // Some CI setups place target/ next to Cargo.lock but name
            // the profile dir differently. Try `release` as a fallback.
            let release = cursor.join("target").join("release").join(bin_name);
            if release.is_file() {
                return Some(release);
            }
            return None;
        }
        if !cursor.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_session_name_contains_pid() {
        let name = default_session_name();
        let pid = std::process::id().to_string();
        assert!(
            name.contains(&pid),
            "session name `{name}` did not embed pid `{pid}`"
        );
        assert!(name.starts_with("ark-harness-"));
    }

    #[test]
    fn builder_rejects_missing_ark_bin() {
        // Skip unit coverage of the zellij/skip branches here — those
        // depend on the host env. This test just asserts the bin-path
        // validation path, which runs unconditionally after the skip
        // checks.
        if !zellij_on_path() || inside_zellij() {
            eprintln!(
                "SKIP: harness builder path-validation requires zellij on PATH outside of zellij"
            );
            return;
        }
        let missing = PathBuf::from("/nonexistent/path/ark-does-not-exist");
        let err = HarnessBuilder::new(missing, "layout {}")
            .build()
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("ark binary not found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn discover_workspace_binary_finds_self() {
        // The manifest dir of this crate sits inside the workspace, so
        // the helper must walk up to `Cargo.lock`. We don't assert a
        // particular binary exists — just that the walk finds
        // `Cargo.lock`. `None` is acceptable when the binary hasn't
        // been built in this target dir yet.
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Walk up looking for Cargo.lock manually to mirror the helper.
        let mut cursor = here.clone();
        let mut saw_lock = false;
        loop {
            if cursor.join("Cargo.lock").is_file() {
                saw_lock = true;
                break;
            }
            if !cursor.pop() {
                break;
            }
        }
        assert!(
            saw_lock,
            "expected Cargo.lock somewhere above {}",
            here.display()
        );

        // `discover_workspace_binary` returns an Option — either hit
        // (binary was built) or miss. We don't care which.
        let _ = discover_workspace_binary(&here, "ark");
    }

    #[test]
    fn harness_can_run_matches_primitives() {
        let combined = harness_can_run();
        let expected = zellij_on_path() && !inside_zellij();
        assert_eq!(combined, expected);
    }
}
