//! Stack control ops (scene-2026-04-18 T-023).
//!
//! Houses ops that operate on a `stack` handle rather than a pane or
//! tab. Currently the only op here is [`ClearOp`]; `spawn_into` lives
//! next to `spawn` in [`super::spawn`] so all "create a new pane" ops
//! stay clustered.
//!
//! # Idempotency
//!
//! Per R-7 scene-2026-04-18: `clear @stack` is **idempotent** — calling
//! it on an already-empty stack is a no-op. Mux-side "absent"
//! errors therefore collapse to `Ok(IntentValue::None)` via
//! [`idempotent_map`]. This mirrors `close @pane` / `close @tab`'s
//! handling of vanished handles.

use ark_view::HandleId;
use async_trait::async_trait;
use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{Intent, IntentContext, IntentValue, first_argument, idempotent_map};

/// `clear @stack` — close every child pane currently in `@stack`.
///
/// The stack itself is preserved; only its children are torn down.
/// Subsequent `spawn_into @stack { … }` calls push fresh children on
/// top of the empty stack (non-idempotent per R-7).
#[derive(Debug, Default)]
pub struct ClearOp;

const CLEAR_NAME: &str = "ark.core.clear";

#[async_trait]
impl Intent for ClearOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let mux = ctx.mux.as_ref().ok_or_else(|| SceneError::OpFailed {
            op: CLEAR_NAME.to_string(),
            message: "mux handle not wired".to_string(),
        })?;
        let raw = first_argument(args).ok_or_else(|| SceneError::OpFailed {
            op: CLEAR_NAME.to_string(),
            message: "missing `@stack` argument".to_string(),
        })?;
        if raw.is_empty() {
            return Err(SceneError::OpFailed {
                op: CLEAR_NAME.to_string(),
                message: "empty `@stack` argument".to_string(),
            });
        }
        let stack = HandleId::new(raw.clone());
        tracing::info!(
            target: "scene::ops",
            op = CLEAR_NAME,
            stack = %raw,
            origin = %ctx.origin,
            "clear"
        );
        idempotent_map(CLEAR_NAME, mux.clear_stack(&stack))
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

    fn ctx_with(mux: Arc<MockMux>) -> IntentContext {
        let bus = Arc::new(MockBus::default());
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
    }

    #[tokio::test]
    async fn clear_dispatches_to_mux() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"clear "@subs""#);
        let v = ClearOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
        let calls = mux.take_calls();
        assert_eq!(calls, vec!["clear_stack(@subs)".to_string()]);
    }

    #[tokio::test]
    async fn clear_idempotent_on_absent_stack() {
        // Mux returns "not found" → clear must swallow it as Ok(None).
        let mux = Arc::new(MockMux::default());
        mux.set_fail("stack not found");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"clear "@ghost""#);
        let v = ClearOp.dispatch(&node, &ctx).await.expect("absent -> ok");
        assert_eq!(v, IntentValue::None);
    }

    #[tokio::test]
    async fn clear_surfaces_non_noop_errors() {
        let mux = Arc::new(MockMux::default());
        mux.set_fail("mux socket disconnected");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"clear "@subs""#);
        let err = ClearOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("must surface");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "ark.core.clear"));
    }

    #[tokio::test]
    async fn clear_missing_handle_errors() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"clear"#);
        let err = ClearOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn clear_double_call_is_noop_safe() {
        // Empty → clear → clear should both succeed without
        // side-effects accumulating.
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"clear "@subs""#);
        ClearOp.dispatch(&node, &ctx).await.expect("first ok");
        ClearOp.dispatch(&node, &ctx).await.expect("second ok");
        let calls = mux.take_calls();
        assert_eq!(
            calls,
            vec![
                "clear_stack(@subs)".to_string(),
                "clear_stack(@subs)".to_string(),
            ]
        );
    }
}
