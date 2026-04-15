//! Supervisor-owned broadcast-bus consumer tasks.
//!
//! The mux-coupled consumers live here (rather than under `ark-core`) so the
//! core crate can stay free of any dependency on the concrete mux type. The
//! only consumer in this module today is [`status_pipe`]; the mux-free
//! consumers (`state_writer`, `reaction_dispatcher`) live in
//! [`ark_core::consumers`]. T-5.7 deleted the standalone `hook_dispatcher`
//! consumer that used to ship alongside `state_writer`.
//!
//! Relocated in the mux tight-coupling revision (Wave B, task M-9). See
//! `context/impl/impl-mux-tight-coupling.md` for the decision record.

pub mod status_pipe;

pub use status_pipe::status_pipe;

/// Classify an `AgentEvent` to its serde tag slug. Mirrors the
/// `pub(crate) fn event_kind_slug` helper in `ark_core::consumers` so this
/// module can stay tree-local without re-exporting from `ark-core`.
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
