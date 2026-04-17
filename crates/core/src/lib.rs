//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `config` — placeholder `Config` (T-018 fills the schema).
//! - `consumers` — supervisor broadcast-bus consumer tasks
//!   (`state_writer`, `reaction_dispatcher`) per
//!   cavekit-soul-phase-1-supervisor.md R2. Soul phase 1 T-025 / T-026
//!   rewrote both against `CoreEvent`.
//! - `control_socket` — per-supervisor unix control socket primitive
//!   (cavekit-hook-ipc.md R4, cavekit-soul-phase-1-supervisor.md R7).
//! - `engine` — `Engine` trait + `EngineHandle` + `ApprovalPolicy`
//!   (cavekit-architecture.md R1).
//! - `engine_contract` — trait-level conformance suite every `Engine`
//!   implementation must pass.
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant
//!   reader (cavekit-soul-phase-1-types.md R7).
//! - `orchestrator` — `Orchestrator` trait + `World` capability bag
//!   (cavekit-soul-phase-1-supervisor.md R8). `World.mux` is
//!   `Arc<ark_mux_zellij::ZellijMux>` (concrete).
//! - `orchestrator_contract` — trait-level conformance suite every
//!   `Orchestrator` implementation must pass.
//! - `socket_paths` — sessions-socket-dir + per-session path helpers
//!   (cavekit-hook-ipc.md R4).
//! - `status_writer` — atomic `status.json` writer/reader
//!   (cavekit-soul-phase-1-types.md R6).

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
pub use engine::{ApprovalPolicy, Engine, EngineHandle};
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use orchestrator::{Orchestrator, World};
pub use status_writer::{read_status, write_session_status_atomic};
