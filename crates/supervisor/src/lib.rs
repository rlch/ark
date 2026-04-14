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

// T-066: per-supervisor socket command handlers (cavekit-hook-ipc R5).
pub mod commands;
pub use commands::{SignalSender, SupervisorCommandCtx, SupervisorCommandHandler};

// T-067: signal handlers + socket-cleanup guard (cavekit-supervisor R7).
pub mod signals;
pub use signals::{ControlSocketGuard, SignalTaskHandle, install_signal_handlers};

// T-068: control-socket audit log (cavekit-hook-ipc R5 audit log bullet).
pub mod audit_log;
pub use audit_log::AuditLogger;

// T-069: factory + full R3 boot sequence.
pub mod factory;
pub mod orchestration;
pub use factory::{build_engine, build_multiplexer, build_orchestrator};
pub use orchestration::{
    SupervisorMode, finalize_state, outcome_exit_code, run_supervisor, run_supervisor_with,
};

// T-070: SIGTERM kill handler (cavekit-supervisor R4).
pub mod kill;
pub use kill::{DEFAULT_KILL_GRACE, TabRegistry, apply_tab_event, kill_handler, new_tab_registry};

// T-071: PID liveness + crash detection (cavekit-supervisor R5).
pub mod crash;
pub use crash::{adjust_status_if_crashed, detect_crashed, is_pid_alive};

// T-072: auto-close policy on outcome (cavekit-supervisor R6).
pub mod auto_close;
pub use auto_close::{AutoClosePolicy, apply_auto_close_policy, collect_opened_tabs};
