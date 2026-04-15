//! Supervisor consumer tasks attached to the broadcast event bus.
//!
//! Implements cavekit-supervisor.md R2 — mux-free consumer tasks that
//! subscribe to the supervisor's `tokio::sync::broadcast::Sender<AgentEvent>`:
//!
//! - [`state_writer`] — appends every event to `events.jsonl` and rolls up
//!   `status.json`; emits `PhaseTransition` on actual phase change.
//! - [`hook_dispatcher`] — fires user-configured `[[hooks]]` commands on
//!   matching events, detached, with a 30s timeout each.
//!
//! The third consumer, `status_pipe`, lives in `ark-supervisor` so `ark-core`
//! has no dependency on the concrete mux type. See
//! `ark_supervisor::consumers::status_pipe`.
//!
//! Both consumers are resilient to `RecvError::Lagged(n)` (warn-log +
//! continue), exit cleanly on `RecvError::Closed`, and honor a
//! `tokio_util::sync::CancellationToken` for supervisor-driven shutdown.

pub mod hook_dispatcher;
pub mod reaction_dispatcher;
pub mod state_writer;

pub use hook_dispatcher::hook_dispatcher;
pub use reaction_dispatcher::{reaction_dispatcher, ReactionDispatcherCtx};
pub use state_writer::state_writer;

/// Shared helper: classify an `AgentEvent` to its serde tag slug used by
/// hooks (`on_event = ["done", "stall", ...]`).
///
/// Mirrors `#[serde(tag = "kind", rename_all = "snake_case")]` on
/// [`ark_types::AgentEvent`]. `ark-supervisor`'s `status_pipe` mirrors the
/// equivalent in its own consumers module.
pub(crate) fn event_kind_slug(event: &ark_types::AgentEvent) -> &'static str {
    use ark_types::AgentEvent::*;
    match event {
        Started { .. } => "started",
        TabOpened { .. } => "tab_opened",
        TabClosed { .. } => "tab_closed",
        Progress { .. } => "progress",
        TaskDone { .. } => "task_done",
        Iteration { .. } => "iteration",
        PhaseTransition { .. } => "phase_transition",
        ToolUse { .. } => "tool_use",
        Message { .. } => "message",
        FileEdited { .. } => "file_edited",
        ReviewComment { .. } => "review_comment",
        PermissionAsked { .. } => "permission_asked",
        PermissionResolved { .. } => "permission_resolved",
        Stall { .. } => "stall",
        Log { .. } => "log",
        Error { .. } => "error",
        Done { .. } => "done",
        // Catch-all for the `#[non_exhaustive]` enum.
        _ => "unknown",
    }
}

/// Shared helper: extract the originating `AgentId` for events that carry one.
/// `Started` carries an `AgentSpec` rather than a bare id, so we reach into it.
pub(crate) fn event_agent_id(event: &ark_types::AgentEvent) -> Option<&ark_types::AgentId> {
    use ark_types::AgentEvent::*;
    match event {
        Started { spec } => Some(&spec.id),
        TabOpened { id, .. }
        | TabClosed { id, .. }
        | Progress { id, .. }
        | TaskDone { id, .. }
        | Iteration { id, .. }
        | PhaseTransition { id, .. }
        | ToolUse { id, .. }
        | Message { id, .. }
        | FileEdited { id, .. }
        | ReviewComment { id, .. }
        | PermissionAsked { id, .. }
        | PermissionResolved { id, .. }
        | Stall { id, .. }
        | Log { id, .. }
        | Error { id, .. }
        | Done { id, .. } => Some(id),
        _ => None,
    }
}

/// Shared helper: extract a severity slug from events that carry one,
/// for hook `on_severity` filters.
pub(crate) fn event_severity_slug(event: &ark_types::AgentEvent) -> Option<String> {
    use ark_types::AgentEvent::*;
    match event {
        ReviewComment { severity, .. } => Some(match severity {
            ark_types::Severity::P0 => "P0",
            ark_types::Severity::P1 => "P1",
            ark_types::Severity::P2 => "P2",
            ark_types::Severity::P3 => "P3",
        })
        .map(String::from),
        _ => None,
    }
}
