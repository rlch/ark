//! Zellij CLI helpers used by the harness.
//!
//! Every helper shells out to the real `zellij` binary — no FFI, no
//! embedded server. Matches the surface already exercised by
//! `crates/cli/tests/launch_pty.rs`, just lifted into a shared crate.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

/// Is a working `zellij` binary on `PATH`?
///
/// Runs `zellij --version` and treats a zero exit status as "yes".
/// Matches the CLI test's approach over stdlib PATH walking so a
/// broken-but-present zellij still counts as absent.
pub fn zellij_on_path() -> bool {
    Command::new("zellij")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Locate the `zellij` binary on `PATH` via stdlib search. Returns
/// `None` when no file named `zellij` exists on any `$PATH` entry.
///
/// Used by callers that need the resolved path (e.g. to stat the
/// executable or log it); the happy-path check should use
/// [`zellij_on_path`].
pub fn locate_zellij() -> Option<PathBuf> {
    let raw = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&raw) {
        let candidate = dir.join("zellij");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Is the current process running inside an existing zellij session?
///
/// Detected by reading `$ZELLIJ` — zellij exports it in every pane's
/// env. Nesting a harness inside that would confuse polling + pollute
/// the caller's session.
pub fn inside_zellij() -> bool {
    match std::env::var("ZELLIJ") {
        Ok(v) => !v.is_empty(),
        Err(_) => false,
    }
}

/// Run `zellij <args>` capturing stdout + stderr.
pub fn run_zellij(args: &[&str]) -> Result<Output> {
    Command::new("zellij")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("zellij {:?} failed to spawn", args))
}

/// Poll `zellij list-sessions` at 200 ms intervals until `name`
/// appears in the stdout lines, or `timeout` elapses.
///
/// Returns `true` iff the session surfaced within the deadline.
pub fn wait_for_session(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let out = Command::new("zellij").arg("list-sessions").output();
        if let Ok(o) = out {
            let text = String::from_utf8_lossy(&o.stdout);
            if text.lines().any(|line| line.contains(name)) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Best-effort `zellij kill-session <name>`.
pub fn kill_session(name: &str) -> Result<()> {
    let status = Command::new("zellij")
        .args(["kill-session", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("zellij kill-session {name} failed to spawn"))?;
    if !status.success() {
        // Non-fatal — a session that never came up won't exist to be
        // killed. Return Ok anyway so callers don't have to
        // special-case teardown.
        tracing::debug!(
            name = %name,
            code = status.code().unwrap_or(-1),
            "zellij kill-session non-zero exit",
        );
    }
    Ok(())
}

/// Dump the current screen contents of `session` via `zellij action
/// dump-screen <tmpfile>`, then read the file back.
///
/// `dump-screen` writes ANSI-bearing screen text to the file passed as
/// its positional argument. We use a tempfile so stdout/stderr of
/// zellij stay clean.
///
/// Returns `Err` when:
///   * the tempfile can't be created,
///   * `zellij action dump-screen` exits non-zero (session missing /
///     zellij version predates the subcommand / IPC error),
///   * the tempfile is empty after the call returns (some zellij
///     builds exit 0 without writing anything when the session has no
///     active pane yet).
pub fn dump_screen(session: &str) -> Result<String> {
    let tmp = tempfile::NamedTempFile::new()
        .with_context(|| "failed to create tempfile for dump-screen")?;
    let path = tmp.path().to_path_buf();
    let path_str = path.display().to_string();

    let status = Command::new("zellij")
        .args(["--session", session, "action", "dump-screen", &path_str])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| "failed to spawn `zellij action dump-screen`")?;

    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr).into_owned();
        return Err(anyhow!(
            "`zellij action dump-screen` exited with code {:?}: {stderr}",
            status.status.code()
        ));
    }

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read dump-screen output at {}", path.display()))?;
    if text.is_empty() {
        return Err(anyhow!("dump-screen produced an empty file"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_zellij_reads_env() {
        // SAFETY: single-threaded test. Restore prior value on drop.
        let prior = std::env::var_os("ZELLIJ");
        unsafe {
            std::env::remove_var("ZELLIJ");
        }
        assert!(!inside_zellij());
        unsafe {
            std::env::set_var("ZELLIJ", "");
        }
        assert!(!inside_zellij(), "empty $ZELLIJ should count as outside");
        unsafe {
            std::env::set_var("ZELLIJ", "/tmp/some/pipe");
        }
        assert!(inside_zellij());
        unsafe {
            match prior {
                Some(v) => std::env::set_var("ZELLIJ", v),
                None => std::env::remove_var("ZELLIJ"),
            }
        }
    }

    #[test]
    fn locate_zellij_consistent_with_version_probe() {
        // When `zellij --version` succeeds we expect PATH search to
        // find *something* named zellij. When the version probe
        // fails, PATH search is allowed to find or miss.
        if zellij_on_path() {
            assert!(
                locate_zellij().is_some(),
                "zellij --version succeeded but PATH search found nothing"
            );
        }
    }
}
