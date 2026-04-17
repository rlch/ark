//! `supervisor_main` — top-level bootstrap entry point for the supervisor
//! process (W-1 / cavekit-supervisor R1 + R3).
//!
//! This module provides [`supervisor_main`], the single async function that
//! both the daemon branch and the `--no-detach` foreground path call to
//! drive the full R3 18-step lifecycle. It wraps
//! [`crate::orchestration::run_supervisor`] with:
//!
//! 1. **Readiness-signal ownership**: the ready writer (`Option<ReadyWriter>`)
//!    is consumed by the underlying `run_supervisor` at R3 step 12. If
//!    `run_supervisor` fails before step 12, the writer is dropped without
//!    writing the ACK — the parent CLI observes EOF and surfaces a clean
//!    failure (W-2 protocol).
//!
//! 2. **Error logging**: pre-ready errors are logged at `error` level before
//!    propagating. Callers that run inside a detached daemon (where the only
//!    observer is `supervisor.log`) get a visible trace line.
//!
//! 3. **Completion tracing**: on success, the clean exit is logged at
//!    `info` level so `supervisor.log` always carries a terminal record.
//!
//! ## Callers
//!
//! * **Daemon branch** (`spawn.rs`, post-`daemonize()`): builds a
//!   current-thread tokio runtime and calls `runtime.block_on(supervisor_main(...))`.
//!   Maps the returned `Result<()>` to a Unix exit code with
//!   `match result { Ok(()) => 0, Err(_) => 1 }`.
//!
//! * **Foreground / `--no-detach`** (`spawn.rs`, no-fork path): spawns a
//!   background thread with a tokio runtime that drives `supervisor_main(...)`.
//!   Passes an `external_cancel` so the main thread can trigger shutdown
//!   when the foreground zellij process exits.

use anyhow::Result;
use ark_core::Config;
use ark_types::{CancellationToken, SessionSpec};
use tracing::{error, info};

use crate::orchestration::SupervisorMode;
use crate::ready_signal::ReadyWriter;

/// Run the supervisor to completion — the top-level bootstrap helper.
///
/// Wraps [`crate::run_supervisor`] with readiness-signal ownership and
/// structured error handling. See the module docs for the full contract.
///
/// # Arguments
///
/// * `spec` — the agent spec built by the CLI.
/// * `mode` — `Daemon` or `Foreground`; controls informational logging.
/// * `config` — the config object (currently `Config::placeholder()`; future
///   tiers thread the real figment-loaded config).
/// * `ready_writer` — the supervisor's end of the parent ↔ daemon ready
///   pipe (W-2). On the happy path, `run_supervisor` writes the ACK byte at
///   R3 step 12. On failure before step 12, the writer is dropped → parent
///   sees EOF. Pass `None` for `--no-detach` paths that don't use the pipe.
/// * `external_cancel` — optional cancellation token held by the caller. The
///   daemon path passes `None` (internal signal handler drives cancel); the
///   `--no-detach` path passes a token it can fire when the foreground
///   zellij process exits.
///
/// # Returns
///
/// `Ok(())` on a clean run. Methodology-specific "failed" / "killed" /
/// "timeout" / "crashed" states are persisted to `status.json` via
/// [`crate::finalize_state`] but do not flow back out of the return type —
/// they all still yield `Ok(())` here. `Err` signals that the supervisor
/// infrastructure itself could not start or could not complete (lock,
/// socket, scene compile, etc.). See cavekit-soul-phase-1-supervisor.md R3.
pub async fn supervisor_main(
    spec: SessionSpec,
    mode: SupervisorMode,
    config: Config,
    ready_writer: Option<ReadyWriter>,
    external_cancel: Option<CancellationToken>,
) -> Result<()> {
    let agent_id = spec.id.clone();

    match crate::run_supervisor(spec, mode, config, ready_writer, external_cancel).await {
        Ok(()) => {
            info!(
                agent = %agent_id.as_str(),
                "supervisor_main: supervisor exited cleanly"
            );
            Ok(())
        }
        Err(err) => {
            // The ReadyWriter (if any) was passed into run_supervisor and
            // is dropped at this point — the parent CLI will see EOF on its
            // read end, surfacing "supervisor exited before signalling
            // ready" (W-2 protocol). We log the actual error here so
            // supervisor.log carries the root cause.
            error!(
                agent = %agent_id.as_str(),
                error = %err,
                "supervisor_main: supervisor failed before completion"
            );
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO(cavekit-soul Phase 1): three supervisor_main tests dropped here —
    //   * supervisor_main_returns_ok_on_success
    //   * supervisor_main_propagates_infrastructure_error
    //   * supervisor_main_drops_ready_writer_on_error
    //
    // They all relied on `build_engine` rejecting the "stub-engine" slug.
    // Post-migration the factory accepts any non-empty slug (engine identity
    // moved out of core scope), and SessionSpec no longer carries an engine
    // field at all — so the "unknown engine slug" error path these tests
    // probed is gone. Reconstructing a forced-failure path through
    // `supervisor_main` requires a real StateLayout + mux injection, which
    // belongs in `run_supervisor_with` integration coverage (orchestration.rs).
    // Rewriting for the cavekit-soul success path is Tier-4 follow-up work.

    /// When a ReadyWriter is provided and the supervisor succeeds through
    /// step 12, the ACK byte must have been written. We can't easily test
    /// this through supervisor_main (it requires a real StateLayout + mux),
    /// but the orchestration tests cover that path. Here we verify the
    /// contract: ReadyWriter::write_ack writes ACK_BYTE and closes the fd.
    #[test]
    fn ready_writer_write_ack_writes_ack_byte_and_closes_fd() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe");

        let writer = unsafe { ReadyWriter::from_raw_fd(std::os::fd::AsRawFd::as_raw_fd(&write_fd)) };
        std::mem::forget(write_fd);

        writer.write_ack().expect("write_ack");

        let mut buf = [0u8; 2];
        let n = nix::unistd::read(std::os::fd::AsRawFd::as_raw_fd(&read_fd), &mut buf)
            .expect("read");
        assert_eq!(n, 1, "exactly 1 byte should be written");
        assert_eq!(buf[0], crate::ready_signal::ACK_BYTE, "byte must be ACK_BYTE");

        // Second read should return EOF (fd closed after write_ack consumed
        // the writer).
        let n2 = nix::unistd::read(std::os::fd::AsRawFd::as_raw_fd(&read_fd), &mut buf)
            .expect("read eof");
        assert_eq!(n2, 0, "second read must return EOF");
    }
}
