//! ACP ops — T-105, R7/R17.
//!
//! * [`AcpPromptOp`]  — `acp.prompt text="…"`.
//! * [`AcpCancelOp`]  — `acp.cancel`.
//! * [`AcpPermitOp`]  — `acp.permit request_id="…" outcome="…"`.
//! * [`AcpSetModeOp`] — `acp.set_mode mode="…"`.
//!
//! Each op delegates to the [`AcpHandle`](crate::intent::AcpHandle)
//! trait object on [`IntentContext`]. When no ACP-capable extension is
//! active (`ctx.acp` is `None`), every op logs a
//! [`tracing::warn!`] and returns `Ok(IntentValue::None)` — a no-op,
//! not an error (T-106).

use async_trait::async_trait;
use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{Intent, IntentContext, IntentValue, property_str, strict_map};

// ---------------------------------------------------------------------------
// acp.prompt
// ---------------------------------------------------------------------------

/// `acp.prompt text="…"` — send a user message into the ACP session.
///
/// Dispatches through [`AcpHandle::prompt`](crate::intent::AcpHandle::prompt).
/// The `text` property is required; missing it surfaces as
/// [`SceneError::OpFailed`].
#[derive(Debug, Default)]
pub struct AcpPromptOp;

const ACP_PROMPT_NAME: &str = "ark.acp.prompt";

#[async_trait]
impl Intent for AcpPromptOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let Some(acp) = ctx.acp.as_ref() else {
            tracing::warn!(
                target: "scene::ops",
                op = ACP_PROMPT_NAME,
                origin = %ctx.origin,
                "acp.prompt ignored — no ACP-capable extension is active"
            );
            return Ok(IntentValue::None);
        };
        let text = property_str(args, "text").ok_or_else(|| SceneError::OpFailed {
            op: ACP_PROMPT_NAME.to_string(),
            message: "missing required property `text=`".to_string(),
        })?;
        tracing::info!(
            target: "scene::ops",
            op = ACP_PROMPT_NAME,
            origin = %ctx.origin,
            "acp.prompt"
        );
        strict_map(ACP_PROMPT_NAME, acp.prompt(&text).await)
    }
}

// ---------------------------------------------------------------------------
// acp.cancel
// ---------------------------------------------------------------------------

/// `acp.cancel` — cancel the in-flight ACP turn.
///
/// Dispatches through [`AcpHandle::cancel`](crate::intent::AcpHandle::cancel).
/// Takes no arguments beyond the optional `when=` guard (handled upstream).
#[derive(Debug, Default)]
pub struct AcpCancelOp;

const ACP_CANCEL_NAME: &str = "ark.acp.cancel";

#[async_trait]
impl Intent for AcpCancelOp {
    async fn dispatch(
        &self,
        _args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let Some(acp) = ctx.acp.as_ref() else {
            tracing::warn!(
                target: "scene::ops",
                op = ACP_CANCEL_NAME,
                origin = %ctx.origin,
                "acp.cancel ignored — no ACP-capable extension is active"
            );
            return Ok(IntentValue::None);
        };
        tracing::info!(
            target: "scene::ops",
            op = ACP_CANCEL_NAME,
            origin = %ctx.origin,
            "acp.cancel"
        );
        strict_map(ACP_CANCEL_NAME, acp.cancel().await)
    }
}

// ---------------------------------------------------------------------------
// acp.permit
// ---------------------------------------------------------------------------

/// `acp.permit request_id="…" outcome="…"` — respond to a pending
/// permission request.
///
/// The `outcome` property must be one of `"allow"`, `"reject_once"`,
/// or `"reject_always"` — validated here before dispatch so callers
/// get a clear error rather than a protocol-level rejection.
#[derive(Debug, Default)]
pub struct AcpPermitOp;

const ACP_PERMIT_NAME: &str = "ark.acp.permit";

/// Accepted values for the `outcome=` property on `acp.permit`.
const VALID_PERMIT_OUTCOMES: &[&str] = &["allow", "reject_once", "reject_always"];

#[async_trait]
impl Intent for AcpPermitOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let Some(acp) = ctx.acp.as_ref() else {
            tracing::warn!(
                target: "scene::ops",
                op = ACP_PERMIT_NAME,
                origin = %ctx.origin,
                "acp.permit ignored — no ACP-capable extension is active"
            );
            return Ok(IntentValue::None);
        };
        let request_id =
            property_str(args, "request_id").ok_or_else(|| SceneError::OpFailed {
                op: ACP_PERMIT_NAME.to_string(),
                message: "missing required property `request_id=`".to_string(),
            })?;
        let outcome =
            property_str(args, "outcome").ok_or_else(|| SceneError::OpFailed {
                op: ACP_PERMIT_NAME.to_string(),
                message: "missing required property `outcome=`".to_string(),
            })?;
        if !VALID_PERMIT_OUTCOMES.contains(&outcome.as_str()) {
            return Err(SceneError::OpFailed {
                op: ACP_PERMIT_NAME.to_string(),
                message: format!(
                    "invalid outcome `{outcome}` — expected one of: {}",
                    VALID_PERMIT_OUTCOMES.join(", ")
                ),
            });
        }
        tracing::info!(
            target: "scene::ops",
            op = ACP_PERMIT_NAME,
            request_id = %request_id,
            outcome = %outcome,
            origin = %ctx.origin,
            "acp.permit"
        );
        strict_map(ACP_PERMIT_NAME, acp.permit(&request_id, &outcome).await)
    }
}

// ---------------------------------------------------------------------------
// acp.set_mode
// ---------------------------------------------------------------------------

/// `acp.set_mode mode="…"` — set the ACP agent mode.
///
/// Dispatches through [`AcpHandle::set_mode`](crate::intent::AcpHandle::set_mode).
/// The `mode` property is required.
#[derive(Debug, Default)]
pub struct AcpSetModeOp;

const ACP_SET_MODE_NAME: &str = "ark.acp.set_mode";

#[async_trait]
impl Intent for AcpSetModeOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let Some(acp) = ctx.acp.as_ref() else {
            tracing::warn!(
                target: "scene::ops",
                op = ACP_SET_MODE_NAME,
                origin = %ctx.origin,
                "acp.set_mode ignored — no ACP-capable extension is active"
            );
            return Ok(IntentValue::None);
        };
        let mode =
            property_str(args, "mode").ok_or_else(|| SceneError::OpFailed {
                op: ACP_SET_MODE_NAME.to_string(),
                message: "missing required property `mode=`".to_string(),
            })?;
        tracing::info!(
            target: "scene::ops",
            op = ACP_SET_MODE_NAME,
            mode = %mode,
            origin = %ctx.origin,
            "acp.set_mode"
        );
        strict_map(ACP_SET_MODE_NAME, acp.set_mode(&mode).await)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentContext;
    use crate::intent::tests::{MockAcp, MockBus, MockMux, node_from, test_scene_id};
    use std::sync::Arc;

    fn ctx_with_acp(acp: Arc<MockAcp>) -> IntentContext {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
            .with_acp(acp)
    }

    fn ctx_without_acp() -> IntentContext {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
    }

    // -- acp.prompt --------------------------------------------------------

    #[tokio::test]
    async fn prompt_dispatches_text() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp.clone());
        let node = node_from(r#"acp.prompt text="hello agent""#);
        AcpPromptOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(acp.take_calls(), vec!["prompt(hello agent)"]);
    }

    #[tokio::test]
    async fn prompt_requires_text() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp);
        let node = node_from(r#"acp.prompt"#);
        let err = AcpPromptOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn prompt_noop_when_no_acp() {
        let ctx = ctx_without_acp();
        let node = node_from(r#"acp.prompt text="hello""#);
        let v = AcpPromptOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }

    // -- acp.cancel --------------------------------------------------------

    #[tokio::test]
    async fn cancel_dispatches() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp.clone());
        let node = node_from(r#"acp.cancel"#);
        AcpCancelOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(acp.take_calls(), vec!["cancel()"]);
    }

    #[tokio::test]
    async fn cancel_noop_when_no_acp() {
        let ctx = ctx_without_acp();
        let node = node_from(r#"acp.cancel"#);
        let v = AcpCancelOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }

    // -- acp.permit --------------------------------------------------------

    #[tokio::test]
    async fn permit_dispatches_with_valid_outcome() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp.clone());
        let node = node_from(r#"acp.permit request_id="perm-0" outcome="allow""#);
        AcpPermitOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(acp.take_calls(), vec!["permit(perm-0,allow)"]);
    }

    #[tokio::test]
    async fn permit_rejects_invalid_outcome() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp);
        let node = node_from(r#"acp.permit request_id="perm-0" outcome="yolo""#);
        let err = AcpPermitOp.dispatch(&node, &ctx).await.expect_err("must error");
        if let SceneError::OpFailed { message, .. } = &err {
            assert!(message.contains("invalid outcome"), "got: {message}");
        } else {
            panic!("expected OpFailed, got {err:?}");
        }
    }

    #[tokio::test]
    async fn permit_requires_request_id() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp);
        let node = node_from(r#"acp.permit outcome="allow""#);
        let err = AcpPermitOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn permit_requires_outcome() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp);
        let node = node_from(r#"acp.permit request_id="perm-0""#);
        let err = AcpPermitOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn permit_noop_when_no_acp() {
        let ctx = ctx_without_acp();
        let node = node_from(r#"acp.permit request_id="perm-0" outcome="allow""#);
        let v = AcpPermitOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }

    // -- acp.set_mode ------------------------------------------------------

    #[tokio::test]
    async fn set_mode_dispatches() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp.clone());
        let node = node_from(r#"acp.set_mode mode="plan""#);
        AcpSetModeOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(acp.take_calls(), vec!["set_mode(plan)"]);
    }

    #[tokio::test]
    async fn set_mode_requires_mode() {
        let acp = Arc::new(MockAcp::default());
        let ctx = ctx_with_acp(acp);
        let node = node_from(r#"acp.set_mode"#);
        let err = AcpSetModeOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn set_mode_noop_when_no_acp() {
        let ctx = ctx_without_acp();
        let node = node_from(r#"acp.set_mode mode="plan""#);
        let v = AcpSetModeOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }
}
