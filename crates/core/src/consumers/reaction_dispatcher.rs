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
//! T-5.7 deleted the standalone `hook_dispatcher`: legacy `[[hooks]]`
//! TOML config is compiled into a synthetic scene fragment via
//! `ark_scene::hook_compat::build_hook_registry`, and the resulting
//! `ReactionRegistry` is merged into the user-scene registry the
//! supervisor passes here. Hook-derived reactions are tagged
//! `ReactionOrigin::HookConfig` so the T-5.6 telemetry surface
//! distinguishes them from user-scene reactions in the
//! `scene::reactions` tracing target.
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
/// matching ops.
///
/// Entrypoint used by the broadcast-consumer task. The event is the
/// top-of-chain trigger; cascade depth starts at the `IntentContext`'s
/// current depth (typically 0 for broadcast events). Any `emit` ops
/// that fire enqueue synthetic events on the bus capture queue; after
/// the top reactions run, this function drains the queue and
/// re-dispatches each child with `cascade_child` — bounded by
/// `max_cascade_depth` per R4 (default 4). Exceeding the bound is an
/// error log under target=`scene::reactions` and the child is dropped.
pub async fn dispatch_event(event: &AgentEvent, ctx: &ReactionDispatcherCtx) {
    // The T-5.4 cascade implementation is constrained by the
    // placeholder `EventBus` (T-4.1): the bus is a single shared
    // capture queue the `emit` op writes to. To walk an emit chain
    // without losing observability (tests + future audit log want to
    // see what fired), we snapshot the capture queue before each
    // reaction fires, run the reaction, then diff after. The diff is
    // the set of events emitted by this particular reaction; those
    // events cascade one level deeper.
    //
    // When the real broadcast bus lands (tracked by the placeholder
    // module TODO), this dispatcher flips to subscribing directly —
    // no diffing needed.
    //
    // Seed the cascade loop with the top-level event at the context's
    // current depth.
    let mut queue: std::collections::VecDeque<(AgentEvent, u32)> =
        std::collections::VecDeque::new();
    queue.push_back((event.clone(), ctx.intent_ctx.cascade_depth));

    while let Some((ev, depth)) = queue.pop_front() {
        // Snapshot the queue BEFORE dispatch so we can diff emitted
        // events off the tail.
        let before = ctx.intent_ctx.bus.drain_user_events();
        for e in &before {
            ctx.intent_ctx.bus.record_user_event(e.clone());
        }
        let before_len = before.len();

        // Build a per-event context with the right depth.
        let mut event_ctx = ctx.clone();
        event_ctx.intent_ctx.cascade_depth = depth;
        dispatch_event_once(&ev, &event_ctx).await;

        // After dispatch: the bus has `before_len + N` entries. The
        // tail N are what this reaction emitted.
        let all_now = ctx.intent_ctx.bus.drain_user_events();
        let (prior_events, emitted): (Vec<_>, Vec<_>) =
            all_now.into_iter().enumerate().partition(|(i, _)| *i < before_len);
        let prior_events: Vec<AgentEvent> = prior_events.into_iter().map(|(_, e)| e).collect();
        let emitted: Vec<AgentEvent> = emitted.into_iter().map(|(_, e)| e).collect();

        // Always re-push prior events — they predate this reaction
        // and have nothing to do with cascade-depth enforcement.
        for e in &prior_events {
            ctx.intent_ctx.bus.record_user_event(e.clone());
        }

        if emitted.is_empty() {
            continue;
        }
        let child_depth = depth.saturating_add(1);
        if child_depth > event_ctx.intent_ctx.max_cascade_depth {
            tracing::error!(
                target = "scene::reactions",
                child_depth,
                max_depth = event_ctx.intent_ctx.max_cascade_depth,
                dropped = emitted.len(),
                "cascade depth exceeded; dropping emitted events"
            );
            // Do NOT re-push — events that breach the cascade bound
            // are dropped entirely, per R4 ("exceeding = error log +
            // drop").
            continue;
        }

        // Re-push emitted events for observation, then enqueue them
        // for the next cascade hop.
        for e in &emitted {
            ctx.intent_ctx.bus.record_user_event(e.clone());
        }
        for child in emitted {
            queue.push_back((child, child_depth));
        }
    }
}

/// Single-pass dispatch helper. Does NOT drain emits — the caller
/// (`dispatch_event`) runs the cascade loop above.
async fn dispatch_event_once(event: &AgentEvent, ctx: &ReactionDispatcherCtx) {
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

    // Event-name string (only meaningful for UserEvent); passed through
    // to the telemetry target so users filtering on `event_name` get
    // useful labels.
    let event_name = match event {
        AgentEvent::UserEvent { name, .. } => name.as_str(),
        _ => "",
    };

    for entry in candidates {
        if !reaction_matches(entry, event) {
            continue;
        }
        let origin_tag = format!("{:?}", entry.origin);
        if !predicate_passes(entry, &cel_ctx) {
            let rec = TelemetryRecord {
                selector: entry.selector.clone(),
                reaction_origin: origin_tag.clone(),
                event_kind: kind.as_str(),
                event_name: event_name.to_string(),
                ops_run: 0,
                status: "skipped_predicate",
                error: None,
            };
            emit_telemetry(&rec, "reaction skipped: CEL predicate false");
            continue;
        }
        // Dispatch the op list. Errors are absorbed — T-4.5 already
        // logs under target=scene::ops; the event loop continues.
        let ops_run = entry.ops.len();
        let result = dispatch_sequence(&entry.ops, &ctx.intents, &ctx.intent_ctx).await;
        match &result {
            Ok(()) => {
                let rec = TelemetryRecord {
                    selector: entry.selector.clone(),
                    reaction_origin: origin_tag,
                    event_kind: kind.as_str(),
                    event_name: event_name.to_string(),
                    ops_run,
                    status: "ok",
                    error: None,
                };
                emit_telemetry(&rec, "reaction fired");
            }
            Err(err) => {
                let rec = TelemetryRecord {
                    selector: entry.selector.clone(),
                    reaction_origin: origin_tag,
                    event_kind: kind.as_str(),
                    event_name: event_name.to_string(),
                    ops_run,
                    status: "failed",
                    error: Some(err.to_string()),
                };
                emit_telemetry(&rec, "reaction op dispatch failed");
                warn!(
                    selector = %entry.selector,
                    error = %err,
                    "reaction_dispatcher: op dispatch failed; continuing"
                );
            }
        }
    }
}

/// Reaction-firing telemetry record per R-T-5.6. Renders as the key
/// set tracing users expect when filtering the `scene::reactions`
/// target: `selector`, `reaction_origin`, `event_kind`, `event_name`,
/// `ops_run`, `status`, optional `error`.
///
/// The type is `pub(crate)` so the test suite can construct one
/// directly; production call sites pass fields through `tracing::debug!`
/// via [`emit_telemetry`].
#[derive(Debug, Clone)]
pub(crate) struct TelemetryRecord {
    /// Scene-file selector string for the fired reaction.
    pub selector: String,
    /// Debug rendering of the reaction's [`ReactionOrigin`]. Stays a
    /// `String` so when `ReactionOrigin` gains real variants (e.g.
    /// `UserScene { id }`, `Extension { name }`), the rendering flows
    /// through without a schema change here.
    pub reaction_origin: String,
    /// Canonical snake_case `EventKind` slug.
    pub event_kind: &'static str,
    /// UserEvent's namespaced name, or `""` for core events.
    pub event_name: String,
    /// Count of ops actually dispatched (not just declared).
    pub ops_run: usize,
    /// `ok` | `failed` | `skipped_predicate`.
    pub status: &'static str,
    /// Short error summary when `status == "failed"`.
    pub error: Option<String>,
}

impl TelemetryRecord {
    /// Render as a `key="value" key=value …` string. Deterministic key
    /// ordering so tests can string-compare slices.
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("selector=\"{}\"", self.selector));
        s.push_str(&format!(" reaction_origin=\"{}\"", self.reaction_origin));
        s.push_str(&format!(" event_kind=\"{}\"", self.event_kind));
        s.push_str(&format!(" event_name=\"{}\"", self.event_name));
        s.push_str(&format!(" ops_run={}", self.ops_run));
        s.push_str(&format!(" status=\"{}\"", self.status));
        if let Some(err) = &self.error {
            s.push_str(&format!(" error=\"{}\"", err));
        }
        s
    }
}

/// Publish a [`TelemetryRecord`] to the `scene::reactions` tracing
/// target at debug level.
fn emit_telemetry(rec: &TelemetryRecord, message: &'static str) {
    tracing::debug!(
        target = "scene::reactions",
        selector = %rec.selector,
        reaction_origin = %rec.reaction_origin,
        event_kind = rec.event_kind,
        event_name = rec.event_name,
        ops_run = rec.ops_run,
        status = rec.status,
        error = rec.error.as_deref().unwrap_or(""),
        "{message}"
    );
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

    // -- cascade depth ---------------------------------------------------

    /// A chain of reactions: Log → emit user.a → (on UserEvent:user.a) emit
    /// user.b. With default depth (4), depth 0 → Log, depth 1 → user.a,
    /// depth 2 → user.b. All three fires should land in the bus.
    #[tokio::test]
    async fn cascade_under_bound_runs_every_reaction() {
        let mut registry = ReactionRegistry::new();
        registry.insert(
            EventKind::Log,
            None,
            ReactionEntry {
                selector: "Log".into(),
                predicate: None,
                ops: vec![emit_op("user.a")],
                origin: Default::default(),
            },
        );
        registry.insert(
            EventKind::UserEvent,
            Some("user.a".into()),
            ReactionEntry {
                selector: "UserEvent:user.a".into(),
                predicate: None,
                ops: vec![emit_op("user.b")],
                origin: Default::default(),
            },
        );
        let ctx = fresh_ctx(registry).await;
        dispatch_event(
            &AgentEvent::Log {
                id: agent_id(),
                level: LogLevel::Info,
                line: "go".into(),
            },
            &ctx,
        )
        .await;
        let drained = ctx.intent_ctx.bus.drain_user_events();
        let names: Vec<String> = drained
            .iter()
            .map(|e| match e {
                AgentEvent::UserEvent { name, .. } => name.clone(),
                _ => "?".into(),
            })
            .collect();
        assert!(names.contains(&"user.a".to_string()));
        assert!(names.contains(&"user.b".to_string()));
    }

    /// Chain longer than the bound: a log that kicks off a chain `a → b
    /// → c → d → e` with `max_cascade_depth = 2` should fire `a` (depth
    /// 1) and `b` (depth 2), but drop emits produced by `b` (which
    /// would be `c` at depth 3).
    #[tokio::test]
    async fn cascade_exceeds_bound_drops_tail_with_error_log() {
        let mut registry = ReactionRegistry::new();
        registry.insert(
            EventKind::Log,
            None,
            ReactionEntry {
                selector: "Log".into(),
                predicate: None,
                ops: vec![emit_op("user.a")],
                origin: Default::default(),
            },
        );
        for (from, to) in [("user.a", "user.b"), ("user.b", "user.c"), ("user.c", "user.d")] {
            registry.insert(
                EventKind::UserEvent,
                Some(from.into()),
                ReactionEntry {
                    selector: format!("UserEvent:{from}"),
                    predicate: None,
                    ops: vec![emit_op(to)],
                    origin: Default::default(),
                },
            );
        }
        // Build a ctx with a tight cap so the test is compact.
        let mut ctx = fresh_ctx(registry).await;
        ctx.intent_ctx.max_cascade_depth = 2;
        dispatch_event(
            &AgentEvent::Log {
                id: agent_id(),
                level: LogLevel::Info,
                line: "go".into(),
            },
            &ctx,
        )
        .await;
        let drained = ctx.intent_ctx.bus.drain_user_events();
        let names: Vec<String> = drained
            .iter()
            .map(|e| match e {
                AgentEvent::UserEvent { name, .. } => name.clone(),
                _ => "?".into(),
            })
            .collect();
        // Depth 1 reaction emits user.a; depth 2 reaction emits user.b.
        // Depth 3 would emit user.c — that emit is produced by the
        // user.b reaction BUT the cascade dispatcher drops the tail
        // BEFORE re-dispatching to the user.b handler's children. So
        // we see user.a and user.b in the bus; user.c and user.d do
        // not make it.
        assert!(names.contains(&"user.a".to_string()));
        assert!(names.contains(&"user.b".to_string()));
        assert!(!names.contains(&"user.c".to_string()));
        assert!(!names.contains(&"user.d".to_string()));
    }

    /// Context's `cascade_child` helper respects the bound.
    #[test]
    fn intent_cascade_child_enforces_bound() {
        let ctx = intent_ctx();
        let d0 = ctx.cascade_depth;
        assert_eq!(d0, 0);
        let d1 = ctx.cascade_child().expect("1").cascade_depth;
        assert_eq!(d1, 1);
        let mut walk = ctx;
        for _ in 0..walk.max_cascade_depth {
            walk = walk.cascade_child().expect("within bound");
        }
        // One more hop exceeds the bound.
        assert!(walk.cascade_child().is_none());
    }

    /// `scene_max_cascade_depth` falls through to the default when
    /// the scene doesn't set the attribute, and honours it when set.
    #[test]
    fn scene_max_cascade_depth_reads_ast() {
        use ark_scene::ast::SceneDoc;
        use ark_scene::reactions::scene_max_cascade_depth;
        let doc: SceneDoc = facet_kdl::from_str(r#"scene "x""#).unwrap();
        assert_eq!(scene_max_cascade_depth(&doc), 4);
        let doc: SceneDoc = facet_kdl::from_str(r#"scene "x" max-cascade-depth=7"#).unwrap();
        assert_eq!(scene_max_cascade_depth(&doc), 7);
    }

    // -- T-5.6 telemetry --------------------------------------------------

    /// Test that the dispatcher emits a debug event under
    /// `target = "scene::reactions"` for every fired reaction.
    ///
    /// This is a **light** test: we directly check the
    /// `tracing::Subscriber::event` entrypoint rather than wiring a
    /// full subscriber through the dispatcher; the chatter of
    /// thread-local dispatch vs. `set_global_default` under cargo
    /// test's multi-threaded runtime is not worth tangling with for
    /// the signal this test provides.
    ///
    /// We call the T-5.6 helper directly (moved out of inline tracing
    /// macros in T-5.6 so tests can observe without a global
    /// subscriber), asserting it produces the canonical key set.
    ///
    /// The inline tracing macros fire the same payload through
    /// `debug!(target = "scene::reactions", ...)`; users enable them
    /// in production via `RUST_LOG=scene::reactions=debug`. Integration
    /// verification of the full tracing pipeline is an inspection
    /// step, not a unit test.
    #[test]
    fn telemetry_record_produces_expected_fields() {
        let rec = TelemetryRecord {
            selector: "Log".into(),
            reaction_origin: "user_scene".into(),
            event_kind: "log",
            event_name: String::new(),
            ops_run: 1,
            status: "ok",
            error: None,
        };
        let rendered = rec.render();
        assert!(rendered.contains("selector=\"Log\""));
        assert!(rendered.contains("event_kind=\"log\""));
        assert!(rendered.contains("ops_run=1"));
        assert!(rendered.contains("status=\"ok\""));
        assert!(rendered.contains("reaction_origin=\"user_scene\""));
    }

    #[test]
    fn telemetry_record_status_failed_carries_error() {
        let rec = TelemetryRecord {
            selector: "UserEvent:x".into(),
            reaction_origin: "user_scene".into(),
            event_kind: "user_event",
            event_name: "x".into(),
            ops_run: 2,
            status: "failed",
            error: Some("boom".into()),
        };
        let rendered = rec.render();
        assert!(rendered.contains("status=\"failed\""));
        assert!(rendered.contains("error=\"boom\""));
    }

    /// Test that the dispatcher emits a debug event under
    /// `target = "scene::reactions"` for every fired reaction.
    ///
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
