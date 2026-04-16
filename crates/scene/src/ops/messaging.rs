//! Messaging ops — T-050, R7.
//!
//! * [`PipeOp`]      — `pipe from=@h to=@h payload="…"`.
//! * [`EmitOp`]      — `emit "<name>" { <kv payload> }`.
//! * [`SetStatusOp`] — `set_status text="…" severity=… ttl_ms=…`.
//!
//! All three ops are classified as "always side-effect" — no
//! idempotency shortcuts. Absence of a runtime handle (mux for `pipe`,
//! bus for `emit` / `set_status`) surfaces as a clean
//! [`SceneError::OpFailed`] so scenes authored against the R7 surface
//! still parse + compile cleanly in test environments.

use async_trait::async_trait;
use kdl::{KdlNode, KdlValue};

use crate::error::SceneError;
use crate::intent::{
    Intent, IntentContext, IntentValue, first_argument, parse_handle,
    property_str, property_u64, strict_map,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_bus(ctx: &IntentContext, op: &'static str) -> Result<(), SceneError> {
    if ctx.bus.is_some() {
        Ok(())
    } else {
        Err(SceneError::OpFailed {
            op: op.to_string(),
            message: "event bus not wired".to_string(),
        })
    }
}

fn require_mux(ctx: &IntentContext, op: &'static str) -> Result<(), SceneError> {
    if ctx.mux.is_some() {
        Ok(())
    } else {
        Err(SceneError::OpFailed {
            op: op.to_string(),
            message: "mux handle not wired".to_string(),
        })
    }
}

/// Convert a [`KdlValue`] into a [`serde_json::Value`] for emit payloads.
///
/// KDL 2.0 values (string / i128 / f64 / bool / null) map cleanly onto
/// JSON; the only lossy case is i128 values that overflow i64, which we
/// truncate via `as i64` and log. Extensions that need arbitrary-
/// precision numbers emit them as strings.
fn kdl_value_to_json(v: &KdlValue) -> serde_json::Value {
    if let Some(s) = v.as_string() {
        serde_json::Value::String(s.to_string())
    } else if let Some(b) = v.as_bool() {
        serde_json::Value::Bool(b)
    } else if let Some(i) = v.as_integer() {
        // KDL integers are i128; JSON is i64.
        match i64::try_from(i) {
            Ok(n) => serde_json::Value::Number(n.into()),
            Err(_) => serde_json::Value::Number((i as i64).into()),
        }
    } else if let Some(f) = v.as_float() {
        serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    }
}

/// Walk an `emit "<name>" { <kv> … }` payload block into a JSON object.
///
/// Each child node of the payload is treated as a key whose value is
/// the first positional argument. Example: `key "value"` →
/// `{"key": "value"}`. Nested blocks are currently flattened into the
/// child's first argument (matches v2 behavior); a recursive walker
/// lands when extensions ship scenes that require nested payloads.
fn payload_to_json(node: &KdlNode) -> serde_json::Value {
    let Some(doc) = node.children() else {
        return serde_json::Value::Object(Default::default());
    };
    let mut map = serde_json::Map::new();
    for child in doc.nodes() {
        let key = child.name().value().to_string();
        let value = child
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .map(|e| kdl_value_to_json(e.value()))
            .unwrap_or(serde_json::Value::Null);
        map.insert(key, value);
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// pipe
// ---------------------------------------------------------------------------

/// `pipe from=@handle to=@handle payload="…"` — forward a payload from
/// one pane to another.
///
/// Panes must both exist; "not found" errors on either side surface as
/// a genuine [`SceneError::OpFailed`] (pipe is classified always-
/// side-effect, so even absent targets are errors — dropping a pipe on
/// the floor would be hard to debug).
#[derive(Debug, Default)]
pub struct PipeOp;

const PIPE_NAME: &str = "ark.core.pipe";

#[async_trait]
impl Intent for PipeOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, PIPE_NAME)?;
        let from_raw = property_str(args, "from").ok_or_else(|| SceneError::OpFailed {
            op: PIPE_NAME.to_string(),
            message: "missing required property `from=`".to_string(),
        })?;
        let to_raw = property_str(args, "to").ok_or_else(|| SceneError::OpFailed {
            op: PIPE_NAME.to_string(),
            message: "missing required property `to=`".to_string(),
        })?;
        let payload =
            property_str(args, "payload").ok_or_else(|| SceneError::OpFailed {
                op: PIPE_NAME.to_string(),
                message: "missing required property `payload=`".to_string(),
            })?;
        let from = parse_handle(&from_raw, PIPE_NAME)?;
        let to = parse_handle(&to_raw, PIPE_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = PIPE_NAME,
            from = %from.raw(),
            to = %to.raw(),
            origin = %ctx.origin,
            "pipe"
        );
        strict_map(PIPE_NAME, mux.pipe(&from, &to, &payload))
    }
}

// ---------------------------------------------------------------------------
// emit
// ---------------------------------------------------------------------------

/// `emit "<event-name>" { <kv payload> }` — emit a UserEvent on the bus.
///
/// The emitter attribution (`source` field on the generated event)
/// follows [`IntentContext::origin`]. The scene dispatcher constructs
/// contexts with `origin = "scene"` for user-authored reactions;
/// extensions get `origin = "ext:<name>"`. A scene that carries no
/// explicit origin still attributes to `"scene"` (the default value
/// the reactions dispatcher installs).
#[derive(Debug, Default)]
pub struct EmitOp;

const EMIT_NAME: &str = "ark.core.emit";

#[async_trait]
impl Intent for EmitOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_bus(ctx, EMIT_NAME)?;
        let name = first_argument(args).ok_or_else(|| SceneError::OpFailed {
            op: EMIT_NAME.to_string(),
            message: "missing event-name argument".to_string(),
        })?;
        let payload = payload_to_json(args);
        let bus = ctx.bus.as_ref().expect("checked by require_bus");
        tracing::info!(
            target: "scene::ops",
            op = EMIT_NAME,
            event = %name,
            origin = %ctx.origin,
            "emit"
        );
        bus.emit_user_event(&name, &ctx.origin, payload);
        Ok(IntentValue::None)
    }
}

// ---------------------------------------------------------------------------
// set_status
// ---------------------------------------------------------------------------

/// `set_status text="…" [severity=…] [ttl_ms=…]` — push a status-bar
/// message.
///
/// Routes through [`crate::intent::EventBus::push_status`] which
/// currently emits a `ark.status.push` UserEvent; the status extension
/// subscribes and renders it. The op stays a core op (rather than
/// syntactic sugar over `emit`) because the status bar is a first-class
/// concept in the scene grammar.
#[derive(Debug, Default)]
pub struct SetStatusOp;

const SET_STATUS_NAME: &str = "ark.core.set_status";

#[async_trait]
impl Intent for SetStatusOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_bus(ctx, SET_STATUS_NAME)?;
        let text = property_str(args, "text").ok_or_else(|| SceneError::OpFailed {
            op: SET_STATUS_NAME.to_string(),
            message: "missing required property `text=`".to_string(),
        })?;
        let severity = property_str(args, "severity");
        let ttl_ms = property_u64(args, "ttl_ms");
        let bus = ctx.bus.as_ref().expect("checked by require_bus");
        tracing::info!(
            target: "scene::ops",
            op = SET_STATUS_NAME,
            text = %text,
            severity = ?severity,
            ttl_ms = ?ttl_ms,
            origin = %ctx.origin,
            "set_status"
        );
        bus.push_status(&text, severity.as_deref(), ttl_ms);
        Ok(IntentValue::None)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentContext;
    use crate::intent::tests::{MockBus, MockMux, node_from, test_scene_id};
    use std::sync::Arc;

    fn ctx_with(mux: Arc<MockMux>, bus: Arc<MockBus>) -> IntentContext {
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
    }

    #[tokio::test]
    async fn pipe_sends_payload() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node =
            node_from(r#"pipe from="@src" to="@dst" payload="hello""#);
        PipeOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(
            mux.take_calls(),
            vec!["pipe(@src,@dst,hello)".to_string()]
        );
    }

    #[tokio::test]
    async fn pipe_requires_from_to_payload() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node = node_from(r#"pipe from="@src" to="@dst""#);
        let err = PipeOp.dispatch(&node, &ctx).await.expect_err("missing payload");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn emit_records_user_event() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node = node_from(
            r#"emit "user.hello" {
                who "world"
                n 42
            }"#,
        );
        EmitOp.dispatch(&node, &ctx).await.expect("ok");
        let events = bus.take_events();
        assert_eq!(events.len(), 1);
        let (name, source, payload) = &events[0];
        assert_eq!(name, "user.hello");
        assert_eq!(source, "scene");
        assert_eq!(payload["who"], serde_json::json!("world"));
        assert_eq!(payload["n"], serde_json::json!(42));
    }

    #[tokio::test]
    async fn emit_with_no_payload_emits_empty_object() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node = node_from(r#"emit "user.ping""#);
        EmitOp.dispatch(&node, &ctx).await.expect("ok");
        let events = bus.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].2, serde_json::json!({}));
    }

    #[tokio::test]
    async fn emit_uses_origin_as_source() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = IntentContext::new(test_scene_id(), "ext:myext")
            .with_mux(mux)
            .with_bus(bus.clone());
        let node = node_from(r#"emit "user.x""#);
        EmitOp.dispatch(&node, &ctx).await.expect("ok");
        let events = bus.take_events();
        assert_eq!(events[0].1, "ext:myext");
    }

    #[tokio::test]
    async fn set_status_routes_to_push_status() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node =
            node_from(r#"set_status text="ready" severity="info" ttl_ms=2000"#);
        SetStatusOp.dispatch(&node, &ctx).await.expect("ok");
        let events = bus.take_events();
        assert_eq!(events.len(), 1);
        let (name, _source, payload) = &events[0];
        assert_eq!(name, "ark.status.push");
        assert_eq!(payload["text"], serde_json::json!("ready"));
        assert_eq!(payload["severity"], serde_json::json!("info"));
        assert_eq!(payload["ttl_ms"], serde_json::json!(2000));
    }

    #[tokio::test]
    async fn set_status_requires_text() {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux.clone(), bus.clone());
        let node = node_from(r#"set_status severity="info""#);
        let err = SetStatusOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }
}
