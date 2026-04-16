//! Spawn ops — T-049, R7.
//!
//! * [`SpawnOp`]  — `spawn @handle { <view> }` (tiled) or
//!                  `spawn @handle overlay pos=… size=… { <view> }`
//!                  (overlay).
//! * [`NewTabOp`] — `new_tab @handle [name=…] [cwd=…]`.
//!
//! Both ops follow the T-055 "check-then-create-else-focus" policy:
//! when the handle already exists the op focuses the existing target
//! rather than failing or re-creating.

use async_trait::async_trait;
use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{
    Intent, IntentContext, IntentValue, first_argument, parse_handle,
    property_str, strict_map,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the `@handle` positional argument from `node`, surfacing a
/// clean `op/failed` when it's missing.
fn require_handle(
    node: &KdlNode,
    op: &'static str,
) -> Result<crate::ast::layout::Handle, SceneError> {
    let raw = first_argument(node).ok_or_else(|| SceneError::OpFailed {
        op: op.to_string(),
        message: "missing `@handle` argument".to_string(),
    })?;
    parse_handle(&raw, op)
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

/// Detect whether `spawn @h …` carries the `overlay` keyword.
///
/// `overlay` is a bare positional word in the KDL shape (`spawn @h
/// overlay pos=… size=… { … }`) — it appears as a second unnamed
/// argument whose string value is literally `"overlay"`.
fn is_overlay(node: &KdlNode) -> bool {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .skip(1) // skip the `@handle` argument
        .any(|e| e.value().as_string() == Some("overlay"))
}

/// Render the node's children block as a KDL-formatted string so the
/// mux layer can splice it into the spawn command verbatim.
///
/// Returns `None` when the node has no children — callers pass `None`
/// through to [`crate::intent::MuxHandle::spawn_pane`].
fn view_body(node: &KdlNode) -> Option<String> {
    node.children().map(|doc| doc.to_string())
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

/// `spawn @handle [overlay pos=… size=…] { <view> }` — create a pane.
///
/// If the handle is already live in the mux, the op focuses the
/// existing pane instead of spawning a duplicate (T-055
/// check-then-create-else-focus). Fail-fast on mux-side errors
/// unrelated to "handle already exists".
#[derive(Debug, Default)]
pub struct SpawnOp;

const SPAWN_NAME: &str = "ark.core.spawn";

#[async_trait]
impl Intent for SpawnOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, SPAWN_NAME)?;
        let handle = require_handle(args, SPAWN_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        let overlay = is_overlay(args);
        let body = view_body(args);
        if mux.handle_exists(&handle) {
            tracing::info!(
                target: "scene::ops",
                op = SPAWN_NAME,
                handle = %handle.raw(),
                overlay,
                origin = %ctx.origin,
                "spawn: handle exists, focusing"
            );
            return strict_map(SPAWN_NAME, mux.focus_pane(&handle));
        }
        tracing::info!(
            target: "scene::ops",
            op = SPAWN_NAME,
            handle = %handle.raw(),
            overlay,
            origin = %ctx.origin,
            "spawn"
        );
        strict_map(
            SPAWN_NAME,
            mux.spawn_pane(&handle, overlay, body.as_deref()),
        )
    }
}

// ---------------------------------------------------------------------------
// new_tab
// ---------------------------------------------------------------------------

/// `new_tab @handle [name="…"] [cwd="…"]` — create a tab.
///
/// Follows the same check-then-create-else-focus policy as
/// [`SpawnOp`].
#[derive(Debug, Default)]
pub struct NewTabOp;

const NEW_TAB_NAME: &str = "ark.core.new_tab";

#[async_trait]
impl Intent for NewTabOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, NEW_TAB_NAME)?;
        let handle = require_handle(args, NEW_TAB_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        let name = property_str(args, "name");
        let cwd = property_str(args, "cwd");
        if mux.handle_exists(&handle) {
            tracing::info!(
                target: "scene::ops",
                op = NEW_TAB_NAME,
                handle = %handle.raw(),
                origin = %ctx.origin,
                "new_tab: handle exists, focusing"
            );
            return strict_map(NEW_TAB_NAME, mux.focus_tab(&handle));
        }
        tracing::info!(
            target: "scene::ops",
            op = NEW_TAB_NAME,
            handle = %handle.raw(),
            name = ?name,
            cwd = ?cwd,
            origin = %ctx.origin,
            "new_tab"
        );
        strict_map(
            NEW_TAB_NAME,
            mux.new_tab(&handle, name.as_deref(), cwd.as_deref()),
        )
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
    async fn new_tab_creates_when_absent() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"new_tab "@review" name="review" cwd="/tmp""#);
        let v = NewTabOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
        let calls = mux.take_calls();
        assert_eq!(
            calls,
            vec![r#"new_tab(@review,name=Some("review"),cwd=Some("/tmp"))"#.to_string()]
        );
    }

    #[tokio::test]
    async fn new_tab_focuses_when_exists() {
        let mux = Arc::new(MockMux::default());
        mux.mark_existing("@review");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"new_tab "@review" name="review""#);
        NewTabOp.dispatch(&node, &ctx).await.expect("ok");
        let calls = mux.take_calls();
        // Should take the focus-existing branch instead of new_tab.
        assert_eq!(calls, vec!["focus_tab(@review)".to_string()]);
    }

    #[tokio::test]
    async fn spawn_tiled_creates_pane() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn "@editor" { shell }"#);
        SpawnOp.dispatch(&node, &ctx).await.expect("ok");
        let calls = mux.take_calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("spawn_pane(@editor,overlay=false"));
    }

    #[tokio::test]
    async fn spawn_overlay_sets_overlay_flag() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn "@palette" "overlay" pos="top-right" size="60%x40%" { command }"#);
        SpawnOp.dispatch(&node, &ctx).await.expect("ok");
        let calls = mux.take_calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("overlay=true"));
    }

    #[tokio::test]
    async fn spawn_existing_handle_focuses() {
        let mux = Arc::new(MockMux::default());
        mux.mark_existing("@editor");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn "@editor" { shell }"#);
        SpawnOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(mux.take_calls(), vec!["focus_pane(@editor)".to_string()]);
    }

    #[tokio::test]
    async fn spawn_surface_error_on_non_noop() {
        let mux = Arc::new(MockMux::default());
        mux.set_fail("out of memory");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn "@editor" { shell }"#);
        let err = SpawnOp.dispatch(&node, &ctx).await.expect_err("must surface");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "ark.core.spawn"));
    }

    #[tokio::test]
    async fn missing_handle_errors() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn"#);
        let err = SpawnOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }
}
