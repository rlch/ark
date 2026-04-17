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

pub mod consumers;
pub mod control_socket;
pub mod daemon;
pub mod foreground;
pub mod lock;

pub use consumers::status_pipe;

pub use control_socket::{
    ControlCommandHandler, ControlSocketHandle, NoopHandler, bind_control_socket, shutdown,
};
pub use daemon::{DaemonizeError, DaemonizeOutcome, daemonize, setup_supervisor_log};
pub use foreground::{
    ForegroundCtx, SharedWriter, StderrSink, build_foreground_dispatch, run_foreground,
};
pub use lock::{LockError, LockGuard, acquire_lock};

// T-066: per-supervisor socket command handlers (cavekit-hook-ipc R5).
//
// T-6.2 grew the surface with `Intent` / `Emit` / `Permit` for the
// scene-bridge dispatchers (`ark-hook intent | emit | permit`).
pub mod commands;
pub use commands::{
    IntentBridge, SignalSender, SupervisorCommandCtx, SupervisorCommandHandler,
};

// T-067: signal handlers + socket-cleanup guard (cavekit-supervisor R7).
pub mod signals;
pub use signals::{ControlSocketGuard, SignalTaskHandle, install_signal_handlers};

// T-068: control-socket audit log (cavekit-hook-ipc R5 audit log bullet).
pub mod audit_log;
pub use audit_log::AuditLogger;

// T-069: factory + full R3 boot sequence.
pub mod factory;
pub mod orchestration;
pub use factory::{SupervisorError, build_engine, build_multiplexer, build_orchestrator};

// T-ACP.7: minimal ACP-engine Engine-trait stub that replaces the
// retired `ark-engines-claude-code` crate.
pub mod engine_stub;
pub use engine_stub::{AcpEngineStub, preflight as engine_preflight};
pub use orchestration::{
    SupervisorMode, finalize_state, run_supervisor, run_supervisor_with,
};

// W-1: supervisor_main bootstrap helper (cavekit-supervisor R1 + R3 step 12).
// Top-level async entry point wrapping run_supervisor with readiness-signal
// ownership and structured error logging. Both the daemon branch and the
// --no-detach foreground path call this.
pub mod bootstrap;
pub use bootstrap::supervisor_main;

// T-070: SIGTERM kill handler (cavekit-supervisor R4).
pub mod kill;
pub use kill::{DEFAULT_KILL_GRACE, TabRegistry, apply_tab_event, kill_handler, new_tab_registry};

// T-071: PID liveness + crash detection (cavekit-supervisor R5).
pub mod crash;
pub use crash::{adjust_status_if_crashed, detect_crashed, is_pid_alive};

// T-072 / cavekit-soul-phase-1 T-017: auto-close on CoreEvent::SessionEnded.
pub mod auto_close;
pub use auto_close::apply_auto_close_policy;

// W-2: parent ↔ daemon ready handshake (cavekit-supervisor R3 step 12).
pub mod ready_signal;
pub use ready_signal::{ACK_BYTE, ReadyWriter};

// T-7.2: plugin lifecycle manager — tracks mount state for every scene-
// declared plugin, fans out mount failures as `ark.plugin.failed`
// UserEvents, and drives the always-on mount sequence at session boot.
pub mod plugin_lifecycle;
pub use plugin_lifecycle::{
    MountOutcome, MountState, PluginLifecycleManager, PLUGIN_FAILED_EVENT,
};

// T-8.1: scene compile at supervisor boot. Reads `AgentSpec.scene_path`
// (falling back to the embedded built-in default), parses + validates
// the scene, builds a `ReactionRegistry` from its `on { }` / `keybind`
// nodes, and exposes lowered plugin decls for the always-on mount
// pass. See cavekit-supervisor.md R3 step 7.
pub mod scene_runtime;
pub use scene_runtime::{CompiledScene, SceneSource, compile_scene_for_runtime};

