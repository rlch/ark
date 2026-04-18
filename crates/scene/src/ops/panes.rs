//! Pane / tab ops — T-048, R7.
//!
//! Handle-addressed ops that target an existing pane or tab:
//!
//! * [`FocusOp`]  — `focus @handle` (polymorphic tab-or-pane)
//! * [`CloseOp`]  — `close @handle` (polymorphic)
//! * [`RenameOp`] — `rename @handle to="…"` (tab only)
//! * [`ResizeOp`] — `resize @handle direction=… by=…` (pane only)
//! * [`MoveOp`]   — `move @handle to=…` (pane only)
//! * [`PinOp`]    — `pin @handle` (overlay pane)
//! * [`UnpinOp`]  — `unpin @handle` (overlay pane)
//!
//! All seven ops are classified as "noop on absent handle" per the
//! T-055 idempotency matrix: a mux error whose body contains
//! `"not found"` maps to `Ok(IntentValue::None)`. Any other error
//! surfaces as [`SceneError::OpFailed`].
//!
//! Handle-type resolution (tab vs pane) for the polymorphic `focus` /
//! `close` ops is driven by [`IntentContext::handle_type_hint`] — the
//! compile pipeline attaches the hint when it resolves the `@handle`
//! reference against the layout's declaration. When the hint is absent
//! (extension-registered reactions that bypass the compile pass), the
//! ops default to the pane branch because panes vastly outnumber tabs
//! in practice.

use async_trait::async_trait;
use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{
    HandleKind, Intent, IntentContext, IntentValue, first_argument, idempotent_map, parse_handle,
    property_str,
};

// ---------------------------------------------------------------------------
// Small helper: extract the `@handle` first argument.
// ---------------------------------------------------------------------------

/// Pull `@handle` off `node`'s first positional argument, surfacing a
/// clean [`SceneError::OpFailed`] when the argument is missing or
/// malformed. The compile-time op-ref validator (T-052) catches
/// declared-but-wrong refs earlier; this guard is defence-in-depth for
/// ops dispatched outside the compile pipeline (extension reactions).
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

/// Require a mux handle on the context, surfacing a clean
/// [`SceneError::OpFailed`] when it's absent. Scenes running outside a
/// real session (tests, `ark scene check`) hit this branch.
fn require_mux(ctx: &IntentContext, op: &'static str) -> Result<(), SceneError> {
    if ctx.mux.is_some() {
        Ok(())
    } else {
        Err(SceneError::OpFailed {
            op: op.to_string(),
            message: "mux handle not wired (scene-less agent?)".to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// focus — polymorphic (tab or pane)
// ---------------------------------------------------------------------------

/// `focus @handle` — transfer focus to the referenced tab or pane.
///
/// Tab-vs-pane resolution follows
/// [`IntentContext::handle_type_hint`]. Idempotent: focusing an absent
/// handle silently succeeds.
#[derive(Debug, Default)]
pub struct FocusOp;

const FOCUS_NAME: &str = "ark.core.focus";

#[async_trait]
impl Intent for FocusOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, FOCUS_NAME)?;
        let handle = require_handle(args, FOCUS_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        // TODO(T-054): apply `{Rhai}` interpolation to handle when
        // extensions grow parameterized-handle syntax.
        let result = match ctx.handle_type_hint {
            Some(HandleKind::Tab) => mux.focus_tab(&handle),
            // Pane / Command / Plugin — all pane-shaped from the mux
            // perspective. The typed distinction is a Tier-10 compile
            // concern.
            _ => mux.focus_pane(&handle),
        };
        tracing::info!(
            target: "scene::ops",
            op = FOCUS_NAME,
            handle = %handle.raw(),
            origin = %ctx.origin,
            "focus"
        );
        idempotent_map(FOCUS_NAME, result)
    }
}

// ---------------------------------------------------------------------------
// close — polymorphic
// ---------------------------------------------------------------------------

/// `close @handle` — close the referenced tab or pane.
#[derive(Debug, Default)]
pub struct CloseOp;

const CLOSE_NAME: &str = "ark.core.close";

#[async_trait]
impl Intent for CloseOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, CLOSE_NAME)?;
        let handle = require_handle(args, CLOSE_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        let result = match ctx.handle_type_hint {
            Some(HandleKind::Tab) => mux.close_tab(&handle),
            _ => mux.close_pane(&handle),
        };
        tracing::info!(
            target: "scene::ops",
            op = CLOSE_NAME,
            handle = %handle.raw(),
            origin = %ctx.origin,
            "close"
        );
        idempotent_map(CLOSE_NAME, result)
    }
}

// ---------------------------------------------------------------------------
// rename — tab only
// ---------------------------------------------------------------------------

/// `rename @handle to="name"` — rename a tab.
///
/// Tab-only at compile time per R7. Pane handles are rejected by the
/// op-reference validator (T-052) before dispatch; dispatching with a
/// pane hint still routes through `rename_tab` and returns whatever
/// error the mux surfaces.
#[derive(Debug, Default)]
pub struct RenameOp;

const RENAME_NAME: &str = "ark.core.rename";

#[async_trait]
impl Intent for RenameOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, RENAME_NAME)?;
        let handle = require_handle(args, RENAME_NAME)?;
        let to = property_str(args, "to").ok_or_else(|| SceneError::OpFailed {
            op: RENAME_NAME.to_string(),
            message: "missing required property `to=`".to_string(),
        })?;
        // TODO(T-054): render `to` through event-scope Rhai holes. The
        // raw value is passed through today because the reactions
        // dispatcher (Tier 6) pre-renders string args before calling
        // dispatch.
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = RENAME_NAME,
            handle = %handle.raw(),
            to = %to,
            origin = %ctx.origin,
            "rename"
        );
        idempotent_map(RENAME_NAME, mux.rename_tab(&handle, &to))
    }
}

// ---------------------------------------------------------------------------
// resize — pane only
// ---------------------------------------------------------------------------

/// `resize @handle direction=<dir> by=<inc|dec>` — pane-only resize.
///
/// Direction / magnitude strings are forwarded verbatim to the mux;
/// deep validation (accepted direction set, inc/dec grammar) lives in
/// T-052 so diagnostics attach to the scene source span.
#[derive(Debug, Default)]
pub struct ResizeOp;

const RESIZE_NAME: &str = "ark.core.resize";

#[async_trait]
impl Intent for ResizeOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, RESIZE_NAME)?;
        let handle = require_handle(args, RESIZE_NAME)?;
        let direction = property_str(args, "direction").ok_or_else(|| SceneError::OpFailed {
            op: RESIZE_NAME.to_string(),
            message: "missing required property `direction=`".to_string(),
        })?;
        let by = property_str(args, "by").ok_or_else(|| SceneError::OpFailed {
            op: RESIZE_NAME.to_string(),
            message: "missing required property `by=`".to_string(),
        })?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = RESIZE_NAME,
            handle = %handle.raw(),
            direction = %direction,
            by = %by,
            origin = %ctx.origin,
            "resize"
        );
        idempotent_map(RESIZE_NAME, mux.resize_pane(&handle, &direction, &by))
    }
}

// ---------------------------------------------------------------------------
// move — pane only
// ---------------------------------------------------------------------------

/// `move @handle to=<anchor>` — reposition an overlay / pane.
#[derive(Debug, Default)]
pub struct MoveOp;

const MOVE_NAME: &str = "ark.core.move";

#[async_trait]
impl Intent for MoveOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, MOVE_NAME)?;
        let handle = require_handle(args, MOVE_NAME)?;
        let to = property_str(args, "to").ok_or_else(|| SceneError::OpFailed {
            op: MOVE_NAME.to_string(),
            message: "missing required property `to=`".to_string(),
        })?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = MOVE_NAME,
            handle = %handle.raw(),
            to = %to,
            origin = %ctx.origin,
            "move"
        );
        idempotent_map(MOVE_NAME, mux.move_pane(&handle, &to))
    }
}

// ---------------------------------------------------------------------------
// pin — overlay pane
// ---------------------------------------------------------------------------

/// `pin @handle` — pin an overlay pane so it survives tab switch.
#[derive(Debug, Default)]
pub struct PinOp;

const PIN_NAME: &str = "ark.core.pin";

#[async_trait]
impl Intent for PinOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, PIN_NAME)?;
        let handle = require_handle(args, PIN_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = PIN_NAME,
            handle = %handle.raw(),
            origin = %ctx.origin,
            "pin"
        );
        idempotent_map(PIN_NAME, mux.pin_pane(&handle))
    }
}

// ---------------------------------------------------------------------------
// unpin — overlay pane
// ---------------------------------------------------------------------------

/// `unpin @handle` — unpin a previously pinned overlay.
#[derive(Debug, Default)]
pub struct UnpinOp;

const UNPIN_NAME: &str = "ark.core.unpin";

#[async_trait]
impl Intent for UnpinOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, UNPIN_NAME)?;
        let handle = require_handle(args, UNPIN_NAME)?;
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        tracing::info!(
            target: "scene::ops",
            op = UNPIN_NAME,
            handle = %handle.raw(),
            origin = %ctx.origin,
            "unpin"
        );
        idempotent_map(UNPIN_NAME, mux.unpin_pane(&handle))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::tests::{MockBus, MockMux, node_from, test_scene_id};
    use crate::intent::{HandleKind, IntentContext};
    use std::sync::Arc;

    fn ctx_with_mux(mux: Arc<MockMux>, hint: Option<HandleKind>) -> IntentContext {
        let bus = Arc::new(MockBus::default());
        let mut ctx = IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus);
        if let Some(h) = hint {
            ctx = ctx.with_handle_type_hint(h);
        }
        ctx
    }

    #[tokio::test]
    async fn focus_routes_to_pane_by_default() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"focus "@editor""#);
        let v = FocusOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
        let calls = mux.take_calls();
        assert_eq!(calls, vec!["focus_pane(@editor)".to_string()]);
    }

    #[tokio::test]
    async fn focus_routes_to_tab_when_hinted() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), Some(HandleKind::Tab));
        let node = node_from(r#"focus "@main""#);
        FocusOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(mux.take_calls(), vec!["focus_tab(@main)".to_string()]);
    }

    #[tokio::test]
    async fn focus_noop_on_absent_handle() {
        let mux = Arc::new(MockMux::default());
        mux.set_fail("pane not found");
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"focus "@ghost""#);
        let v = FocusOp.dispatch(&node, &ctx).await.expect("idempotent ok");
        assert_eq!(v, IntentValue::None);
    }

    #[tokio::test]
    async fn focus_surfaces_non_absent_error() {
        let mux = Arc::new(MockMux::default());
        mux.set_fail("mux disconnected");
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"focus "@editor""#);
        let err = FocusOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("non-absent must surface");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "ark.core.focus"));
    }

    #[tokio::test]
    async fn close_routes_per_hint() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), Some(HandleKind::Tab));
        let node = node_from(r#"close "@main""#);
        CloseOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(mux.take_calls(), vec!["close_tab(@main)".to_string()]);
    }

    #[tokio::test]
    async fn rename_passes_name_through() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), Some(HandleKind::Tab));
        let node = node_from(r#"rename "@main" to="review""#);
        RenameOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(
            mux.take_calls(),
            vec!["rename_tab(@main,review)".to_string()]
        );
    }

    #[tokio::test]
    async fn rename_missing_to_errors() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), Some(HandleKind::Tab));
        let node = node_from(r#"rename "@main""#);
        let err = RenameOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn resize_forwards_direction_and_by() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"resize "@p" direction="up" by="inc""#);
        ResizeOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(mux.take_calls(), vec!["resize_pane(@p,up,inc)".to_string()]);
    }

    #[tokio::test]
    async fn move_forwards_anchor() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"move "@p" to="top-right""#);
        MoveOp.dispatch(&node, &ctx).await.expect("ok");
        assert_eq!(
            mux.take_calls(),
            vec!["move_pane(@p,top-right)".to_string()]
        );
    }

    #[tokio::test]
    async fn pin_and_unpin_dispatch() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), None);
        let pin_node = node_from(r#"pin "@overlay""#);
        let unpin_node = node_from(r#"unpin "@overlay""#);
        PinOp.dispatch(&pin_node, &ctx).await.expect("ok");
        UnpinOp.dispatch(&unpin_node, &ctx).await.expect("ok");
        let calls = mux.take_calls();
        assert_eq!(
            calls,
            vec![
                "pin_pane(@overlay)".to_string(),
                "unpin_pane(@overlay)".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn missing_handle_errors() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with_mux(mux.clone(), None);
        let node = node_from(r#"focus"#);
        let err = FocusOp.dispatch(&node, &ctx).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "ark.core.focus"));
    }

    #[tokio::test]
    async fn op_without_mux_errors() {
        let ctx = IntentContext::new(test_scene_id(), "scene");
        let node = node_from(r#"focus "@x""#);
        let err = FocusOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("no mux -> error");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "ark.core.focus"));
    }
}
