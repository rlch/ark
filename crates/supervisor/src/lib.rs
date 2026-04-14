//! Ark supervisor — fork/detach + lifecycle primitives.
//!
//! This crate hosts the ephemeral per-agent supervisor process logic. See
//! `cavekit-supervisor.md` for the full spec. The modules here currently
//! cover:
//!
//! - [`daemon`] — `daemonize()` double-fork/setsid detach + stdio redirect
//!   + `tracing-subscriber` install (cavekit-supervisor R1, minus
//!   `--no-detach` which is tracked separately in T-063).
//! - [`lock`] — per-agent file lock under `$STATE/locks/{id}.lock` used to
//!   guard against double-spawn collisions (cavekit-supervisor R3 step 2 +
//!   R5 crash-recovery hand-off).
//!
//! Future waves layer orchestration, signal handling, and the control
//! socket on top of these primitives.

#![cfg(unix)]

pub mod daemon;
pub mod lock;

pub use daemon::{DaemonizeError, DaemonizeOutcome, daemonize, setup_supervisor_log};
pub use lock::{LockError, LockGuard, acquire_lock};
