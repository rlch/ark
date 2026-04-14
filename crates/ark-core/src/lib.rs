//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `engine` — `Engine` trait + `EngineHandle` + `ApprovalPolicy`
//!   (cavekit-architecture.md R1).
//! - `orchestrator` — `Orchestrator` trait + `World` stub
//!   (cavekit-architecture.md R2).
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant reader
//!   (cavekit-types-state-events.md R7).

pub mod engine;
pub mod events_log;
pub mod orchestrator;

pub use engine::{ApprovalPolicy, Engine, EngineHandle};
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use orchestrator::{Orchestrator, World};
