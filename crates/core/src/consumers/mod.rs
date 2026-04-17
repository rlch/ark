//! Supervisor consumer tasks attached to the broadcast event bus.
//!
//! Soul phase 1 T-025 / T-026 — mux-free consumer tasks that subscribe to the
//! supervisor's `tokio::sync::broadcast::Sender<CoreEvent>`:
//!
//! - [`state_writer`] — maintains `spec.json` + `status.json` +
//!   `events.jsonl` per session per cavekit-soul-phase-1-types.md
//!   R1/R4/R6.
//! - [`reaction_dispatcher`] — fires scene reactions on matching events
//!   and routes the resulting [`ark_scene::ast::ops::OpNode`] list
//!   through the [`ark_scene::intent::IntentRegistry`] (with `Emit`
//!   publishing on the bus as [`ark_types::CoreEvent::Ext`] and
//!   `SetStatus` writing into `SessionStatus::ext_state`).
//!
//! A third consumer, `status_pipe`, lives in `ark-supervisor` so
//! `ark-core` has no dependency on the concrete mux type.
//!
//! All consumers are resilient to `RecvError::Lagged(n)` (warn-log +
//! continue), exit cleanly on `RecvError::Closed`, and honor a
//! `tokio_util::sync::CancellationToken` for supervisor-driven shutdown.

pub mod reaction_dispatcher;
pub mod state_writer;

pub use reaction_dispatcher::{ReactionDispatcherCtx, reaction_dispatcher};
pub use state_writer::state_writer;
