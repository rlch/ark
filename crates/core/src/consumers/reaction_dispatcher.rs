//! `reaction_dispatcher` consumer task (T-5.3).
//!
//! Replacement path for `hook_dispatcher` (T-5.7 removes the old one).
//! This consumer subscribes to the supervisor's
//! `tokio::sync::broadcast::Sender<AgentEvent>`, and for every event:
//!
//! 1. Classifies the event via [`EventKind::of`] (T-5.1).
//! 2. Looks up reactions in the primary `ReactionRegistry` index by
//!    kind; for `UserEvent`, additionally unions in the secondary
//!    index by the event's `name` field.
//! 3. Evaluates each candidate's parsed [`EventSelector`] matcher
//!    (T-5.2) against the live event.
//! 4. Evaluates each candidate's optional CEL `if=` predicate (T-2.1 /
//!    T-2.2) against a context built from the event + agent + session
//!    snapshots.
//! 5. Dispatches the reaction's op list through
//!    [`dispatch_sequence`](ark_scene::ops::dispatch::dispatch_sequence)
//!    (T-4.5). Op failures are absorbed here so the event loop keeps
//!    running.
//!
//! Coexists with the legacy `hook_dispatcher` until T-5.7 migrates hook
//! config into a synthetic scene fragment. Both consumers are attached
//! to the same broadcast bus; neither interferes with the other.
//!
//! Resilient to `RecvError::Lagged(n)` (warn-log + continue), exits on
//! `RecvError::Closed`, honors a `tokio_util::sync::CancellationToken`
//! for supervisor-driven shutdown.

use std::sync::Arc;

use anyhow::Result;
use ark_scene::cel::{self, Context as CelContext};
use ark_scene::context::{AgentSnapshot, SessionSnapshot, build_context};
use ark_scene::intent::{IntentContext, IntentRegistry};
use ark_scene::ops::dispatch::dispatch_sequence;
use ark_scene::reactions::{EventKind, ReactionEntry, ReactionRegistry};
use ark_scene::selector::parse_selector;
use ark_types::AgentEvent;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Handle bundling the inputs `reaction_dispatcher` needs to dispatch
/// ops: the compiled [`ReactionRegistry`], the [`IntentRegistry`] the
/// scene has registered ops against, and the CEL context snapshots
/// (agent + session). Each field is `Arc`-wrapped so the dispatcher can
/// cheaply clone the bundle into per-event tasks if a future tier
/// chooses to fan out.
#[derive(Clone)]
pub struct ReactionDispatcherCtx {
    /// Compiled reactions — keyed by EventKind + UserEvent:name.
    pub reactions: Arc<ReactionRegistry>,

    /// Op dispatch surface registered with the core op set (`ark.core.*`)
    /// plus any extension ops contributed by `use` declarations.
    pub intents: IntentRegistry,

    /// Intent context handed to every op dispatch (mux / bus / supervisor
    /// handles + scene identity + reaction origin). Cloned per-event so
    /// per-reaction overrides (e.g. cascade-depth telemetry in T-5.4)
    /// don't leak across dispatches.
    pub intent_ctx: IntentContext,

    /// Agent snapshot fed into CEL's `agent.*` binding (T-2.2).
    pub agent: Arc<AgentSnapshot>,

    /// Session snapshot fed into CEL's `session.*` binding (T-2.2).
    pub session: Arc<SessionSnapshot>,
}

/// Long-running consumer task. See module docs.
pub async fn reaction_dispatcher(
    mut rx: Receiver<AgentEvent>,
    ctx: ReactionDispatcherCtx,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("reaction_dispatcher: cancel fired, exiting");
                return Ok(());
            }
            recv = rx.recv() => match recv {
                Ok(event) => {
                    dispatch_event(&event, &ctx).await;
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "reaction_dispatcher: broadcast lagged; continuing");
                    continue;
                }
                Err(RecvError::Closed) => {
                    debug!("reaction_dispatcher: broadcast closed, exiting");
                    return Ok(());
                }
            }
        }
    }
}

/// Evaluate every candidate reaction for the given event and dispatch
/// matching ops. Isolated from the task loop so tests can drive it
/// synchronously.
pub async fn dispatch_event(event: &AgentEvent, ctx: &ReactionDispatcherCtx) {
    let kind = EventKind::of(event);

    // Assemble candidate reactions. Primary index always; secondary
    // index for UserEvent to avoid linear-scanning UserEvent reactions.
    let mut candidates: Vec<&ReactionEntry> = ctx.reactions.by_kind(&kind).iter().collect();
    if let AgentEvent::UserEvent { name, .. } = event {
        // Dedupe: the secondary-index entries already live in the
        // primary UserEvent slot. We iterate only the secondary-index
        // matches and skip entries whose selector we've already seen.
        // For small reaction counts (the common case) this dedupe cost
        // is irrelevant; for pathological scenes with hundreds of
        // UserEvent reactions we want the secondary index to prune.
        let sec = ctx.reactions.by_user_event_name(name);
        // Replace the primary list with the (narrower) secondary one
        // for UserEvent:<name> dispatches — primary-only entries
        // (`on "UserEvent"` bare) are dropped because they'd fire for
        // every UserEvent name, which the spec doesn't require but
        // also doesn't forbid. At this tier we keep the narrow
        // interpretation so secondary-index reactions get exclusive
        // treatment; if a scene needs the broad form, T-5.2 selectors
        // allow a `UserEvent name="*"` pattern against the primary
        // index.
        //
        // TODO(post-v1): revisit whether bare `on "UserEvent"` should
        // union with secondary-indexed reactions. Leaving them
        // disjoint for now matches the spec's emphasis on namespaced
        // UserEvent names.
        candidates = sec.iter().collect();
    }

    if candidates.is_empty() {
        return;
    }

    // Build CEL context once per event (shared across all reactions'
    // `if=` predicates).
    let cel_ctx = match build_context(event, None, ctx.agent.as_ref(), ctx.session.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                error = %e,
                kind = kind.as_str(),
                "reaction_dispatcher: failed to build CEL context; skipping event"
            );
            return;
        }
    };

    for entry in candidates {
        if !reaction_matches(entry, event) {
            continue;
        }
        if !predicate_passes(entry, &cel_ctx) {
            continue;
        }
        // Dispatch the op list. Errors are absorbed — T-4.5 already
        // logs under target=scene::ops; the event loop continues.
        if let Err(err) = dispatch_sequence(&entry.ops, &ctx.intents, &ctx.intent_ctx).await {
            warn!(
                selector = %entry.selector,
                error = %err,
                "reaction_dispatcher: op dispatch failed; continuing"
            );
        }
    }
}

/// Parse the entry's stored selector string and run the T-5.2 matcher
/// against the live event. Parse failures are treated as no-match and
/// warn-logged; the scene compile pipeline should have already rejected
/// malformed selectors at `ark scene check`.
fn reaction_matches(entry: &ReactionEntry, event: &AgentEvent) -> bool {
    match parse_selector(&entry.selector) {
        Ok(sel) => sel.matches(event),
        Err(e) => {
            warn!(
                selector = %entry.selector,
                error = %e,
                "reaction_dispatcher: selector parse failed at runtime; ignoring reaction \
                 (should have been caught at scene check)"
            );
            false
        }
    }
}

/// Evaluate the entry's optional CEL predicate. `None` = no predicate =
/// pass. Eval failures are warn-logged and treated as "skip reaction".
fn predicate_passes(entry: &ReactionEntry, cel_ctx: &CelContext<'_>) -> bool {
    match &entry.predicate {
        None => true,
        Some(program) => match cel::eval_bool(program.as_ref(), cel_ctx) {
            Ok(true) => true,
            Ok(false) => false,
            Err(e) => {
                warn!(
                    selector = %entry.selector,
                    error = %e,
                    "reaction_dispatcher: CEL predicate eval failed; skipping reaction"
                );
                false
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_scene::cel;
    use ark_scene::id::SceneId;
    use ark_scene::intent::{IntentContext, IntentRegistry};
    use ark_scene::ops::dispatch::CompiledOp;
    use ark_scene::ops::register_core_ops;
    use ark_scene::ops::Idempotency;
    use ark_scene::reactions::{EventKind, ReactionEntry, ReactionRegistry};
    use ark_types::AgentEvent;
    use ark_types::event::LogLevel;
    use ark_types::id::AgentId;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn agent_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    fn agent_snap() -> AgentSnapshot {
        AgentSnapshot {
            id: agent_id().to_string(),
            name: "builder".into(),
            orchestrator: "cavekit".into(),
            engine: "claude-code".into(),
            cwd: "/tmp/worktree".into(),
            cmd: "claude".into(),
            args: vec!["--resume".into()],
        }
    }

    fn session_snap() -> SessionSnapshot {
        SessionSnapshot {
            name: "ark-cavekit-auth".into(),
        }
    }

    fn intent_ctx() -> IntentContext {
        IntentContext::placeholder(SceneId::from_bytes(
            PathBuf::from("/tmp/scene.kdl"),
            b"scene \"x\" { }",
        ))
    }

    fn emit_op(name: &str) -> CompiledOp {
        let src = format!(r#"emit "{name}""#);
        // Parse via the same KDL surface scene uses — exposed transitively
        // through ark_scene's `kdl` dep.
        let doc: ::kdl::KdlDocument = src.parse().expect("kdl");
        let node = doc.nodes().first().cloned().expect("node");
        CompiledOp::new("ark.core.emit", Idempotency::AlwaysSideEffect, node)
    }

    async fn fresh_ctx(reactions: ReactionRegistry) -> ReactionDispatcherCtx {
        let intents = IntentRegistry::new();
        register_core_ops(&intents).await;
        ReactionDispatcherCtx {
            reactions: Arc::new(reactions),
            intents,
            intent_ctx: intent_ctx(),
            agent: Arc::new(agent_snap()),
            session: Arc::new(session_snap()),
        }
    }

    // -- happy path: matching kind fires the op ---------------------------

    #[tokio::test]
    async fn matching_kind_selector_dispatches_ops() {
        let mut registry = ReactionRegistry::new();
        let entry = ReactionEntry {
            selector: "Log".into(),
            predicate: None,
            ops: vec![emit_op("user.fired")],
            origin: Default::default(),
        };
        registry.insert(EventKind::Log, None, entry);

        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "hello".into(),
        };
        dispatch_event(&ev, &ctx).await;

        let drained = ctx.intent_ctx.bus.drain_user_events();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            AgentEvent::UserEvent { name, .. } => assert_eq!(name, "user.fired"),
            other => panic!("expected UserEvent, got {other:?}"),
        }
    }

    // -- mismatched kind: no dispatch -------------------------------------

    #[tokio::test]
    async fn non_matching_kind_drops() {
        let mut registry = ReactionRegistry::new();
        let entry = ReactionEntry {
            selector: "Started".into(),
            predicate: None,
            ops: vec![emit_op("user.should_not_fire")],
            origin: Default::default(),
        };
        registry.insert(EventKind::Started, None, entry);

        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        dispatch_event(&ev, &ctx).await;
        assert!(ctx.intent_ctx.bus.drain_user_events().is_empty());
    }

    // -- field-pattern selector narrows matches ---------------------------

    #[tokio::test]
    async fn field_pattern_selector_gates_dispatch() {
        let mut registry = ReactionRegistry::new();
        let entry = ReactionEntry {
            selector: r#"PhaseTransition to="review""#.into(),
            predicate: None,
            ops: vec![emit_op("user.review_ready")],
            origin: Default::default(),
        };
        registry.insert(EventKind::PhaseTransition, None, entry);

        let ctx = fresh_ctx(registry).await;

        // Non-match: wrong phase.
        dispatch_event(
            &AgentEvent::PhaseTransition {
                id: agent_id(),
                from: None,
                to: "running".into(),
            },
            &ctx,
        )
        .await;
        assert!(ctx.intent_ctx.bus.drain_user_events().is_empty());

        // Match.
        dispatch_event(
            &AgentEvent::PhaseTransition {
                id: agent_id(),
                from: None,
                to: "review".into(),
            },
            &ctx,
        )
        .await;
        assert_eq!(ctx.intent_ctx.bus.drain_user_events().len(), 1);
    }

    // -- CEL predicate gates dispatch -------------------------------------

    #[tokio::test]
    async fn cel_predicate_false_skips_reaction() {
        let mut registry = ReactionRegistry::new();
        let prog = cel::compile(r#"event.level == "error""#, "<if>", 0).expect("cel");
        let entry = ReactionEntry {
            selector: "Log".into(),
            predicate: Some(Arc::new(prog)),
            ops: vec![emit_op("user.error_seen")],
            origin: Default::default(),
        };
        registry.insert(EventKind::Log, None, entry);

        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "nothing".into(),
        };
        dispatch_event(&ev, &ctx).await;
        assert!(ctx.intent_ctx.bus.drain_user_events().is_empty());
    }

    #[tokio::test]
    async fn cel_predicate_true_fires_reaction() {
        let mut registry = ReactionRegistry::new();
        let prog = cel::compile(r#"event.level == "error""#, "<if>", 0).expect("cel");
        let entry = ReactionEntry {
            selector: "Log".into(),
            predicate: Some(Arc::new(prog)),
            ops: vec![emit_op("user.error_seen")],
            origin: Default::default(),
        };
        registry.insert(EventKind::Log, None, entry);

        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Error,
            line: "boom".into(),
        };
        dispatch_event(&ev, &ctx).await;
        assert_eq!(ctx.intent_ctx.bus.drain_user_events().len(), 1);
    }

    // -- secondary UserEvent index --------------------------------------

    #[tokio::test]
    async fn user_event_secondary_index_narrows_dispatch() {
        let mut registry = ReactionRegistry::new();
        // Two reactions on different user-event names.
        registry.insert(
            EventKind::UserEvent,
            Some("user.hello".into()),
            ReactionEntry {
                selector: "UserEvent:user.hello".into(),
                predicate: None,
                ops: vec![emit_op("user.hello_echo")],
                origin: Default::default(),
            },
        );
        registry.insert(
            EventKind::UserEvent,
            Some("user.world".into()),
            ReactionEntry {
                selector: "UserEvent:user.world".into(),
                predicate: None,
                ops: vec![emit_op("user.world_echo")],
                origin: Default::default(),
            },
        );

        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::UserEvent {
            name: "user.hello".into(),
            payload: serde_json::Value::Null,
            source: "scene".into(),
        };
        dispatch_event(&ev, &ctx).await;
        let drained = ctx.intent_ctx.bus.drain_user_events();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            AgentEvent::UserEvent { name, .. } => assert_eq!(name, "user.hello_echo"),
            other => panic!("expected UserEvent, got {other:?}"),
        }
    }

    // -- multiple matching reactions all fire -----------------------------

    #[tokio::test]
    async fn overlapping_reactions_all_fire() {
        let mut registry = ReactionRegistry::new();
        for suffix in ["a", "b", "c"] {
            registry.insert(
                EventKind::Log,
                None,
                ReactionEntry {
                    selector: "Log".into(),
                    predicate: None,
                    ops: vec![emit_op(&format!("user.{suffix}"))],
                    origin: Default::default(),
                },
            );
        }
        let ctx = fresh_ctx(registry).await;
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        dispatch_event(&ev, &ctx).await;
        assert_eq!(ctx.intent_ctx.bus.drain_user_events().len(), 3);
    }

    // -- cancellation shuts the consumer down cleanly --------------------

    #[tokio::test]
    async fn cancel_exits_cleanly() {
        use tokio::sync::broadcast;
        let (tx, rx) = broadcast::channel::<AgentEvent>(4);
        let ctx = fresh_ctx(ReactionRegistry::new()).await;
        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move { reaction_dispatcher(rx, ctx, cancel).await })
        };
        drop(tx); // no traffic
        cancel.cancel();
        let res = handle.await.expect("join");
        assert!(res.is_ok());
    }
}
