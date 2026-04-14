//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `config` — placeholder `Config` (T-018 fills the schema).
//! - `engine` — `Engine` trait + `EngineHandle` + `ApprovalPolicy`
//!   (cavekit-architecture.md R1).
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant reader
//!   (cavekit-types-state-events.md R7).
//! - `multiplexer` — `Multiplexer` trait
//!   (cavekit-architecture.md R4).
//! - `orchestrator` — `Orchestrator` trait + `World` capability bag
//!   (cavekit-architecture.md R2 + R3).
//! - `status_writer` — atomic `status.json` writer/reader
//!   (cavekit-types-state-events.md R6).

pub mod config;
pub mod engine;
pub mod events_log;
pub mod multiplexer;
pub mod orchestrator;
pub mod status_writer;

pub use config::Config;
pub use engine::{ApprovalPolicy, Engine, EngineHandle};
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use multiplexer::Multiplexer;
pub use orchestrator::{Orchestrator, World};
pub use status_writer::{read_status, write_status_atomic};
