//! Spawn ops — T-049, R7.
//!
//! * [`SpawnOp`]      — `spawn @handle { <view> }` (tiled) or
//!                       `spawn @handle overlay pos=… size=… { <view> }`
//!                       (overlay).
//! * [`NewTabOp`]     — `new_tab @handle [name=…] [cwd=…]`.
//! * [`SpawnIntoOp`]  — scene-2026-04-18 T-022 — `spawn_into @stack
//!                       { <view> }` mints a fresh child pane under
//!                       `@stack` with an ark-generated
//!                       `<stack>-<ulid>` identity (R-7).
//!
//! `spawn` / `new_tab` follow the T-055 "check-then-create-else-focus"
//! policy: when the handle already exists the op focuses the existing
//! target rather than failing or re-creating.
//!
//! `spawn_into` is NON-idempotent per R-7: every call on a stack
//! meaningfully pushes another child, so re-dispatch must not be
//! elided. The strict-map surfaces mux errors verbatim.

use ark_view::HandleId;
use async_trait::async_trait;
use kdl::KdlNode;

use crate::error::SceneError;
use crate::intent::{
    Intent, IntentContext, IntentValue, first_argument, parse_handle, property_str, strict_map,
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
// spawn_into — scene-2026-04-18 T-022
// ---------------------------------------------------------------------------

/// `spawn_into @stack { <view> }` — mint a new child pane inside the
/// stack identified by `@stack`.
///
/// Semantics per R-7:
/// * First positional arg is the `@stack` handle.
/// * Optional body block carries the pane's view content (same shape
///   as `spawn`'s view child).
/// * Non-idempotent: every dispatch meaningfully pushes another child,
///   so no "check-then-focus" branch. Errors from the mux surface via
///   [`strict_map`].
/// * The returned [`HandleId`] (from [`MuxHandle::spawn_into_stack`])
///   is the ark-minted `<stack>-<ulid>` child id. That id is NOT
///   recorded in the compile-time `ViewTable` — the table is scene
///   source + layout ground-truth, which doesn't know about runtime
///   children. The dispatcher logs it through tracing so `ark scene
///   explain` can chase it.
#[derive(Debug, Default)]
pub struct SpawnIntoOp;

const SPAWN_INTO_NAME: &str = "ark.core.spawn_into";

#[async_trait]
impl Intent for SpawnIntoOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        require_mux(ctx, SPAWN_INTO_NAME)?;
        // Pull the first positional argument — the `@stack` handle ref.
        let raw = first_argument(args).ok_or_else(|| SceneError::OpFailed {
            op: SPAWN_INTO_NAME.to_string(),
            message: "missing `@stack` argument".to_string(),
        })?;
        if raw.is_empty() {
            return Err(SceneError::OpFailed {
                op: SPAWN_INTO_NAME.to_string(),
                message: "empty `@stack` argument".to_string(),
            });
        }
        let mux = ctx.mux.as_ref().expect("checked by require_mux");
        let stack = HandleId::new(raw.clone());
        let body = view_body(args);
        tracing::info!(
            target: "scene::ops",
            op = SPAWN_INTO_NAME,
            stack = %raw,
            origin = %ctx.origin,
            "spawn_into"
        );
        match mux.spawn_into_stack(&stack, body.as_deref()) {
            Ok(child) => {
                tracing::info!(
                    target: "scene::ops",
                    op = SPAWN_INTO_NAME,
                    stack = %raw,
                    child = %child.as_str(),
                    "spawn_into minted child"
                );
                Ok(IntentValue::String(child.as_str().to_string()))
            }
            Err(msg) => Err(SceneError::OpFailed {
                op: SPAWN_INTO_NAME.to_string(),
                message: msg,
            }),
        }
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
        let node =
            node_from(r#"spawn "@palette" "overlay" pos="top-right" size="60%x40%" { command }"#);
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
        let err = SpawnOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("must surface");
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

    // -----------------------------------------------------------------
    // scene-2026-04-18 T-022 — SpawnIntoOp
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn spawn_into_dispatches_to_mux_and_returns_child_id() {
        let mux = Arc::new(MockMux::default());
        // Pin the ULID portion so the child id is fully deterministic.
        mux.set_child_ulid("01jabcdefghijklmnopqrstuv");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn_into "@subs" { command }"#);
        let v = SpawnIntoOp.dispatch(&node, &ctx).await.expect("ok");
        // Returned value is the minted child id.
        match v {
            IntentValue::String(s) => {
                assert_eq!(s, "@subs-01jabcdefghijklmnopqrstuv");
            }
            other => panic!("expected IntentValue::String, got {other:?}"),
        }
        // Mux recorded the call with the body preserved.
        let calls = mux.take_calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].starts_with("spawn_into_stack(@subs,view="),
            "unexpected call: {calls:?}"
        );
        // Child id list tracks the mint.
        assert_eq!(
            mux.take_child_ids(),
            vec!["@subs-01jabcdefghijklmnopqrstuv".to_string()]
        );
    }

    #[tokio::test]
    async fn spawn_into_missing_handle_errors() {
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn_into"#);
        let err = SpawnIntoOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn spawn_into_surfaces_mux_error_strictly() {
        // R-7 non-idempotent — even an "absent" error must surface.
        let mux = Arc::new(MockMux::default());
        mux.set_fail("handle not found");
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn_into "@subs" { command }"#);
        let err = SpawnIntoOp
            .dispatch(&node, &ctx)
            .await
            .expect_err("must surface");
        assert!(matches!(
            err,
            SceneError::OpFailed { op, .. } if op == "ark.core.spawn_into"
        ));
    }

    #[tokio::test]
    async fn spawn_into_non_idempotent_double_call() {
        // Two back-to-back dispatches must both reach the mux — this
        // is the R-7 non-idempotent contract in action.
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn_into "@subs" { command }"#);
        SpawnIntoOp.dispatch(&node, &ctx).await.expect("ok");
        SpawnIntoOp.dispatch(&node, &ctx).await.expect("ok");
        let calls = mux.take_calls();
        assert_eq!(calls.len(), 2, "both calls must reach the mux: {calls:?}");
        assert_eq!(mux.take_child_ids().len(), 2);
    }

    #[tokio::test]
    async fn spawn_into_child_id_default_is_lowercase_ulid() {
        // Without the override, the generated ulid must be 26 chars
        // long and entirely lowercase (R-7 formatting).
        let mux = Arc::new(MockMux::default());
        let ctx = ctx_with(mux.clone());
        let node = node_from(r#"spawn_into "@subs" { command }"#);
        let v = SpawnIntoOp.dispatch(&node, &ctx).await.expect("ok");
        let s = match v {
            IntentValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        // Expected format: `@subs-<26 chars of lowercase base32>`.
        let prefix = "@subs-";
        assert!(s.starts_with(prefix), "unexpected child id: {s}");
        let ulid_part = &s[prefix.len()..];
        assert_eq!(
            ulid_part.len(),
            26,
            "ulid part must be 26 chars: got {ulid_part:?}"
        );
        for ch in ulid_part.chars() {
            assert!(
                ch.is_ascii_digit() || (ch.is_ascii_alphabetic() && ch.is_ascii_lowercase()),
                "ulid must be lowercase ascii alnum, got {ch:?} in {ulid_part:?}"
            );
        }
    }
}
