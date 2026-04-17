//! Supervisor-owned broadcast-bus consumer tasks.
//!
//! The mux-coupled consumers live here (rather than under `ark-core`) so the
//! core crate can stay free of any dependency on the concrete mux type. The
//! only consumer in this module today is [`status_pipe`]; the mux-free
//! consumers (`state_writer`, `reaction_dispatcher`) live in
//! [`ark_core::consumers`].
//!
//! cavekit-soul Phase 1 swap: the rich `AgentEvent` enum is gone. The
//! status pipe consumer now operates on the narrow [`ark_types::CoreEvent`]
//! envelope. Methodology-flavoured signal re-homes inside extensions and
//! rides on `CoreEvent::Ext(ExtEvent)`.

pub mod status_pipe;

pub use status_pipe::status_pipe;

/// Classify a [`ark_types::CoreEvent`] to a stable string slug. Mirrors
/// [`ark_types::FlatEvent::name`] for the core variants.
pub(crate) fn event_kind_slug(event: &ark_types::CoreEvent) -> String {
    use ark_types::CoreEvent::*;
    match event {
        Log { .. } => "log".to_string(),
        Error { .. } => "error".to_string(),
        SessionStarted { .. } => "session_started".to_string(),
        SessionEnded { .. } => "session_ended".to_string(),
        Ext(ev) => format!("{}.{}", ev.ext, ev.kind),
    }
}
