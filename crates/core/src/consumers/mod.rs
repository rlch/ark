//! Supervisor consumer tasks attached to the broadcast event bus.
//!
//! Implements cavekit-supervisor.md R2 — mux-free consumer tasks that
//! subscribe to the supervisor's `tokio::sync::broadcast::Sender<AgentEvent>`:
//!
//! - [`state_writer`] — appends every event to `events.jsonl` and rolls up
//!   `status.json`; emits `PhaseTransition` on actual phase change.
//! - [`reaction_dispatcher`] — fires scene reactions on matching events.
//!   The legacy `[[hooks]]` TOML config is compiled into a synthetic
//!   `ReactionRegistry` (via `ark_scene::hook_compat`, T-5.7) so the same
//!   dispatcher handles both user-scene reactions and legacy hooks.
//!
//! The third consumer, `status_pipe`, lives in `ark-supervisor` so `ark-core`
//! has no dependency on the concrete mux type. See
//! `ark_supervisor::consumers::status_pipe`.
//!
//! ## Migration history
//!
//! Pre-T-5.7 ark shipped a dedicated `hook_dispatcher` consumer that ran
//! `[[hooks]]` commands directly via `Command::spawn`. T-5.7 deleted that
//! consumer; the supervisor now translates legacy hooks into scene
//! reactions tagged `ReactionOrigin::HookConfig` so a single
//! `reaction_dispatcher` covers both code paths uniformly.
//!
//! All consumers are resilient to `RecvError::Lagged(n)` (warn-log +
//! continue), exit cleanly on `RecvError::Closed`, and honor a
//! `tokio_util::sync::CancellationToken` for supervisor-driven shutdown.

pub mod reaction_dispatcher;
pub mod state_writer;

pub use reaction_dispatcher::{ReactionDispatcherCtx, reaction_dispatcher};
pub use state_writer::state_writer;

// Note: the `event_kind_slug` / `event_agent_id` / `event_severity_slug`
// helpers that lived here pre-T-5.7 were exclusive to the deleted
// `hook_dispatcher` consumer. The reaction_dispatcher (T-5.3) computes
// kind / severity directly off the live `AgentEvent` via
// `ark_scene::reactions::EventKind` and `ark_scene::context::build_context`,
// so the helpers had no remaining call sites. `ark_supervisor`'s
// `status_pipe` carries its own copy of the kind-slug helper.
