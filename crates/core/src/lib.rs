//! ark-core — shared traits and runtime primitives for ark.
//!
//! - `config` — placeholder `Config` (T-018 fills the schema).
//! - `consumers` — supervisor broadcast-bus consumer tasks
//!   (`state_writer`, `reaction_dispatcher`) per
//!   cavekit-soul-phase-1-supervisor.md R2. Soul phase 1 T-025 / T-026
//!   rewrote both against `CoreEvent`.
//! - `control_socket` — per-supervisor unix control socket primitive
//!   (cavekit-hook-ipc.md R4, cavekit-soul-phase-1-supervisor.md R7).
//! - `events_log` — `events.jsonl` append writer + corruption-tolerant
//!   reader (cavekit-soul-phase-1-types.md R7).
//! - `socket_paths` — sessions-socket-dir + per-session path helpers
//!   (cavekit-hook-ipc.md R4).
//! - `status_writer` — atomic `status.json` writer/reader
//!   (cavekit-soul-phase-1-types.md R6).
//!
//! cleanup-T-010: the `Engine` + `Orchestrator` trait families
//! (`engine`, `engine_contract`, `orchestrator`, `orchestrator_contract`
//! modules) were deleted — the supervisor boot path no longer carries
//! trait objects, and per-engine behaviour re-homed into extensions.

pub mod config;
pub mod consumers;
pub mod control_socket;
pub mod events_log;
pub mod socket_paths;
pub mod status_writer;

pub use config::Config;
pub use events_log::{EventLogHandle, EventLogReader, EventLogWriter, EventRecord};
pub use status_writer::{read_status, write_session_status_atomic};
