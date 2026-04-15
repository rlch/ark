//! Parent ↔ supervisor-daemon ready handshake (W-2 / cavekit-supervisor R3
//! step 12).
//!
//! Background: the previous step-12 implementation wrote `agent_id\n` to
//! stdout, expecting the parent CLI to read it. That never worked under
//! `daemonize()` because [`crate::daemon::daemonize`] redirects the
//! grandchild's stdout to `supervisor.log` *before* `run_supervisor`
//! starts — so the "signal" went to a log file the parent never read.
//!
//! This module replaces the broken signal with a pipe-inheritance
//! handshake (Stevens APUE pattern; mirrors the prior art in zellij's
//! IPC bootstrap and wezterm-mux-server's startup):
//!
//! 1. The CLI parent creates a `pipe2(O_CLOEXEC)` BEFORE calling
//!    `daemonize()`. Both the read fd (kept by the parent) and the
//!    write fd (passed to the supervisor) survive the fork.
//! 2. After `daemonize()` returns, the parent closes the write fd; the
//!    daemon closes the read fd.
//! 3. The daemon wraps the write fd in a [`ReadyWriter`] and threads it
//!    into [`crate::run_supervisor`]. At step 12, the supervisor calls
//!    [`ReadyWriter::write_ack`], which writes a single ACK byte (0x06)
//!    and drops the fd — the kernel closes it, the parent's `read()`
//!    returns 1.
//! 4. If the daemon dies before step 12, the OS closes the write fd
//!    when the process exits → parent's `read()` returns 0 (EOF) →
//!    parent surfaces a clean failure instead of hanging on a 5s
//!    timeout.
//!
//! The parent-side `wait_for_ready` helper lives in `ark-cli`
//! (`crates/cli/src/supervisor_handoff.rs`) so this crate has no
//! reverse dependency on the CLI types.

use std::io::Write;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};

/// One-shot writer wrapping the supervisor's end of a parent-CLI ready
/// pipe. The supervisor calls [`write_ack`] once, between R3 step 11
/// (`Started` event emitted) and step 13 (`orchestrator.run`).
///
/// Drop semantics: a `ReadyWriter` that is dropped without `write_ack`
/// being called is interpreted by the parent as failure — the kernel
/// closes the fd on drop, the parent's `read()` returns 0 (EOF), and
/// the parent surfaces a "supervisor exited before signalling ready"
/// error.
#[derive(Debug)]
pub struct ReadyWriter {
    fd: OwnedFd,
}

impl ReadyWriter {
    /// Wrap an already-owned write fd. Used by the daemon entry path to
    /// adopt the fd inherited from the CLI parent across `fork()`.
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Wrap a raw fd. Caller must guarantee the fd is open and unique
    /// (no other handle aliases it). Used at the supervisor entry point
    /// where the fd number was passed via the inherited pipe — there
    /// is no `OwnedFd` to hand over because the fd was created in the
    /// CLI parent process before fork.
    ///
    /// # Safety
    /// `fd` must be open, owned by the caller, and not aliased
    /// elsewhere in the process.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        // SAFETY: contract delegated to the caller.
        Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }

    /// Send the ACK byte (0x06) and close the fd. Returns `Ok(())` if
    /// the byte was written — the parent will observe `read() == 1`
    /// with `buf[0] == 0x06`. Errors are logged at the call site so
    /// the supervisor doesn't abort an otherwise-healthy run on a
    /// pipe-write failure (the parent's 5 s timeout will catch it).
    pub fn write_ack(self) -> std::io::Result<()> {
        let mut file = std::fs::File::from(self.fd);
        file.write_all(&[ACK_BYTE])?;
        file.flush()?;
        // file drop closes the fd; parent's read() then returns the
        // byte we just wrote.
        Ok(())
    }

    /// Return the raw fd number for callers that need to pass it across
    /// a re-exec or via env var. Consumes the writer (the caller now
    /// owns the fd lifetime). Currently unused — the in-process daemon
    /// model just uses [`from_owned_fd`] / [`from_raw_fd`] directly —
    /// but exposed for future re-exec paths.
    pub fn into_raw_fd(self) -> RawFd {
        self.fd.into_raw_fd()
    }
}

/// The ACK byte written by [`ReadyWriter::write_ack`] and matched by
/// the CLI parent's `wait_for_ready`. Value chosen to match ASCII ACK
/// (0x06); any non-zero value would work, but ACK is conventional.
pub const ACK_BYTE: u8 = 0x06;
