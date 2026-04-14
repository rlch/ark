//! Ark supervisor — fork/detach + lifecycle primitives.
//!
//! This crate hosts the ephemeral per-agent supervisor process logic. See
//! `cavekit-supervisor.md` for the full spec. The modules here currently
//! cover:
//!
//! - [`daemon`] — `daemonize()` double-fork/setsid detach, stdio redirect
//!   and `tracing-subscriber` install (cavekit-supervisor R1, first four
//!   bullets).
//! - [`foreground`] — `run_foreground()` `--no-detach` entry point that
//!   tees tracing to file + parent stderr (cavekit-supervisor R1, fifth
//!   bullet; T-063).
//! - [`lock`] — per-agent file lock under `$STATE/locks/{id}.lock` used to
//!   guard against double-spawn collisions (cavekit-supervisor R3 step 2 +
//!   R5 crash-recovery hand-off).
//! - [`control_socket`] — per-agent unix control socket bind + accept
//!   loop (cavekit-supervisor R7; T-065). Reuses `ark_core::ControlListener`
//!   for the low-level bind + NDJSON codec.
//!
//! Future waves layer orchestration, signal handling, and the full R3
//! boot sequence on top of these primitives.

#![cfg(unix)]

pub mod control_socket;
pub mod daemon;
pub mod foreground;
pub mod lock;

pub use control_socket::{
    ControlCommandHandler, ControlSocketHandle, NoopHandler, bind_control_socket, shutdown,
};
pub use daemon::{DaemonizeError, DaemonizeOutcome, daemonize, setup_supervisor_log};
pub use foreground::{
    ForegroundCtx, SharedWriter, StderrSink, build_foreground_dispatch, run_foreground,
};
pub use lock::{LockError, LockGuard, acquire_lock};
