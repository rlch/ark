//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `config` — placeholder `Config` (T-018 fills the schema).
//! - `consumers` — supervisor broadcast-bus consumer tasks
//!   (`state_writer`, `reaction_dispatcher`) per cavekit-supervisor.md R2.
//!   The mux-coupled `status_pipe` consumer lives in `ark-supervisor` so
//!   `ark-core` stays mux-free. T-5.7 deleted the legacy `hook_dispatcher`
//!   consumer; legacy `[[hooks]]` config is now compiled into a synthetic
//!   `ReactionRegistry` (`ark_scene::hook_compat`) and dispatched through
//!   the unified `reaction_dispatcher`.
//! - `control_socket` — per-supervisor unix control socket primitive
//!   (cavekit-hook-ipc.md R4, cavekit-supervisor.md R7).
//! - `engine` — `Engine` trait + `EngineHandle` + `ApprovalPolicy`
//!   (cavekit-architecture.md R1).
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant reader
//!   (cavekit-types-state-events.md R7).
//! - `orchestrator` — `Orchestrator` trait + `World` capability bag
//!   (cavekit-architecture.md R2 + R3). `World.mux` is
//!   `Arc<ark_mux_zellij::ZellijMux>` (concrete).
//! - `socket_paths` — agents-socket-dir + per-agent path helpers
//!   (cavekit-hook-ipc.md R4).
//! - `status_writer` — atomic `status.json` writer/reader
//!   (cavekit-types-state-events.md R6).

pub mod config;
pub mod consumers;
pub mod control_socket;
pub mod engine;
pub mod engine_contract;
pub mod events_log;
pub mod orchestrator;
pub mod orchestrator_contract;
pub mod socket_paths;
pub mod status_writer;

pub use config::Config;
pub use consumers::{ReactionDispatcherCtx, reaction_dispatcher, state_writer};
pub use control_socket::{
    ControlListener, Response, gc_stale_socket, handle_single_request, unlink_if_exists,
};
pub use engine::{ApprovalPolicy, Engine, EngineHandle};
pub use engine_contract::engine_contract_suite;
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use orchestrator::{Orchestrator, World};
pub use orchestrator_contract::{OrchestratorFixtures, orchestrator_contract_suite};
pub use socket_paths::{agent_socket_path, ensure_agents_dir, runtime_root};
pub use status_writer::{read_status, write_status_atomic};
