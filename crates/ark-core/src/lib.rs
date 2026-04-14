//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `config` — placeholder `Config` (T-018 fills the schema).
//! - `control_socket` — per-supervisor unix control socket primitive
//!   (cavekit-hook-ipc.md R4, cavekit-supervisor.md R7).
//! - `engine` — `Engine` trait + `EngineHandle` + `ApprovalPolicy`
//!   (cavekit-architecture.md R1).
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant reader
//!   (cavekit-types-state-events.md R7).
//! - `multiplexer` — `Multiplexer` trait
//!   (cavekit-architecture.md R4).
//! - `orchestrator` — `Orchestrator` trait + `World` capability bag
//!   (cavekit-architecture.md R2 + R3).
//! - `socket_paths` — agents-socket-dir + per-agent path helpers
//!   (cavekit-hook-ipc.md R4).
//! - `status_writer` — atomic `status.json` writer/reader
//!   (cavekit-types-state-events.md R6).

pub mod config;
pub mod control_socket;
pub mod engine;
pub mod events_log;
pub mod multiplexer;
pub mod orchestrator;
pub mod socket_paths;
pub mod status_writer;

pub use config::Config;
pub use control_socket::{
    ControlListener, Response, gc_stale_socket, handle_single_request, unlink_if_exists,
};
pub use engine::{ApprovalPolicy, Engine, EngineHandle};
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use multiplexer::Multiplexer;
pub use orchestrator::{Orchestrator, World};
pub use socket_paths::{agent_socket_path, ensure_agents_dir, runtime_root};
pub use status_writer::{read_status, write_status_atomic};
