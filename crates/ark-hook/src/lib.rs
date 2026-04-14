//! `ark-hook` sidecar library.
//!
//! See `context/kits/cavekit-hook-ipc.md` R1.
//!
//! T-046 scope (skeleton only):
//! - clap-derive arg parsing (`--id <AgentId>` + `--event <EVENT_NAME>`)
//! - single JSON document read from stdin (kept as `serde_json::Value`;
//!   structured translation lands in T-047)
//! - `<200ms` budget tracked via [`std::time::Instant`] and emitted to
//!   tracing — no artificial cancellation, the budget is a design
//!   constraint not a runtime kill switch (see kit R1)
//! - exit codes: `0` on success **and** on every runtime error path
//!   (fail-open per R3 — stdin, state, pipes must never block claude).
//!   Exit `2` is reserved for exactly two paths:
//!   (a) clap argument-validation failure at launch — a setup-time bug
//!   in the engine-injected hook config and must be loud;
//!   (b) future explicit `PermissionRequest` deny (T-050).
//!   The skeleton itself never produces `2`; clap's own parse-failure
//!   path is the only way this crate exits non-zero today.
//! - all logs go to stderr; stdout is reserved for the future
//!   `PermissionRequest` payload (T-050)
//!
//! Modules are intentionally narrow so downstream tasks can plug in:
//! - T-047 → payload parser (replace `Value` with typed struct)
//! - T-048 → JSONL writers
//! - T-049 → zellij/picker pipe forwarders
//! - T-050 → PermissionRequest stdout writer + explicit-deny exit 2
//! - T-051 → expanded fail-open behavior

pub mod cli;
pub mod event;
pub mod payload;
pub mod run;

pub use cli::Cli;
pub use event::HookEvent;
pub use payload::{FILE_EDIT_TOOLS, HookPayload, SUMMARY_MAX_CHARS, payload_to_events};
pub use run::{HOOK_BUDGET_MS, run};
