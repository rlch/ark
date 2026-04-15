//! W-2: parent CLI ↔ supervisor-daemon ready handshake.
//!
//! Pipe-inheritance pattern (Stevens APUE; mirrors zellij IPC + wezterm
//! mux-server). The CLI parent creates a `pipe2(O_CLOEXEC)` BEFORE
//! calling `daemonize()`. After fork:
//!
//! - **Parent** closes the write end and polls the read end with a 5 s
//!   timeout via [`wait_for_ready`]. On read=1 byte ACK → success; on
//!   read=0 (EOF) → supervisor died before signalling; on timeout →
//!   surface a clean error.
//!
//! - **Daemon** closes the read end, wraps the write end in
//!   [`ark_supervisor::ReadyWriter`], and threads it into
//!   `run_supervisor`. The supervisor calls `write_ack` after R3 step
//!   11 (`Started` event emitted), satisfying the "<1 s parent return"
//!   contract from `cavekit-supervisor.md` R1.
//!
//! `O_CLOEXEC` is set on both ends because we never `exec()`; the in-
//! process daemon model just `fork()`s. The flag's there as defence-
//! in-depth in case anyone later inserts an `exec` between
//! `create_ready_pipe` and `daemonize`.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::Duration;

use nix::fcntl::{F_SETFD, FdFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout};
use nix::unistd::{pipe, read};

use crate::error::CliError;

/// Default timeout for the parent's `wait_for_ready`. Matches the
/// `<1 s typical, < 5 s budget` contract from cavekit-supervisor R1.
pub const READY_TIMEOUT_MS: u64 = 5000;

/// ACK byte the supervisor writes when ready. Mirrors
/// [`ark_supervisor::ACK_BYTE`] — duplicated here to avoid a runtime
/// import path that's only ever compared.
const ACK_BYTE: u8 = ark_supervisor::ACK_BYTE;

/// Allocate the parent ↔ daemon ready pipe.
///
/// Returns `(read_fd, write_fd)`. The caller MUST:
/// 1. Pass `write_fd` to the supervisor (in the in-process daemon
///    model: just keep both fds in scope through `daemonize()`; both
///    survive `fork()` via OS fd table inheritance).
/// 2. After `daemonize()` returns, close the fd that does NOT belong
///    to your branch (parent closes `write_fd`, daemon closes
///    `read_fd`). Dropping `OwnedFd` calls `close(2)`.
///
/// `FD_CLOEXEC` is set on both ends so an accidental `exec` between
/// this call and the fork drops both ends — fail-loud rather than
/// fail-silently. Set via `fcntl(F_SETFD)` instead of `pipe2(O_CLOEXEC)`
/// because `pipe2` is Linux/BSD-only — macOS exposes `pipe(2)` plus
/// `fcntl` only.
pub fn create_ready_pipe() -> Result<(OwnedFd, OwnedFd), CliError> {
    let (read_fd, write_fd) = pipe().map_err(|errno| CliError::Internal {
        reason: format!("create ready pipe: {errno}"),
    })?;
    set_cloexec(&read_fd)?;
    set_cloexec(&write_fd)?;
    Ok((read_fd, write_fd))
}

fn set_cloexec(fd: &OwnedFd) -> Result<(), CliError> {
    fcntl(fd.as_raw_fd(), F_SETFD(FdFlag::FD_CLOEXEC)).map_err(|errno| CliError::Internal {
        reason: format!("set FD_CLOEXEC on ready pipe: {errno}"),
    })?;
    Ok(())
}

/// Outcome of [`wait_for_ready`].
#[derive(Debug)]
enum WaitOutcome {
    /// Supervisor wrote the ACK byte → ready.
    Acked,
    /// Read returned 0 bytes (EOF) → supervisor closed its end without
    /// writing. Indicates the daemon process died before R3 step 12.
    SupervisorDiedBeforeAck,
    /// Read returned 1 byte but it was not the ACK value. Reserved for
    /// a future failure-byte sentinel; for now we treat it the same as
    /// EOF — the supervisor wrote something it shouldn't have.
    UnexpectedByte(u8),
    /// `poll(2)` returned 0 events — the supervisor never wrote within
    /// the timeout. Most likely cause: the daemon is wedged in a long
    /// preflight or factory build.
    Timeout,
}

/// Block until the supervisor signals ready or `timeout` elapses.
///
/// Implementation: `poll(2)` on `read_fd` for `POLLIN` with a millisecond
/// timeout. On wake, `read(2)` exactly 1 byte and classify the outcome.
///
/// Returns `Ok(())` only on `Acked`. All other outcomes map to
/// `CliError::Internal` with a reason that names the failure mode so
/// the operator can diagnose without a log dive.
pub fn wait_for_ready(read_fd: OwnedFd, timeout: Duration) -> Result<(), CliError> {
    let outcome = poll_for_ack(&read_fd, timeout)?;
    match outcome {
        WaitOutcome::Acked => Ok(()),
        WaitOutcome::SupervisorDiedBeforeAck => Err(CliError::Internal {
            reason: "supervisor exited before signalling ready (check supervisor.log)".into(),
        }),
        WaitOutcome::UnexpectedByte(b) => Err(CliError::Internal {
            reason: format!(
                "supervisor sent unexpected ready byte 0x{b:02x} (expected 0x{ACK_BYTE:02x})"
            ),
        }),
        WaitOutcome::Timeout => Err(CliError::Internal {
            reason: format!(
                "supervisor failed to signal ready within {ms}ms (check supervisor.log)",
                ms = timeout.as_millis()
            ),
        }),
    }
}

/// `wait_for_ready` with the default [`READY_TIMEOUT_MS`].
pub fn wait_for_ready_default(read_fd: OwnedFd) -> Result<(), CliError> {
    wait_for_ready(read_fd, Duration::from_millis(READY_TIMEOUT_MS))
}

fn poll_for_ack(read_fd: &OwnedFd, timeout: Duration) -> Result<WaitOutcome, CliError> {
    let raw = read_fd.as_raw_fd();
    let borrowed = read_fd.as_fd();
    let mut fds = [PollFd::new(borrowed, PollFlags::POLLIN)];

    let timeout_ms: u16 = timeout.as_millis().min(u16::MAX as u128) as u16;
    let poll_timeout = PollTimeout::from(timeout_ms);

    let n = nix::poll::poll(&mut fds, poll_timeout).map_err(|errno| CliError::Internal {
        reason: format!("poll ready pipe: {errno}"),
    })?;
    if n == 0 {
        return Ok(WaitOutcome::Timeout);
    }

    let mut buf = [0u8; 1];
    // SAFETY: nix::unistd::read takes a borrowed fd; we constructed it
    // from the OwnedFd we still hold, so the fd is valid for this call.
    let bytes_read = read(raw, &mut buf).map_err(|errno| CliError::Internal {
        reason: format!("read ready pipe: {errno}"),
    })?;
    match bytes_read {
        0 => Ok(WaitOutcome::SupervisorDiedBeforeAck),
        1 if buf[0] == ACK_BYTE => Ok(WaitOutcome::Acked),
        1 => Ok(WaitOutcome::UnexpectedByte(buf[0])),
        n => Err(CliError::Internal {
            reason: format!("read ready pipe: unexpected byte count {n}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::thread;

    #[test]
    fn create_ready_pipe_returns_two_fds() {
        let (rfd, wfd) = create_ready_pipe().expect("pipe");
        assert_ne!(rfd.as_raw_fd(), wfd.as_raw_fd());
    }

    #[test]
    fn wait_for_ready_returns_ok_on_ack_byte() {
        let (rfd, wfd) = create_ready_pipe().expect("pipe");
        // Spawn a writer thread that ACKs after a short delay.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            // SAFETY: wfd is owned by this thread, fd valid.
            let mut f = unsafe { std::fs::File::from_raw_fd(wfd.into_raw_fd()) };
            f.write_all(&[ACK_BYTE]).expect("write ack");
        });
        wait_for_ready(rfd, Duration::from_secs(1)).expect("ready");
    }

    #[test]
    fn wait_for_ready_returns_died_when_writer_drops_without_writing() {
        let (rfd, wfd) = create_ready_pipe().expect("pipe");
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            drop(wfd); // simulates supervisor crash before R3 step 12
        });
        let err = wait_for_ready(rfd, Duration::from_secs(1)).expect_err("must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("supervisor exited before signalling ready"),
            "unexpected error text: {msg}"
        );
    }

    #[test]
    fn wait_for_ready_returns_timeout_when_writer_silent() {
        let (rfd, _wfd) = create_ready_pipe().expect("pipe");
        // _wfd held in scope but never written → no ACK, no EOF.
        let err = wait_for_ready(rfd, Duration::from_millis(150)).expect_err("must timeout");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("failed to signal ready within"),
            "unexpected error text: {msg}"
        );
    }

    #[test]
    fn wait_for_ready_rejects_unexpected_byte() {
        let (rfd, wfd) = create_ready_pipe().expect("pipe");
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut f = unsafe { std::fs::File::from_raw_fd(wfd.into_raw_fd()) };
            f.write_all(&[0xFF]).expect("write garbage");
        });
        let err = wait_for_ready(rfd, Duration::from_secs(1)).expect_err("must reject");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unexpected ready byte 0xff"),
            "unexpected error text: {msg}"
        );
    }
}
