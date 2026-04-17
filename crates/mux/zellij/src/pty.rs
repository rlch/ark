//! Pty-backed zellij spawn (F-730 / F-731).
//!
//! Why this exists: zellij has no `--daemonize` flag. The first
//! `zellij -s <name> --layout <path>` invocation forks the zellij-server
//! daemon AND attaches a TUI client to the parent's controlling
//! terminal. From a bare shell we want to "spawn and forget" — start
//! the session and return immediately — but the TUI client refuses to
//! boot without a real TTY. Null stdio + `setsid` strips the TTY and
//! the client exits with code 2 before the server ever forks.
//!
//! The fix: allocate a pty pair via [`portable_pty`], wire the slave
//! end as the child's stdin/stdout/stderr, issue `TIOCSCTTY` so the
//! slave is the child's controlling terminal, and `setsid()` the
//! child so it owns its own session. The zellij client boots, forks
//! the server daemon (which double-forks and detaches), and the
//! caller can drop the pty pair as soon as a startup grace poll
//! confirms the client did not exit non-zero.
//!
//! This module is the single owner of the pty-zellij pattern. It is
//! consumed by:
//! - `crates/mux/zellij/src/mux.rs` — `ZellijMux::create_tab` for the
//!   first tab of an outside-zellij session (F-731). Replaced the
//!   broken `setsid zellij ...` external-binary call.
//!
//! The layout-file path MUST end in `.kdl` — zellij issue #4994
//! silently fails for other extensions when invoked with `--layout`.
//! Callers are responsible for the extension; this helper does not
//! validate.

use std::path::Path;

use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};
use thiserror::Error;

/// Errors from the pty-zellij spawn path.
#[derive(Debug, Error)]
pub enum PtySpawnError {
    /// The kernel could not allocate a new pty pair (file descriptor
    /// limit, missing `/dev/ptmx`, etc.). The error is propagated from
    /// the underlying `openpty(3)` call.
    #[error("openpty: {0}")]
    OpenPty(String),

    /// `portable_pty::SlavePty::spawn_command` failed. Most common
    /// causes: zellij is not on `PATH` (callers should have already
    /// run a `zellij --version` preflight) or `fork(2)` failed under
    /// resource pressure.
    #[error("spawn zellij in pty: {0}")]
    Spawn(String),

    /// The zellij client exited non-zero within the 500 ms startup
    /// grace window. Most common cause: the layout KDL is malformed
    /// or missing. Includes the exit code from the child's
    /// [`portable_pty::ExitStatus::exit_code`].
    #[error("zellij exited with code {code} before session came up")]
    EarlyExit {
        /// Exit code from `wait_for_ready`. `-1` if signal-killed.
        code: i32,
    },

    /// `try_wait` returned an I/O error during the startup poll.
    /// Should be exceedingly rare on Unix — `waitpid(WNOHANG)` is
    /// nearly infallible.
    #[error("wait on zellij pty child: {0}")]
    Wait(String),
}

/// Handle returned by [`spawn_zellij_with_pty`].
///
/// **Lifetime contract:** the caller MUST keep the handle alive
/// through the startup grace poll. Dropping the handle closes the
/// master fd, which sends `SIGHUP` to the zellij client (the slave is
/// its controlling terminal). After the server daemon has forked the
/// SIGHUP is harmless — the daemon has already detached and lives
/// independently — but a premature drop kills the client before the
/// server forks, leaving the session in a half-created state.
///
/// `pair` is listed first so it drops AFTER `child` per Rust's
/// declaration-order drop rule; this keeps the slave end open while
/// the child is being reaped, avoiding spurious `EIO` on the child's
/// stdio fds during teardown.
pub struct PtyZellijHandle {
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub pair: PtyPair,
}

/// Allocate a pty and spawn zellij inside it.
///
/// `cwd` is passed to the zellij client; the client inherits it as
/// its current directory, which is what shipped layouts expect when
/// they bind agent commands.
///
/// `layout_path` is the rendered KDL path on disk. Must end in `.kdl`
/// (zellij issue #4994).
///
/// Defaults: pty size 40 × 120, `set_controlling_tty(true)` (left at
/// the portable-pty default), no env-clearing — the child inherits
/// the caller's environment. Callers who need to customise these can
/// build their own `CommandBuilder` and call
/// `pair.slave.spawn_command` directly; the helper covers the common
/// case.
pub fn spawn_zellij_with_pty(
    session: &str,
    layout_path: &Path,
    cwd: &Path,
) -> Result<PtyZellijHandle, PtySpawnError> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| PtySpawnError::OpenPty(e.to_string()))?;

    let mut builder = CommandBuilder::new("zellij");
    builder.arg("-s");
    builder.arg(session);
    builder.arg("--layout");
    builder.arg(layout_path);
    builder.cwd(cwd);
    // controlling_tty defaults to true — portable-pty issues
    // TIOCSCTTY in its pre_exec, making the slave the child's
    // controlling terminal.

    let child = pair
        .slave
        .spawn_command(builder)
        .map_err(|e| PtySpawnError::Spawn(e.to_string()))?;

    Ok(PtyZellijHandle { child, pair })
}

/// Startup-grace poll for a pty-spawned zellij child.
///
/// Polls `try_wait()` every 50 ms for up to 500 ms. Returns:
/// - `Ok(())` if the child is still alive at the end of the window OR
///   exited cleanly (zellij-server forks itself a daemon and the
///   original client may exit 0 after).
/// - `Err(PtySpawnError::EarlyExit { code })` if the child exited
///   non-zero inside the grace.
/// - `Err(PtySpawnError::Wait(_))` on a `try_wait` I/O error.
///
/// 500 ms is a heuristic balance: long enough to catch fast failures
/// (bad layout KDL, plugin not installed) without making spawn feel
/// sluggish on the success path.
pub fn pty_child_startup_failure(
    child: &mut (dyn portable_pty::Child + Send + Sync),
) -> Result<(), PtySpawnError> {
    const GRACE_MS: u64 = 500;
    const POLL_MS: u64 = 50;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(GRACE_MS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let code = status.exit_code() as i32;
                return Err(PtySpawnError::EarlyExit { code });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            }
            Err(e) => {
                return Err(PtySpawnError::Wait(e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_child_startup_failure_ok_for_successful_exit() {
        // /usr/bin/true exits 0 immediately.
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let builder = CommandBuilder::new("/usr/bin/true");
        let mut child = pair.slave.spawn_command(builder).expect("spawn true");
        pty_child_startup_failure(child.as_mut()).expect("ok for true");
        drop(pair);
    }

    #[test]
    fn pty_child_startup_failure_reports_early_exit() {
        // /usr/bin/false exits 1 immediately.
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let builder = CommandBuilder::new("/usr/bin/false");
        let mut child = pair.slave.spawn_command(builder).expect("spawn false");
        let err = pty_child_startup_failure(child.as_mut()).expect_err("must fail");
        match err {
            PtySpawnError::EarlyExit { code } => assert_ne!(code, 0),
            other => panic!("expected EarlyExit, got {other:?}"),
        }
        drop(pair);
    }
}
