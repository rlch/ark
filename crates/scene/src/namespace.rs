//! Namespace enforcement pass (T-078 / R11).
//!
//! All intent and event names in a compiled scene must use the
//! `<owner>.<name>` dot-separated format. This module walks the AST and:
//!
//! 1. **Rewrites** unprefixed names according to context:
//!    - `NamespaceContext::User` → `user.<name>`
//!    - `NamespaceContext::Extension(ext)` → `<ext>.<name>`
//! 2. **Validates** that no rewritten or already-prefixed name collides
//!    with the reserved `ark.core.*` namespace.
//!
//! The pass is intended to run after include resolution (T-074) but before
//! the full compile pipeline. It mutates the AST in place.

use crate::ast::ops::OpNode;
use crate::ast::{OnNode, SceneBodyNode, SceneNode};
use crate::error::SceneError;

/// Reserved namespace prefix. Any name starting with `ark.core.` is
/// reserved for host-owned ops and events.
const RESERVED_PREFIX: &str = "ark.core.";

/// Determines how unprefixed names are rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceContext {
    /// Top-level user scene — unprefixed names become `user.<name>`.
    User,
    /// Extension fragment — unprefixed names become `<ext>.<name>`.
    Extension(String),
}

impl NamespaceContext {
    /// Returns the owner prefix for this context (e.g. `"user"` or the
    /// extension name).
    fn owner(&self) -> &str {
        match self {
            NamespaceContext::User => "user",
            NamespaceContext::Extension(name) => name.as_str(),
        }
    }

    /// Label used in error messages to identify the source context.
    fn label(&self) -> &str {
        match self {
            NamespaceContext::User => "user",
            NamespaceContext::Extension(name) => name.as_str(),
        }
    }
}

/// Returns `true` when `name` already contains a dot, meaning it is
/// already namespace-qualified.
fn is_qualified(name: &str) -> bool {
    name.contains('.')
}

/// Qualify `name` under the given context's owner if it is not already
/// qualified. Returns the (possibly rewritten) name.
fn qualify(name: &str, ctx: &NamespaceContext) -> String {
    if is_qualified(name) {
        name.to_string()
    } else {
        format!("{}.{}", ctx.owner(), name)
    }
}

/// Check that `fqn` does not fall under the reserved `ark.core.*`
/// namespace. Returns `Err(ExtReservedNamespace)` on collision.
fn check_reserved(fqn: &str, ctx: &NamespaceContext) -> Result<(), SceneError> {
    if fqn.starts_with(RESERVED_PREFIX) || fqn == "ark.core" {
        return Err(SceneError::ExtReservedNamespace {
            ext: ctx.label().to_string(),
            attempted: fqn.to_string(),
        });
    }
    Ok(())
}

/// Qualify and validate a single name. Returns the rewritten name or an
/// error if the resulting FQN collides with `ark.core.*`.
fn rewrite_name(name: &str, ctx: &NamespaceContext) -> Result<String, SceneError> {
    let fqn = qualify(name, ctx);
    check_reserved(&fqn, ctx)?;
    Ok(fqn)
}

/// Walk an op list and rewrite `emit` event names in place.
fn rewrite_ops(ops: &mut [OpNode], ctx: &NamespaceContext) -> Result<(), SceneError> {
    for op in ops.iter_mut() {
        if let OpNode::Emit(emit_op) = op {
            emit_op.event_name = rewrite_name(&emit_op.event_name, ctx)?;
        }
    }
    Ok(())
}

/// Walk an `on` node: rewrite the selector's event kind and any `emit`
/// ops in the body.
fn rewrite_on(on: &mut OnNode, ctx: &NamespaceContext) -> Result<(), SceneError> {
    // Rewrite the event selector kind if present.
    if let Some(selector) = &mut on.selector {
        selector.kind = rewrite_name(&selector.kind, ctx)?;
    }
    // Rewrite ops inside the on block.
    rewrite_ops(&mut on.ops, ctx)
}

/// Walk the body nodes of a scene and apply namespacing rewrites.
fn rewrite_body(body: &mut [SceneBodyNode], ctx: &NamespaceContext) -> Result<(), SceneError> {
    for node in body.iter_mut() {
        match node {
            SceneBodyNode::On(on) => {
                rewrite_on(on, ctx)?;
            }
            SceneBodyNode::Bind(bind) => {
                rewrite_ops(&mut bind.ops, ctx)?;
            }
            SceneBodyNode::ClearReactions(cr) => {
                // The selector string is the raw event kind/selector.
                // Rewrite the leading event-kind portion if unprefixed.
                // Format: `<EventKind> [field=pat ...]` — we only touch
                // the first whitespace-delimited token.
                let parts: Vec<&str> = cr.selector.splitn(2, char::is_whitespace).collect();
                let kind = parts[0];
                let rest = parts.get(1);
                let new_kind = rewrite_name(kind, ctx)?;
                cr.selector = match rest {
                    Some(r) => format!("{new_kind} {r}"),
                    None => new_kind,
                };
            }
            // Other body nodes (Use, Include, Layout, Mode, ClearBind,
            // DisableExtension) carry no intent/event names.
            _ => {}
        }
    }
    Ok(())
}

/// Apply namespace enforcement to a scene AST.
///
/// Walks all body nodes, rewriting unprefixed intent/event names
/// according to `context` and rejecting any name that collides with the
/// reserved `ark.core.*` namespace.
///
/// # Errors
///
/// Returns [`SceneError::ExtReservedNamespace`] if any resulting
/// fully-qualified name starts with `ark.core.`.
pub fn apply_namespacing(
    scene: &mut SceneNode,
    context: &NamespaceContext,
) -> Result<(), SceneError> {
    rewrite_body(&mut scene.body, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::ops::{EmitOp, FocusOp};
    use crate::ast::selector::EventSelector;
    use crate::ast::{BindNode, ClearReactionsNode, OnNode, SceneBodyNode, SceneNode};
    use std::collections::BTreeMap;

    /// Helper: build a minimal SceneNode with given body nodes.
    fn scene_with(body: Vec<SceneBodyNode>) -> SceneNode {
        SceneNode {
            name: "test".to_string(),
            max_cascade_depth: None,
            body,
        }
    }

    /// Helper: build an OnNode with a selector kind and ops.
    fn on_node(kind: &str, ops: Vec<OpNode>) -> OnNode {
        OnNode {
            selector: Some(EventSelector {
                kind: kind.to_string(),
                field_patterns: BTreeMap::new(),
            }),
            when: None,
            ops,
        }
    }

    /// Helper: build an emit op.
    fn emit(name: &str) -> OpNode {
        OpNode::Emit(EmitOp {
            event_name: name.to_string(),
            payload: None,
            when: None,
        })
    }

    // ── Unprefixed emit in User context ─────────────────────────────

    #[test]
    fn user_context_rewrites_unprefixed_emit() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![emit(
            "foo",
        )]))]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "user.foo");
            } else {
                panic!("expected Emit op");
            }
        } else {
            panic!("expected On node");
        }
    }

    // ── Unprefixed emit in Extension context ────────────────────────

    #[test]
    fn extension_context_rewrites_unprefixed_emit() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![emit(
            "bar",
        )]))]);
        apply_namespacing(&mut scene, &NamespaceContext::Extension("status".into())).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "status.bar");
            } else {
                panic!("expected Emit op");
            }
        } else {
            panic!("expected On node");
        }
    }

    // ── Already-prefixed name passes through unchanged ──────────────

    #[test]
    fn already_prefixed_emit_unchanged() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![emit(
            "ext.ready",
        )]))]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "ext.ready");
            } else {
                panic!("expected Emit op");
            }
        } else {
            panic!("expected On node");
        }
    }

    // ── ark.core.* rejection on emit ────────────────────────────────

    #[test]
    fn rejects_ark_core_emit() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![emit(
            "ark.core.init",
        )]))]);
        let err = apply_namespacing(&mut scene, &NamespaceContext::User).unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { ext, attempted } => {
                assert_eq!(ext, "user");
                assert_eq!(attempted, "ark.core.init");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    #[test]
    fn rejects_ark_core_emit_in_extension() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![emit(
            "ark.core.ready",
        )]))]);
        let err =
            apply_namespacing(&mut scene, &NamespaceContext::Extension("evil".into())).unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { ext, attempted } => {
                assert_eq!(ext, "evil");
                assert_eq!(attempted, "ark.core.ready");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    // ── Unprefixed name that would become ark.core.* is rejected ────

    #[test]
    fn rejects_unprefixed_that_becomes_reserved() {
        // Extension named "ark.core" + unprefixed "init" → "ark.core.init"
        // Use an already-qualified selector kind so we hit the emit rewrite.
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node(
            "other.SomeEvent",
            vec![emit("init")],
        ))]);
        let err = apply_namespacing(
            &mut scene,
            &NamespaceContext::Extension("ark.core".into()),
        )
        .unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { attempted, .. } => {
                assert_eq!(attempted, "ark.core.init");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    #[test]
    fn rejects_unprefixed_selector_that_becomes_reserved() {
        // Extension named "ark.core" + unprefixed selector → "ark.core.SomeEvent"
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("SomeEvent", vec![]))]);
        let err = apply_namespacing(
            &mut scene,
            &NamespaceContext::Extension("ark.core".into()),
        )
        .unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { attempted, .. } => {
                assert_eq!(attempted, "ark.core.SomeEvent");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    // ── On-node selector kind is rewritten ──────────────────────────

    #[test]
    fn rewrites_on_selector_kind() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("MyEvent", vec![]))]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            assert_eq!(on.selector.as_ref().unwrap().kind, "user.MyEvent");
        } else {
            panic!("expected On node");
        }
    }

    #[test]
    fn already_prefixed_selector_kind_unchanged() {
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node("ext.Ready", vec![]))]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            assert_eq!(on.selector.as_ref().unwrap().kind, "ext.Ready");
        } else {
            panic!("expected On node");
        }
    }

    // ── Bind ops are rewritten ──────────────────────────────────────

    #[test]
    fn rewrites_emit_in_bind() {
        let mut scene = scene_with(vec![SceneBodyNode::Bind(BindNode {
            chord: "Alt d".to_string(),
            ops: vec![emit("toggle")],
        })]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::Bind(ref bind) = scene.body[0] {
            if let OpNode::Emit(ref e) = bind.ops[0] {
                assert_eq!(e.event_name, "user.toggle");
            } else {
                panic!("expected Emit op");
            }
        } else {
            panic!("expected Bind node");
        }
    }

    // ── ClearReactions selector is rewritten ────────────────────────

    #[test]
    fn rewrites_clear_reactions_selector() {
        let mut scene = scene_with(vec![SceneBodyNode::ClearReactions(ClearReactionsNode {
            selector: "MyEvent path=**/*.md".to_string(),
        })]);
        apply_namespacing(&mut scene, &NamespaceContext::Extension("git".into())).unwrap();

        if let SceneBodyNode::ClearReactions(ref cr) = scene.body[0] {
            assert_eq!(cr.selector, "git.MyEvent path=**/*.md");
        } else {
            panic!("expected ClearReactions node");
        }
    }

    #[test]
    fn rewrites_clear_reactions_bare_kind() {
        let mut scene = scene_with(vec![SceneBodyNode::ClearReactions(ClearReactionsNode {
            selector: "MyEvent".to_string(),
        })]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::ClearReactions(ref cr) = scene.body[0] {
            assert_eq!(cr.selector, "user.MyEvent");
        } else {
            panic!("expected ClearReactions node");
        }
    }

    // ── Non-emit ops are untouched ──────────────────────────────────

    #[test]
    fn non_emit_ops_pass_through() {
        let focus = OpNode::Focus(FocusOp {
            handle: "@main".to_string(),
            when: None,
        });
        let mut scene = scene_with(vec![SceneBodyNode::On(on_node(
            "ext.Ready",
            vec![focus, emit("notify")],
        ))]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            // Focus op unchanged.
            if let OpNode::Focus(ref f) = on.ops[0] {
                assert_eq!(f.handle, "@main");
            } else {
                panic!("expected Focus op");
            }
            // Emit rewritten.
            if let OpNode::Emit(ref e) = on.ops[1] {
                assert_eq!(e.event_name, "user.notify");
            } else {
                panic!("expected Emit op");
            }
        } else {
            panic!("expected On node");
        }
    }

    // ── Multiple body nodes all rewritten ───────────────────────────

    #[test]
    fn multiple_body_nodes_all_rewritten() {
        let mut scene = scene_with(vec![
            SceneBodyNode::On(on_node("Evt1", vec![emit("a")])),
            SceneBodyNode::On(on_node("Evt2", vec![emit("b")])),
            SceneBodyNode::Bind(BindNode {
                chord: "Alt x".to_string(),
                ops: vec![emit("c")],
            }),
        ]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        // Check all three were rewritten.
        if let SceneBodyNode::On(ref on) = scene.body[0] {
            assert_eq!(on.selector.as_ref().unwrap().kind, "user.Evt1");
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "user.a");
            }
        }
        if let SceneBodyNode::On(ref on) = scene.body[1] {
            assert_eq!(on.selector.as_ref().unwrap().kind, "user.Evt2");
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "user.b");
            }
        }
        if let SceneBodyNode::Bind(ref bind) = scene.body[2] {
            if let OpNode::Emit(ref e) = bind.ops[0] {
                assert_eq!(e.event_name, "user.c");
            }
        }
    }

    // ── Empty scene is fine ─────────────────────────────────────────

    #[test]
    fn empty_scene_succeeds() {
        let mut scene = scene_with(vec![]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();
        assert!(scene.body.is_empty());
    }

    // ── On node with no selector (None) is fine ─────────────────────

    #[test]
    fn on_node_without_selector_rewrites_ops() {
        let mut scene = scene_with(vec![SceneBodyNode::On(OnNode {
            selector: None,
            when: None,
            ops: vec![emit("ping")],
        })]);
        apply_namespacing(&mut scene, &NamespaceContext::User).unwrap();

        if let SceneBodyNode::On(ref on) = scene.body[0] {
            assert!(on.selector.is_none());
            if let OpNode::Emit(ref e) = on.ops[0] {
                assert_eq!(e.event_name, "user.ping");
            }
        }
    }
}
