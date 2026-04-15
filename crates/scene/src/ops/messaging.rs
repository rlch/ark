//! Messaging ops — R7 #9–11.
//!
//! * `pipe plugin=<str> [severity=<str>] [name=<str>] { text <str> OR json <str> }`
//! * `emit <user-event-name=<str>> { <kv payload>* }`
//! * `set_status text=<str> [severity=<str>] [ttl_ms=<int>]` — sugar over
//!   `pipe` targeting the built-in `ark-status` plugin.
//!
//! `pipe` and `set_status` are STUBS — they record a tracing line and
//! return `Ok(None)`. `emit` is PARTIALLY WIRED: it constructs a real
//! [`AgentEvent::UserEvent`] and appends it to the placeholder
//! [`EventBus`]'s capture queue via
//! [`crate::intent::EventBus::record_user_event`]. Tests read the queue
//! via [`crate::intent::EventBus::drain_user_events`]. When the real bus
//! lands (T-5.x), the op switches from capture to broadcast fan-out.
//!
//! TODO(T-5.x): replace pipe/set_status stubs with real calls once the
//! zellij plugin pipe surface is wired.
//! TODO(T-5.x): flip `emit` from capture-queue to real bus broadcast.

use async_trait::async_trait;
use ark_types::event::AgentEvent;
use facet::Facet;
use facet_kdl as kdl;

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

// ---------------------------------------------------------------------------
// pipe
// ---------------------------------------------------------------------------

/// Canonical pipe severities (R7 + R10 status-bar convention).
pub const PIPE_SEVERITIES: &[&str] = &["info", "warn", "error", "debug"];

/// Args to the `pipe` op — forward a message to a named plugin via the
/// zellij pipe primitive.
///
/// R7 shape: `pipe plugin=<str> [severity=<str>] [name=<str>] { text <str> OR json <str> }`.
/// Exactly one of `text` / `json` is required in the body; enforced at
/// dispatch.
#[derive(Facet, Debug)]
pub struct PipeArgs {
    /// Plugin to route the message to. Cross-referenced against
    /// `plugin "<name>" { }` declarations at compile time (T-4.3).
    #[facet(kdl::property)]
    pub plugin: String,

    /// Severity hint surfaced to the plugin (`info`, `warn`, `error`,
    /// `debug`). Validated at dispatch.
    #[facet(kdl::property, default)]
    pub severity: Option<String>,

    /// Optional routing name (e.g. namespace suffix the plugin uses to
    /// dispatch internally).
    #[facet(kdl::property, default)]
    pub name: Option<String>,

    /// `text "<content>"` child — mutually exclusive with `json`.
    #[facet(kdl::child, default)]
    pub text: Option<PipeTextNode>,

    /// `json "<content>"` child — mutually exclusive with `text`.
    #[facet(kdl::child, default)]
    pub json: Option<PipeJsonNode>,
}

/// `text "<string>"` child of a `pipe` body.
#[derive(Facet, Debug)]
pub struct PipeTextNode {
    /// Message body (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// `json "<string>"` child of a `pipe` body. The argument is still a
/// string — the plugin is responsible for parsing it. (Keeping `json` as
/// a string avoids threading `serde_json::Value` through the facet-kdl
/// derive today; a typed variant is deferred to v0.2.)
#[derive(Facet, Debug)]
pub struct PipeJsonNode {
    /// JSON body as a string (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// facet-kdl document wrapper for [`PipeArgs`].
#[derive(Facet, Debug)]
pub struct PipeDoc {
    /// The `pipe` node body.
    #[facet(kdl::child, rename = "pipe")]
    pub pipe: PipeArgs,
}

/// `pipe` op — always side-effects (every fire is a message).
#[derive(Debug, Default)]
pub struct PipeOp;

impl PipeOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for PipeOp {
    type Args = PipeDoc;
    const NAME: &'static str = "ark.core.pipe";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        match (&args.pipe.text, &args.pipe.json) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    "both `text` and `json` children provided; specify exactly one"
                        .to_string()
                        .into(),
                ))
            }
            (None, None) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    "neither `text` nor `json` child provided; specify exactly one"
                        .to_string()
                        .into(),
                ))
            }
        }
        if let Some(sev) = &args.pipe.severity {
            if !PIPE_SEVERITIES.contains(&sev.as_str()) {
                return Err(IntentError::failed(
                    Self::NAME,
                    format!(
                        "invalid `severity=\"{sev}\"`; expected one of {PIPE_SEVERITIES:?}"
                    )
                    .into(),
                ));
            }
        }
        // TODO(T-5.x): call `ctx.mux.pipe_to_plugin(...)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            plugin = %args.pipe.plugin,
            severity = ?args.pipe.severity,
            name = ?args.pipe.name,
            "pipe (stub: awaiting real plugin-pipe handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// emit
// ---------------------------------------------------------------------------

/// Args to the `emit` op — publish a synthetic `UserEvent` onto the bus.
///
/// R7 shape: `emit "<user-event-name>" { <kv payload>* }`. The payload
/// arrives as a single `OpaquePayload` child of arbitrary kv pairs;
/// because facet-kdl's derive surface doesn't natively speak "arbitrary
/// kv bag" today, we accept the payload as a single `json` child — a
/// string whose contents we parse to `serde_json::Value` at dispatch.
/// When facet-kdl grows a reflection path for dynamic maps, we can
/// swap this for a proper kv body.
#[derive(Facet, Debug)]
pub struct EmitArgs {
    /// Event name — dotted, namespaced (e.g. `"user.hello"`,
    /// `"myext.ping"`). Emits render as
    /// `AgentEvent::UserEvent { name, payload, source }`.
    #[facet(kdl::argument)]
    pub name: String,

    /// Optional `json "<string>"` child carrying the payload.
    /// Absent ⇒ payload = `null`. Invalid JSON ⇒ op/failed.
    #[facet(kdl::child, default)]
    pub json: Option<EmitJsonNode>,
}

/// `json "<string>"` child of an `emit` body.
#[derive(Facet, Debug)]
pub struct EmitJsonNode {
    /// Payload as a JSON string (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// facet-kdl document wrapper for [`EmitArgs`].
#[derive(Facet, Debug)]
pub struct EmitDoc {
    /// The `emit` node body.
    #[facet(kdl::child, rename = "emit")]
    pub emit: EmitArgs,
}

/// `emit` op — always side-effects.
#[derive(Debug, Default)]
pub struct EmitOp;

impl EmitOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for EmitOp {
    type Args = EmitDoc;
    const NAME: &'static str = "ark.core.emit";

    async fn dispatch(
        &self,
        args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let payload = match &args.emit.json {
            Some(n) => serde_json::from_str::<serde_json::Value>(&n.value).map_err(|e| {
                IntentError::failed(
                    Self::NAME,
                    format!("payload is not valid JSON: {e}").into(),
                )
            })?,
            None => serde_json::Value::Null,
        };
        let event = AgentEvent::UserEvent {
            name: args.emit.name.clone(),
            payload: payload.clone(),
            source: "scene".to_string(),
        };
        // TODO(T-5.x): replace `record_user_event` capture with a real
        // broadcast through `ctx.bus` once ark_core::EventBus lands.
        ctx.bus.record_user_event(event);
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = %args.emit.name,
            "emit (captured into placeholder bus queue)"
        );
        Ok(Some(payload))
    }
}

// ---------------------------------------------------------------------------
// set_status
// ---------------------------------------------------------------------------

/// Args to the `set_status` op — sugar over
/// `pipe plugin="ark-status"` with a conventional payload shape.
///
/// R7 shape: `set_status text=<str> [severity=<str>] [ttl_ms=<int>]`.
#[derive(Facet, Debug)]
pub struct SetStatusArgs {
    /// Status text to display.
    #[facet(kdl::property)]
    pub text: String,

    /// One of `info`, `warn`, `error`, `debug` (per [`PIPE_SEVERITIES`]).
    #[facet(kdl::property, default)]
    pub severity: Option<String>,

    /// How long to show the status, in milliseconds. Absent ⇒ plugin's
    /// default (typically sticky).
    #[facet(kdl::property, default)]
    pub ttl_ms: Option<u64>,
}

/// facet-kdl document wrapper for [`SetStatusArgs`].
#[derive(Facet, Debug)]
pub struct SetStatusDoc {
    /// The `set_status` node body.
    #[facet(kdl::child, rename = "set_status")]
    pub set_status: SetStatusArgs,
}

/// `set_status` op — always side-effects.
#[derive(Debug, Default)]
pub struct SetStatusOp;

impl SetStatusOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for SetStatusOp {
    type Args = SetStatusDoc;
    const NAME: &'static str = "ark.core.set_status";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        if let Some(sev) = &args.set_status.severity {
            if !PIPE_SEVERITIES.contains(&sev.as_str()) {
                return Err(IntentError::failed(
                    Self::NAME,
                    format!(
                        "invalid `severity=\"{sev}\"`; expected one of {PIPE_SEVERITIES:?}"
                    )
                    .into(),
                ));
            }
        }
        // TODO(T-5.x): lower into a `pipe` call on the `ark-status`
        // plugin once the plugin-pipe handle lands.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            text = %args.set_status.text,
            severity = ?args.set_status.severity,
            ttl_ms = ?args.set_status.ttl_ms,
            "set_status (stub: awaiting real plugin-pipe handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SceneId;
    use crate::intent::IntentRegistry;
    use ::kdl::{KdlDocument, KdlNode};
    use std::path::PathBuf;

    fn ctx() -> IntentContext {
        IntentContext::placeholder(SceneId::from_bytes(
            PathBuf::from("/tmp/scene.kdl"),
            b"scene \"x\" { }",
        ))
    }

    fn node(src: &str) -> KdlNode {
        let doc: KdlDocument = src.parse().expect("parse");
        doc.nodes().first().cloned().expect("node")
    }

    // -- pipe -----------------------------------------------------------

    #[tokio::test]
    async fn pipe_text_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(PipeOp).await;
        let n = node(r#"pipe plugin="picker" severity="info" { text "hi" }"#);
        reg.dispatch_dyn(PipeOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn pipe_json_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(PipeOp).await;
        let n = node(r#"pipe plugin="picker" { json "{\"k\":1}" }"#);
        reg.dispatch_dyn(PipeOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn pipe_rejects_both_bodies() {
        let reg = IntentRegistry::new();
        reg.register(PipeOp).await;
        let n = node(r#"pipe plugin="x" { text "a"; json "{}" }"#);
        let err = reg
            .dispatch_dyn(PipeOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[tokio::test]
    async fn pipe_requires_body() {
        let reg = IntentRegistry::new();
        reg.register(PipeOp).await;
        let n = node(r#"pipe plugin="x""#);
        let err = reg
            .dispatch_dyn(PipeOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[tokio::test]
    async fn pipe_rejects_invalid_severity() {
        let reg = IntentRegistry::new();
        reg.register(PipeOp).await;
        let n = node(r#"pipe plugin="x" severity="oof" { text "a" }"#);
        let err = reg
            .dispatch_dyn(PipeOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    // -- emit -----------------------------------------------------------

    #[tokio::test]
    async fn emit_enqueues_user_event_into_bus() {
        let reg = IntentRegistry::new();
        reg.register(EmitOp).await;
        let c = ctx();

        let n = node(r#"emit "user.hello" { json "{\"greeting\":\"world\"}" }"#);
        let ret = reg
            .dispatch_dyn(EmitOp::NAME, &n, &c)
            .await
            .expect("dispatch");
        assert_eq!(ret, Some(serde_json::json!({"greeting": "world"})));

        let drained = c.bus.drain_user_events();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            AgentEvent::UserEvent {
                name,
                payload,
                source,
            } => {
                assert_eq!(name, "user.hello");
                assert_eq!(source, "scene");
                assert_eq!(payload, &serde_json::json!({"greeting": "world"}));
            }
            other => panic!("expected UserEvent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_without_payload_defaults_to_null() {
        let reg = IntentRegistry::new();
        reg.register(EmitOp).await;
        let c = ctx();
        let n = node(r#"emit "user.tick""#);
        reg.dispatch_dyn(EmitOp::NAME, &n, &c)
            .await
            .expect("dispatch");
        let drained = c.bus.drain_user_events();
        assert_eq!(drained.len(), 1);
        match &drained[0] {
            AgentEvent::UserEvent { payload, .. } => {
                assert_eq!(*payload, serde_json::Value::Null);
            }
            other => panic!("expected UserEvent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emit_rejects_bad_json() {
        let reg = IntentRegistry::new();
        reg.register(EmitOp).await;
        let c = ctx();
        let n = node(r#"emit "x" { json "{not json" }"#);
        let err = reg
            .dispatch_dyn(EmitOp::NAME, &n, &c)
            .await
            .expect_err("bad json");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    // -- set_status -----------------------------------------------------

    #[tokio::test]
    async fn set_status_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(SetStatusOp).await;
        let n = node(r#"set_status text="hello" severity="info" ttl_ms=1000"#);
        reg.dispatch_dyn(SetStatusOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn set_status_rejects_bad_severity() {
        let reg = IntentRegistry::new();
        reg.register(SetStatusOp).await;
        let n = node(r#"set_status text="hi" severity="loud""#);
        let err = reg
            .dispatch_dyn(SetStatusOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[test]
    fn messaging_ops_idempotency_matrix() {
        assert_eq!(PipeOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(EmitOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(SetStatusOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
    }
}
