//! Production implementations of the [`Multiplexer`] and
//! [`SupervisorSpawner`] traits.

use std::path::Path;

use ark_types::{AgentSpec, StateLayout};

use super::traits::{Multiplexer, SupervisorSpawner};
use crate::commands::session::{
    ZellijInvocation, build_switch_session_command, build_zellij_command, inside_zellij,
    require_zellij_on_path,
};
use crate::error::CliError;
use crate::supervisor_handoff::{create_ready_pipe, wait_for_ready_default};

// ---------------------------------------------------------- multiplexer ----

/// Real zellij multiplexer. Thin wrapper over the existing helpers in
/// [`crate::commands::session`] — preflight is the `zellij --version`
/// probe, `is_inside` reads `$ZELLIJ`, and `run_session` dispatches
/// `zellij -s <name>` outside or `zellij action switch-session <name>`
/// inside.
#[derive(Default)]
pub struct ZellijMultiplexer;

impl ZellijMultiplexer {
    pub fn new() -> Self {
        Self
    }
}

impl Multiplexer for ZellijMultiplexer {
    fn preflight(&self) -> Result<(), CliError> {
        require_zellij_on_path()
    }

    fn is_inside(&self) -> bool {
        inside_zellij(|k| std::env::var(k).ok())
    }

    fn run_session(&self, session: &str, layout: Option<&Path>) -> Result<(), CliError> {
        let plan = ZellijInvocation {
            session: session.to_string(),
            layout: layout.map(|p| p.display().to_string()),
        };

        if self.is_inside() {
            let mut cmd = build_switch_session_command(&plan);
            let status = cmd.status().map_err(|e| CliError::Internal {
                reason: format!("zellij action switch-session: {e}"),
            })?;
            if !status.success() {
                let code = status.code().unwrap_or(-1);
                return Err(CliError::Internal {
                    reason: format!("zellij action switch-session exited with code {code}"),
                });
            }
        } else {
            let mut cmd = build_zellij_command(&plan);
            let status = cmd.status().map_err(|e| CliError::Internal {
                reason: format!("zellij: {e}"),
            })?;
            if !status.success() {
                let code = status.code().unwrap_or(-1);
                return Err(CliError::Internal {
                    reason: format!("zellij exited with code {code}"),
                });
            }
        }

        Ok(())
    }
}

// --------------------------------------------------------- supervisor ----

/// Real fork-based supervisor spawner.
///
/// Calls `ark_supervisor::daemonize()` (classic double-fork + setsid +
/// stdio redirect), drives `supervisor_main` in the daemon grandchild,
/// and uses a pipe-inheritance ready handshake so the parent CLI
/// blocks until the supervisor signals ready (or fails cleanly on
/// EOF / timeout).
///
/// The daemon branch never returns from `spawn_and_wait_for_ready`;
/// it calls `std::process::exit(code)` once the supervisor finishes.
/// The parent branch returns `Ok(())` after ready-ack.
#[derive(Default)]
pub struct ForkSupervisor;

impl ForkSupervisor {
    pub fn new() -> Self {
        Self
    }
}

impl SupervisorSpawner for ForkSupervisor {
    fn spawn_and_wait_for_ready(
        &self,
        spec: AgentSpec,
        state_layout: &StateLayout,
    ) -> Result<(), CliError> {
        let (ready_rfd, ready_wfd) = create_ready_pipe()?;

        // SAFETY: `daemonize()` calls `fork(2)`. No tokio runtime or
        // worker threads exist at this point — the CLI pipeline above
        // is purely synchronous. This satisfies the single-threaded
        // fork precondition (see SupervisorSpawner trait docs).
        match ark_supervisor::daemonize(state_layout, &spec.id) {
            Err(e) => Err(CliError::Internal {
                reason: format!("daemonize supervisor: {e}"),
            }),
            Ok(ark_supervisor::DaemonizeOutcome::Daemon) => {
                // Grandchild: supervisor process. Drop the parent's
                // read end, wrap write end in ReadyWriter, drive
                // supervisor_main on a fresh tokio runtime, and exit.
                drop(ready_rfd);
                let writer = ark_supervisor::ReadyWriter::from_owned_fd(ready_wfd);
                let config = ark_core::Config::placeholder();

                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!(error = %e, "build tokio runtime in supervisor");
                        std::process::exit(3);
                    }
                };

                let result = runtime.block_on(ark_supervisor::supervisor_main(
                    spec,
                    ark_supervisor::SupervisorMode::Daemon,
                    config,
                    Some(writer),
                    None,
                ));
                // Cavekit-soul-phase-1 T-015: `supervisor_main` now returns
                // `Result<(), anyhow::Error>`. The richer `Outcome` variants
                // (Killed / Timeout / Crashed) have been collapsed into the
                // generic error case — infrastructure failures or an
                // orchestrator-level crash both exit with 1, while a clean
                // end-of-life exits with 0. Methodology-specific lifecycle
                // signalling re-homes inside extensions in Phase 2+.
                match result {
                    Ok(()) => {
                        tracing::info!("supervisor exited cleanly");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "supervisor returned Err");
                        std::process::exit(1);
                    }
                }
            }
            Ok(ark_supervisor::DaemonizeOutcome::Parent { child_pid }) => {
                // CLI process. Drop write end so EOF fires if the
                // supervisor dies before sending ready.
                drop(ready_wfd);
                tracing::debug!(
                    child_pid = %child_pid,
                    "daemonized supervisor; waiting for ready"
                );
                if let Err(e) = wait_for_ready_default(ready_rfd) {
                    tracing::warn!(
                        child_pid = %child_pid,
                        error = ?e,
                        "supervisor failed to ready",
                    );
                    return Err(e);
                }
                Ok(())
            }
        }
    }
}
