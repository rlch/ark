//! `reaction_dispatcher` consumer task (soul phase 1 T-026).
//!
//! Replacement for the AgentEvent-era dispatcher. This consumer subscribes
//! to the supervisor's `tokio::sync::broadcast::Sender<CoreEvent>` and for
//! every event:
//!
//! 1. Classifies the event via [`EventKind::of`].
//! 2. Looks up reactions in the [`ReactionRegistry`] by kind; for
//!    [`CoreEvent::Ext`], additionally unions in the secondary index
//!    keyed by the dotted `<ext>.<kind>` name.
//! 3. Evaluates each candidate's parsed [`EventSelector`] matcher
//!    against the live event.
//! 4. Evaluates each candidate's optional Rhai `when=` predicate
//!    against an event scope built from the event + session snapshot
//!    + captured locals.
//! 5. Dispatches the reaction's op list through the intent registry.
//!    For an [`OpNode::Emit`] op the dispatcher additionally publishes
//!    the emitted event as a [`CoreEvent::Ext`] on the bus so cascades
//!    land uniformly on the same event surface.
//!
//! No ACP variants — Agent B deleted them from scene's [`OpNode`] and the
//! kit-level decision (interview #2) was to drop ACP outright. No
//! `engine_compat::*` calls — that module is gone. Exec + ReloadScene
//! routing is preserved.
//!
//! Resilient to `RecvError::Lagged(n)` (warn-log + continue), exits on
//! `RecvError::Closed`, honors a `tokio_util::sync::CancellationToken` for
//! supervisor-driven shutdown.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use ark_scene::ast::ops::OpNode;
use ark_scene::context::{SessionSnapshot, build_event_scope};
use ark_scene::intent::{IntentContext, IntentRegistry};
use ark_scene::reactions::{Entry, EventKind, ReactionRegistry, match_selector};
use ark_scene::rhai as scene_rhai;
use ark_types::{CoreEvent, EventSink, ExtEvent, SessionId, SessionStatus, StateLayout};
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::status_writer::{read_status, write_session_status_atomic};

/// Default cascade depth bound when the scene doesn't specify one.
pub const DEFAULT_MAX_CASCADE_DEPTH: u32 = 4;

/// Handle bundling the inputs `reaction_dispatcher` needs to dispatch
/// ops: the compiled [`ReactionRegistry`], the [`IntentRegistry`] the
/// scene has registered ops against, and the context snapshot
/// ([`SessionSnapshot`]).
///
/// `bus` is the same [`EventSink`] the supervisor cloned to every
/// producer; the dispatcher publishes cascade events through it when an
/// [`OpNode::Emit`] fires. `state` is the on-disk layout root the
/// [`OpNode::SetStatus`] handler uses to write into `ext_state`.
#[derive(Clone)]
pub struct ReactionDispatcherCtx {
    /// Compiled reactions — keyed by EventKind + Ext:name.
    pub reactions: Arc<ReactionRegistry>,

    /// Op dispatch surface registered with the core op set (`ark.core.*`)
    /// plus any extension ops contributed by `use` declarations.
    pub intents: Arc<IntentRegistry>,

    /// Intent context handed to every op dispatch (mux / bus / supervisor
    /// handles + scene identity + reaction origin). Cloned per-event so
    /// per-reaction overrides don't leak across dispatches.
    pub intent_ctx: IntentContext,

    /// Session snapshot fed into the Rhai event scope's `session.*` binding.
    pub session: Arc<SessionSnapshot>,

    /// Supervisor broadcast bus — dispatcher publishes cascade
    /// `CoreEvent::Ext` events here when an [`OpNode::Emit`] op fires.
    pub bus: EventSink,

    /// On-disk state layout used by the [`OpNode::SetStatus`] handler to
    /// update `SessionStatus::ext_state[<ext>]`.
    pub state: Arc<StateLayout>,

    /// The session whose `status.json` the SetStatus handler writes into.
    pub session_id: SessionId,

    /// Per-scene cascade-depth bound (R4 `max-cascade-depth=<N>`;
    /// default 4 when the scene attribute is absent).
    pub max_cascade_depth: u32,
}

/// Long-running consumer task. See module docs.
pub async fn reaction_dispatcher(
    mut rx: Receiver<CoreEvent>,
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
/// matching ops. See module docs for the cascade story.
pub async fn dispatch_event(event: &CoreEvent, ctx: &ReactionDispatcherCtx) {
    let kind = EventKind::of(event);

    // Assemble candidate reactions. Primary index always; secondary
    // index (`by_ext_name`) unions in for Ext events so extension-owned
    // reactions with a pinned `name=` pattern dispatch without a linear
    // scan of every `on Ext { … }`.
    let mut candidates: Vec<&Entry> = ctx.reactions.by_kind(kind).iter().collect();
    if let CoreEvent::Ext(ExtEvent { ext, kind: ev_kind, .. }) = event {
        let dotted = format!("{ext}.{ev_kind}");
        for entry in ctx.reactions.by_ext_name(&dotted) {
            if !candidates.iter().any(|c| std::ptr::eq(*c, entry)) {
                candidates.push(entry);
            }
        }
    }

    if candidates.is_empty() {
        return;
    }

    // Build a Rhai engine once per event for predicate eval.
    let rhai_engine = scene_rhai::Engine::new();

    // Event-name string (dotted `<ext>.<kind>` for Ext, empty for
    // core variants) — passed through to the telemetry target.
    let event_name = match event {
        CoreEvent::Ext(ExtEvent { ext, kind: ev_kind, .. }) => {
            format!("{ext}.{ev_kind}")
        }
        _ => String::new(),
    };

    for entry in candidates {
        let captures = match match_selector(&entry.selector, event) {
            Some(caps) => caps,
            None => continue,
        };

        let origin_tag = format!("{:?}", entry.origin);

        if let Some(program) = &entry.predicate {
            let mut scope = build_event_scope(event, ctx.session.as_ref(), &captures);
            match scene_rhai::eval_bool(&rhai_engine, program, &mut scope) {
                Ok(true) => { /* pass — continue to dispatch */ }
                Ok(false) => {
                    let rec = TelemetryRecord {
                        selector: format!("{:?}", entry.selector),
                        reaction_origin: origin_tag.clone(),
                        event_kind: kind.as_str(),
                        event_name: event_name.clone(),
                        ops_run: 0,
                        status: "skipped_predicate",
                        error: None,
                    };
                    emit_telemetry(&rec, "reaction skipped: Rhai predicate false");
                    continue;
                }
                Err(e) => {
                    warn!(
                        selector = ?entry.selector,
                        error = %e,
                        "reaction_dispatcher: Rhai predicate eval failed; skipping reaction"
                    );
                    continue;
                }
            }
        }

        let ops_run = entry.ops.len();
        let result = dispatch_op_sequence(&entry.ops, ctx).await;
        match &result {
            Ok(()) => {
                let rec = TelemetryRecord {
                    selector: format!("{:?}", entry.selector),
                    reaction_origin: origin_tag,
                    event_kind: kind.as_str(),
                    event_name: event_name.clone(),
                    ops_run,
                    status: "ok",
                    error: None,
                };
                emit_telemetry(&rec, "reaction fired");
            }
            Err(err) => {
                let rec = TelemetryRecord {
                    selector: format!("{:?}", entry.selector),
                    reaction_origin: origin_tag,
                    event_kind: kind.as_str(),
                    event_name: event_name.clone(),
                    ops_run,
                    status: "failed",
                    error: Some(err.to_string()),
                };
                emit_telemetry(&rec, "reaction op dispatch failed");
                warn!(
                    selector = ?entry.selector,
                    error = %err,
                    "reaction_dispatcher: op dispatch failed; continuing"
                );
            }
        }
    }
}

/// Dispatch a sequence of [`OpNode`]s. Two ops are handled locally
/// (with no engine-compat plumbing): [`OpNode::Emit`] fans out onto the
/// bus as a [`CoreEvent::Ext`], and [`OpNode::SetStatus`] read-modify-
/// writes `SessionStatus::ext_state`. Everything else routes through
/// the [`IntentRegistry`].
async fn dispatch_op_sequence(
    ops: &[OpNode],
    ctx: &ReactionDispatcherCtx,
) -> Result<(), String> {
    for op in ops {
        match op {
            OpNode::Emit(e) => {
                if let Err(err) = handle_emit_op(e, ctx) {
                    return Err(format!("op `emit` failed: {err}"));
                }
            }
            OpNode::SetStatus(s) => {
                if let Err(err) = handle_set_status_op(s, ctx) {
                    return Err(format!("op `set_status` failed: {err}"));
                }
            }
            _ => {
                let (name, node) = op_to_dispatch_pair(op);
                if let Err(err) = ctx.intents.dispatch(&name, &node, &ctx.intent_ctx).await {
                    return Err(format!("op `{name}` failed: {err}"));
                }
            }
        }
    }
    Ok(())
}

/// Parse an `emit` op into `(ext, kind, payload)` and publish it on the
/// bus as a [`CoreEvent::Ext`].
///
/// The AST carries the full dotted `event_name` (e.g. `"myext.hello"`)
/// plus an opaque KDL payload block. We split the name on the first `.`
/// to recover `(ext, kind)`; a name without a `.` is treated as
/// `(<name>, "")`. The payload block is serialised to JSON by mapping
/// every child node's first argument through `serde_json::Value` — good
/// enough for smoke tests and for the common "pass me a flat payload"
/// case. A richer payload grammar is future work.
fn handle_emit_op(
    op: &ark_scene::ast::ops::EmitOp,
    ctx: &ReactionDispatcherCtx,
) -> Result<(), String> {
    let (ext, ev_kind) = match op.event_name.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (op.event_name.clone(), String::new()),
    };

    let payload = match &op.payload {
        None => serde_json::Value::Null,
        Some(doc) => payload_doc_to_json(doc),
    };

    let ext_ev = ExtEvent {
        ext,
        kind: ev_kind,
        payload,
    };
    // `send` returns Err only when there are no receivers — treat that as
    // a soft noop (no one listening).
    let _ = ctx.bus.send(CoreEvent::Ext(ext_ev));
    Ok(())
}

/// Best-effort translation of an opaque KDL payload document into a JSON
/// object. Each child node contributes one `(name, value)` entry. Values
/// fall through `as_string` → `as_integer` → `as_float` → `as_bool`; a
/// node with no primitive argument serialises as JSON `null`. Children of
/// children are not recursed — scene payloads are expected to be flat.
fn payload_doc_to_json(doc: &kdl::KdlDocument) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for node in doc.nodes() {
        let key = node.name().value().to_string();
        let value = node
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .map(|e| kdl_value_to_json(e.value()))
            .unwrap_or(serde_json::Value::Null);
        obj.insert(key, value);
    }
    serde_json::Value::Object(obj)
}

fn kdl_value_to_json(v: &kdl::KdlValue) -> serde_json::Value {
    if let Some(s) = v.as_string() {
        return serde_json::Value::String(s.to_string());
    }
    if let Some(i) = v.as_integer() {
        return serde_json::json!(i);
    }
    if let Some(f) = v.as_float() {
        return serde_json::json!(f);
    }
    if let Some(b) = v.as_bool() {
        return serde_json::Value::Bool(b);
    }
    serde_json::Value::Null
}

/// Handle a `set_status` op: read `status.json`, find-or-create an
/// `ext_state` bucket for the dispatcher's origin, merge `text` (and
/// optional severity / ttl_ms) into it, write back atomically.
///
/// The bucket key is the intent_ctx's `origin` — that's the ext-name
/// tag the scene compiler records on every reaction. Core reactions
/// use the slug `"scene"`.
fn handle_set_status_op(
    op: &ark_scene::ast::ops::SetStatusOp,
    ctx: &ReactionDispatcherCtx,
) -> Result<(), String> {
    let mut status = match read_status(&ctx.state, &ctx.session_id) {
        Ok(Some(s)) => s,
        Ok(None) => SessionStatus {
            id: ctx.session_id.clone(),
            started_at: chrono::Utc::now(),
            terminated_at: None,
            ext_state: BTreeMap::new(),
        },
        Err(e) => return Err(format!("status.json read failed: {e}")),
    };

    // Build the ext_state payload. If a prior entry exists, merge on
    // object level so unrelated keys survive.
    let bucket_key = ctx.intent_ctx.origin.clone();
    let mut bucket = match status.ext_state.remove(&bucket_key) {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    bucket.insert(
        "text".into(),
        serde_json::Value::String(op.text.clone()),
    );
    if let Some(sev) = &op.severity {
        bucket.insert(
            "severity".into(),
            serde_json::Value::String(sev.clone()),
        );
    }
    if let Some(ttl) = op.ttl_ms {
        bucket.insert("ttl_ms".into(), serde_json::json!(ttl));
    }
    status
        .ext_state
        .insert(bucket_key, serde_json::Value::Object(bucket));

    write_session_status_atomic(&ctx.state, &ctx.session_id, &status)
        .map_err(|e| format!("status.json write failed: {e}"))
}

/// Convert an [`OpNode`] into a `(name, KdlNode)` pair suitable for
/// [`IntentRegistry::dispatch`]. `Emit` and `SetStatus` are handled
/// locally and never reach this fn.
fn op_to_dispatch_pair(op: &OpNode) -> (String, kdl::KdlNode) {
    use kdl::{KdlEntry, KdlNode, KdlValue};

    match op {
        OpNode::Focus(f) => {
            let mut node = KdlNode::new("focus");
            node.push(KdlEntry::new(KdlValue::String(f.handle.clone())));
            ("ark.core.focus".to_string(), node)
        }
        OpNode::Close(c) => {
            let mut node = KdlNode::new("close");
            node.push(KdlEntry::new(KdlValue::String(c.handle.clone())));
            ("ark.core.close".to_string(), node)
        }
        OpNode::Rename(r) => {
            let mut node = KdlNode::new("rename");
            node.push(KdlEntry::new(KdlValue::String(r.handle.clone())));
            node.push(KdlEntry::new_prop("to", KdlValue::String(r.to.clone())));
            ("ark.core.rename".to_string(), node)
        }
        OpNode::Resize(r) => {
            let mut node = KdlNode::new("resize");
            node.push(KdlEntry::new(KdlValue::String(r.handle.clone())));
            node.push(KdlEntry::new_prop(
                "direction",
                KdlValue::String(r.direction.clone()),
            ));
            node.push(KdlEntry::new_prop("by", KdlValue::String(r.by.clone())));
            ("ark.core.resize".to_string(), node)
        }
        OpNode::Move(m) => {
            let mut node = KdlNode::new("move");
            node.push(KdlEntry::new(KdlValue::String(m.handle.clone())));
            node.push(KdlEntry::new_prop("to", KdlValue::String(m.to.clone())));
            ("ark.core.move".to_string(), node)
        }
        OpNode::Pin(p) => {
            let mut node = KdlNode::new("pin");
            node.push(KdlEntry::new(KdlValue::String(p.handle.clone())));
            ("ark.core.pin".to_string(), node)
        }
        OpNode::Unpin(u) => {
            let mut node = KdlNode::new("unpin");
            node.push(KdlEntry::new(KdlValue::String(u.handle.clone())));
            ("ark.core.unpin".to_string(), node)
        }
        OpNode::Spawn(s) => {
            let mut node = KdlNode::new("spawn");
            node.push(KdlEntry::new(KdlValue::String(s.handle.clone())));
            if let Some(overlay) = &s.overlay {
                node.set_children(overlay.clone());
            }
            ("ark.core.spawn".to_string(), node)
        }
        OpNode::NewTab(t) => {
            let mut node = KdlNode::new("new_tab");
            node.push(KdlEntry::new(KdlValue::String(t.handle.clone())));
            if let Some(name) = &t.name {
                node.push(KdlEntry::new_prop("name", KdlValue::String(name.clone())));
            }
            if let Some(cwd) = &t.cwd {
                node.push(KdlEntry::new_prop("cwd", KdlValue::String(cwd.clone())));
            }
            ("ark.core.new_tab".to_string(), node)
        }
        OpNode::UseMode(m) => {
            let mut node = KdlNode::new("use_mode");
            node.push(KdlEntry::new(KdlValue::String(m.mode.clone())));
            ("ark.core.use_mode".to_string(), node)
        }
        OpNode::Pipe(p) => {
            let mut node = KdlNode::new("pipe");
            node.push(KdlEntry::new_prop("from", KdlValue::String(p.from.clone())));
            node.push(KdlEntry::new_prop("to", KdlValue::String(p.to.clone())));
            node.push(KdlEntry::new_prop(
                "payload",
                KdlValue::String(p.payload.clone()),
            ));
            ("ark.core.pipe".to_string(), node)
        }
        OpNode::Exec(e) => {
            let mut node = KdlNode::new("exec");
            node.push(KdlEntry::new_prop(
                "script",
                KdlValue::String(e.script.clone()),
            ));
            if let Some(shell) = &e.shell {
                node.push(KdlEntry::new_prop(
                    "shell",
                    KdlValue::String(shell.clone()),
                ));
            }
            if let Some(timeout) = e.timeout_ms {
                node.push(KdlEntry::new_prop(
                    "timeout_ms",
                    KdlValue::Integer(i128::from(timeout)),
                ));
            }
            ("ark.core.exec".to_string(), node)
        }
        OpNode::ReloadScene(_) => {
            let node = KdlNode::new("reload_scene");
            ("ark.core.reload_scene".to_string(), node)
        }
        OpNode::Unknown { verb, args } => {
            let mut node = KdlNode::new(verb.as_str());
            if !args.nodes().is_empty() {
                node.set_children(args.clone());
            }
            (verb.clone(), node)
        }
        // Emit and SetStatus are handled upstream in dispatch_op_sequence.
        OpNode::Emit(_) | OpNode::SetStatus(_) => {
            let node = KdlNode::new("unreachable");
            ("ark.core.unreachable".to_string(), node)
        }
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// Reaction-firing telemetry record rendered under the `scene::reactions`
/// tracing target.
#[derive(Debug, Clone)]
pub(crate) struct TelemetryRecord {
    pub selector: String,
    pub reaction_origin: String,
    pub event_kind: &'static str,
    pub event_name: String,
    pub ops_run: usize,
    pub status: &'static str,
    pub error: Option<String>,
}

impl TelemetryRecord {
    #[allow(dead_code)]
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

fn emit_telemetry(rec: &TelemetryRecord, message: &'static str) {
    tracing::debug!(
        target = "scene::reactions",
        selector = %rec.selector,
        reaction_origin = %rec.reaction_origin,
        event_kind = rec.event_kind,
        event_name = %rec.event_name,
        ops_run = rec.ops_run,
        status = rec.status,
        error = rec.error.as_deref().unwrap_or(""),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_scene::ast::ops::{EmitOp as AstEmitOp, SetStatusOp as AstSetStatusOp};
    use ark_scene::ast::selector::{EventSelector, FieldPattern, MatchType};
    use ark_scene::context::SessionSnapshot;
    use ark_scene::id::SceneId;
    use ark_scene::intent::IntentContext;
    use ark_scene::ops::register_core_ops;
    use ark_scene::reactions::{Entry, EventKind, ReactionOrigin, ReactionRegistry};
    use ark_types::{CoreEvent, ExtEvent, SessionId, StateLayout, channel};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_session_id() -> SessionId {
        SessionId::new("auth")
    }

    fn test_session_snapshot() -> SessionSnapshot {
        SessionSnapshot::default()
    }

    fn test_intent_ctx() -> IntentContext {
        IntentContext::new(
            SceneId::new(PathBuf::from("/tmp/scene.kdl"), b"scene \"x\" { }"),
            "scene",
        )
    }

    fn layout_in(base: PathBuf) -> Arc<StateLayout> {
        Arc::new(StateLayout::new(
            base.clone(),
            base.join("rt"),
            base.join("cfg"),
        ))
    }

    fn make_ctx(
        reactions: ReactionRegistry,
        layout: Arc<StateLayout>,
        session_id: SessionId,
    ) -> (ReactionDispatcherCtx, EventSink) {
        let mut intents = IntentRegistry::new();
        register_core_ops(&mut intents);
        let (bus, _rx) = channel(64);
        let ctx = ReactionDispatcherCtx {
            reactions: Arc::new(reactions),
            intents: Arc::new(intents),
            intent_ctx: test_intent_ctx(),
            session: Arc::new(test_session_snapshot()),
            bus: bus.clone(),
            state: layout,
            session_id,
            max_cascade_depth: DEFAULT_MAX_CASCADE_DEPTH,
        };
        (ctx, bus)
    }

    fn make_error_selector() -> EventSelector {
        EventSelector {
            kind: "error".to_string(),
            field_patterns: BTreeMap::new(),
        }
    }

    // -- T-026 test 1: Emit publishes ExtEvent -----------------------------

    #[tokio::test]
    async fn emit_publishes_core_ext_event_on_bus() {
        let mut registry = ReactionRegistry::new();
        let entry = Entry {
            selector: make_error_selector(),
            predicate: None,
            ops: vec![OpNode::Emit(AstEmitOp {
                event_name: "myext.fired".into(),
                payload: None,
                when: None,
            })],
            origin: ReactionOrigin::user_scene(PathBuf::from("test.kdl")),
        };
        registry.insert(EventKind::Error, None, entry);

        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = test_session_id();
        let (ctx, bus) = make_ctx(registry, layout, id);

        // Subscribe AFTER constructing ctx (ctx also holds a bus handle).
        let mut spy = bus.subscribe();

        let trigger = CoreEvent::Error {
            error: "boom".into(),
        };
        dispatch_event(&trigger, &ctx).await;

        // The bus should now carry a CoreEvent::Ext with ext="myext",
        // kind="fired". The spy subscribed after dispatch ran: rely on
        // the broadcast buffer to retain the emit (we sized the channel
        // to 64 above, so a single cascade event is retained).
        // Fall back to polling the receiver a couple of times.
        let mut saw = false;
        for _ in 0..10 {
            match spy.try_recv() {
                Ok(CoreEvent::Ext(ExtEvent { ext, kind, .. }))
                    if ext == "myext" && kind == "fired" =>
                {
                    saw = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        assert!(
            saw,
            "expected an Ext event (myext.fired) on the bus after Emit op ran"
        );
    }

    // -- T-026 test 2: SetStatus writes to ext_state ----------------------

    #[tokio::test]
    async fn set_status_writes_bucket_into_ext_state() {
        let mut registry = ReactionRegistry::new();
        let entry = Entry {
            selector: make_error_selector(),
            predicate: None,
            ops: vec![OpNode::SetStatus(AstSetStatusOp {
                text: "hello".into(),
                severity: Some("warn".into()),
                ttl_ms: Some(5000),
                when: None,
            })],
            origin: ReactionOrigin::user_scene(PathBuf::from("test.kdl")),
        };
        registry.insert(EventKind::Error, None, entry);

        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = test_session_id();
        let (ctx, _bus) = make_ctx(registry, layout.clone(), id.clone());

        let trigger = CoreEvent::Error {
            error: "x".into(),
        };
        dispatch_event(&trigger, &ctx).await;

        // Bucket key is the intent_ctx origin ("scene" in our test fixture).
        let status = crate::status_writer::read_status(&layout, &id)
            .expect("read")
            .expect("status exists");
        let bucket = status
            .ext_state
            .get("scene")
            .expect("scene bucket present in ext_state");
        assert_eq!(bucket.get("text").and_then(|v| v.as_str()), Some("hello"));
        assert_eq!(bucket.get("severity").and_then(|v| v.as_str()), Some("warn"));
        assert_eq!(bucket.get("ttl_ms").and_then(|v| v.as_u64()), Some(5000));
    }

    // -- cancel exits cleanly --------------------------------------------

    #[tokio::test]
    async fn cancel_exits_cleanly() {
        use tokio::sync::broadcast;
        let (tx, rx) = broadcast::channel::<CoreEvent>(4);
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let (ctx, _bus) = make_ctx(ReactionRegistry::new(), layout, test_session_id());
        let cancel = CancellationToken::new();
        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move { reaction_dispatcher(rx, ctx, cancel).await })
        };
        drop(tx);
        cancel.cancel();
        let res = handle.await.expect("join");
        assert!(res.is_ok());
    }

    // -- telemetry rendering ---------------------------------------------

    #[test]
    fn telemetry_record_produces_expected_fields() {
        let rec = TelemetryRecord {
            selector: "Error".into(),
            reaction_origin: "user_scene".into(),
            event_kind: "error",
            event_name: String::new(),
            ops_run: 1,
            status: "ok",
            error: None,
        };
        let rendered = rec.render();
        assert!(rendered.contains("event_kind=\"error\""));
        assert!(rendered.contains("ops_run=1"));
        assert!(rendered.contains("status=\"ok\""));
    }

    #[test]
    fn max_cascade_depth_default_is_4() {
        assert_eq!(DEFAULT_MAX_CASCADE_DEPTH, 4);
    }

    // -- mis-classification on mismatched kind ---------------------------

    #[tokio::test]
    async fn non_matching_kind_drops() {
        let mut registry = ReactionRegistry::new();
        let entry = Entry {
            selector: EventSelector {
                kind: "session_started".into(),
                field_patterns: BTreeMap::new(),
            },
            predicate: None,
            ops: vec![OpNode::Emit(AstEmitOp {
                event_name: "should.not.fire".into(),
                payload: None,
                when: None,
            })],
            origin: ReactionOrigin::user_scene(PathBuf::from("test.kdl")),
        };
        registry.insert(EventKind::SessionStarted, None, entry);

        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let (ctx, bus) = make_ctx(registry, layout, test_session_id());
        let mut spy = bus.subscribe();

        dispatch_event(
            &CoreEvent::Error {
                error: "x".into(),
            },
            &ctx,
        )
        .await;

        // No matching reaction → no Ext event published.
        assert!(matches!(
            spy.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
