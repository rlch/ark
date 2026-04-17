//! Injection seams for the bare-`ark` launch path.
//!
//! Splitting the launch pipeline behind two small traits is what makes
//! the bare-ark flow testable without forking real supervisors or
//! spawning real zellij processes. Prod callers use the real impls in
//! [`super::real`]; tests substitute the mocks in [`super::mock`].
//!
//! ## Scope
//!
//! These traits are deliberately launch-crate-internal. They exist
//! purely to hang tests on; there is no plan to add a second prod
//! multiplexer (zellij already ships a web client for browser use —
//! we don't need ark to abstract over multiplexer vendors) or a second
//! supervisor-spawn strategy. Keep the surface minimal.

use std::path::Path;

use ark_types::{AgentSpec, StateLayout};

use crate::error::CliError;

/// Abstraction over the terminal multiplexer that owns the launched
/// session.
///
/// Prod impl: [`super::real::ZellijMultiplexer`].
/// Test impl: [`super::mock::MockMultiplexer`].
pub trait Multiplexer {
    /// Fail fast when the multiplexer binary is missing or too old.
    /// Called once at the top of `launch::run`, before any filesystem
    /// mutation or supervisor fork.
    fn preflight(&self) -> Result<(), CliError>;

    /// Whether the current process is already inside a multiplexer
    /// session of this kind. Drives the inside-vs-outside branching
    /// inside [`Self::run_session`]; exposed for diagnostics (doctor,
    /// debug logging) but not normally called directly by `launch::run`.
    fn is_inside(&self) -> bool;

    /// Launch-or-switch into `session`, optionally applying `layout`.
    ///
    /// Blocks until the foreground client returns (outside) or until
    /// the dispatch ack lands (inside). The implementation picks
    /// `new-session` vs `switch-session` based on [`Self::is_inside`].
    fn run_session(&self, session: &str, layout: Option<&Path>) -> Result<(), CliError>;
}

/// Abstraction over "fork a supervisor for this agent and wait for it
/// to signal ready".
///
/// Prod impl: [`super::real::ForkSupervisor`] — calls
/// `ark_supervisor::daemonize()` (double-fork), runs `supervisor_main`
/// in the daemon grandchild, returns to the parent on ready-ack. In
/// the daemon branch the grandchild never returns from this call — it
/// exits the process directly via `std::process::exit`.
///
/// Test impl: [`super::mock::InlineSupervisor`] — records the spec and
/// synthesises a ready-ack without forking.
pub trait SupervisorSpawner {
    /// Spawn (or simulate spawning) the supervisor for `spec` and
    /// block until it signals ready. Returns `Ok(())` once the ready
    /// handshake completes in the CLI parent.
    ///
    /// ## Fork-safety contract
    ///
    /// Real impls call `fork(2)`. Callers MUST invoke this method
    /// BEFORE building any tokio runtime — fork(2) on a
    /// multi-threaded process produces a child whose non-forked threads
    /// no longer exist but whose mutexes and runtime state are
    /// inherited in a subtly broken state. `launch::run` satisfies
    /// this by keeping the pre-spawn pipeline purely synchronous.
    fn spawn_and_wait_for_ready(
        &self,
        spec: AgentSpec,
        state_layout: &StateLayout,
    ) -> Result<(), CliError>;
}
