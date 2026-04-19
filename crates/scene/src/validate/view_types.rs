//! View-type validator (scene-2026-04-18 T-018).
//!
//! Walks every op body + view attr that carries a typed handle ref
//! (per-view facet SHAPE) and cross-checks the referenced `@handle`
//! against the scene-local [`crate::compile::view_table::ViewTable`].
//! Declared-expected-view ≠ resolved `ViewDecl` surfaces as
//! [`SceneError::ViewTypeMismatch`] with a caret on the offending
//! `@handle` token in source.
//!
//! # Homogeneous-only (R-8)
//!
//! Exact-match semantics only — any union / `OneOf` / heterogeneous
//! ref is deferred to v0.2. The kit is explicit: a stack's declared
//! child view type is a SINGLE [`crate::view::ViewMeta`], and the
//! first `spawn_into @stack { <inner-view> }` validates its inner
//! view's resolved meta name against the stack's declared meta name
//! via string equality.
//!
//! # Scope for v0.1 core
//!
//! Core ops (R7 #1–13) carry NO view-type expectations on their handle
//! arguments today — `focus` / `close` are polymorphic across
//! {tab, pane, stack}, and `resize` / `move` / `pin` / `unpin` are
//! pane-shape-only (covered by [`crate::validate::op_refs`]). The only
//! view-type check this pass emits today is the `spawn_into @stack
//! { <view> }` inner-view vs stack-declared-type check — the rest of
//! the scaffold is for v0.1 extensions (e.g. claude-code's
//! `subagents=@stack-of-X`) to grow into without a second rewrite.
//!
//! # Lookup discipline (R-10)
//!
//! ViewTable lookups go through
//! [`crate::compile::CompiledScene::view_of_internal`] — a
//! crate-private single-handle accessor. The public
//! [`crate::intent::IntentContext::view_of`] is runtime-only and
//! requires an `IntentContext` that doesn't exist during scene
//! compilation, so this pass must NOT call it.
//!
//! # Diagnostic ordering
//!
//! Walk order is textual (KDL doc order), so diagnostics surface in a
//! stable sequence matching the source file. The underlying
//! [`ViewTable`] itself is a `BTreeMap` for deterministic iteration
//! when we need to look at it from a different angle (e.g. future
//! coverage reports), but THIS pass never iterates the table — it
//! only does keyed `view_of_internal` lookups.

use kdl::{KdlDocument, KdlEntry, KdlNode};
use miette::{NamedSource, SourceSpan};

use crate::compile::{CompiledScene, ViewDecl};
use crate::error::SceneError;
use crate::view::ViewRegistry;
use ark_view::{HandleId, HandleKind};

/// Validate that every typed handle reference in the scene resolves to
/// a [`ViewDecl`] whose view meta matches the reference's declared
/// expected view.
///
/// Today this pass emits exactly one family of diagnostic — the
/// `spawn_into @stack { <view> }` inner-view vs stack-declared-type
/// mismatch (scene-2026-04-18 R-8 homogeneous-only). Unknown `@stack`
/// handles are NOT this pass's concern; the
/// [`crate::validate::op_refs`] pass owns `scene/op-unresolved-ref`.
/// Stacks with no [`ViewDecl`] entry (e.g. empty-body source stacks)
/// are treated as "type not yet determined" — the FIRST `spawn_into`
/// that lands on them would populate runtime state, so this pass
/// silently skips them to match the compile-time table's behaviour
/// (see `build_view_table` in [`crate::compile`]).
///
/// Returns an empty `Vec` when every typed ref resolves cleanly.
/// Emits one diagnostic per mismatch otherwise.
pub fn validate_view_types(compiled: &CompiledScene, registry: &ViewRegistry) -> Vec<SceneError> {
    let mut errors = Vec::new();
    let Some(doc) = compiled.ir.kdl_doc.as_ref() else {
        // No raw KDL document — upstream kdl crate rejected the input
        // even though facet-kdl accepted it. Other passes surface the
        // parse error; this pass stays silent (same policy as
        // `pane_views.rs`).
        return errors;
    };
    let path = compiled.ir.path.display().to_string();
    let src = &compiled.ir.src;
    walk_document(doc, compiled, registry, src, &path, &mut errors);
    errors
}

// ---------------------------------------------------------------------------
// KDL walking
// ---------------------------------------------------------------------------

/// Recursively walk every node in a KDL document looking for ops that
/// carry typed view-type references (currently only `spawn_into`).
fn walk_document(
    doc: &KdlDocument,
    compiled: &CompiledScene,
    registry: &ViewRegistry,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for node in doc.nodes() {
        walk_node(node, compiled, registry, src, path, errors);
    }
}

/// Recurse into a single KDL node. When the node's name matches one of
/// the recognised typed-ref-carrying verbs, dispatch to the
/// corresponding check. Either way, descend into any child document so
/// nested ops (inside `on` / `bind` / `mode` / `layout`) are reached.
fn walk_node(
    node: &KdlNode,
    compiled: &CompiledScene,
    registry: &ViewRegistry,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    match node.name().value() {
        "spawn_into" => check_spawn_into(node, compiled, registry, src, path, errors),
        _ => {}
    }
    if let Some(children) = node.children() {
        walk_document(children, compiled, registry, src, path, errors);
    }
}

// ---------------------------------------------------------------------------
// spawn_into @stack { <view> } inner-view check (T-018 + T-019 lean-in)
// ---------------------------------------------------------------------------

/// Validate a single `spawn_into @stack { <inner-view> }` op.
///
/// Three possible outcomes:
/// 1. `@stack` not in the view table → skip (either the handle is
///    unknown — handled by [`crate::validate::op_refs`] — or the stack
///    has no declared child view type yet, in which case there's
///    nothing to check against).
/// 2. Inner view alias not in the registry → skip (the unknown-view
///    diagnostic is T-031's responsibility, not this pass's).
/// 3. Stack's declared view meta != inner view's resolved meta (by
///    `name`) → emit [`SceneError::ViewTypeMismatch`] with the caret
///    on the `@stack` handle token.
fn check_spawn_into(
    node: &KdlNode,
    compiled: &CompiledScene,
    registry: &ViewRegistry,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    // Pull the first positional argument — the `@stack` handle ref.
    let Some(handle_entry) = first_positional(node) else {
        return;
    };
    let Some(raw_handle) = handle_entry.value().as_string() else {
        return;
    };
    if raw_handle.is_empty() {
        return;
    }

    // Pull the first child node — the inner view's alias name.
    let Some(children) = node.children() else {
        return;
    };
    let Some(inner_view_node) = children.nodes().first() else {
        return;
    };
    let inner_alias = inner_view_node.name().value();

    // Look up `@stack` in the scene-local view table via the
    // crate-private single-handle accessor (R-10 — compile-pipeline
    // lookups never go through the public runtime `view_of`).
    let stack_decl: &ViewDecl = match compiled.view_of_internal(&HandleId::new(raw_handle)) {
        Some(decl) => decl,
        None => return,
    };

    // The handle MUST resolve to a stack for this op to make sense; a
    // pane-kind handle is a separate diagnostic owned by T-019
    // (`scene/op-handle-type-mismatch` via op_refs.rs). Skip silently
    // here so we don't double-emit.
    if stack_decl.kind != HandleKind::Stack {
        return;
    }

    // Look up the inner view alias in the registry. Unknown-alias
    // diagnostics belong to T-031; skip silently otherwise we'd
    // double-report.
    let Some(inner_meta) = registry.resolve(inner_alias) else {
        return;
    };

    // Exact-match semantics per R-8 homogeneous-only.
    if stack_decl.view_meta.name == inner_meta.name {
        return;
    }

    errors.push(SceneError::ViewTypeMismatch {
        op: "spawn_into".to_string(),
        attr: "stack".to_string(),
        expected_view: stack_decl.view_meta.name.clone(),
        actual_view: inner_meta.name.clone(),
        src: NamedSource::new(path.to_string(), src.to_string()),
        span: handle_span(handle_entry),
    });
}

// ---------------------------------------------------------------------------
// Span helpers
// ---------------------------------------------------------------------------

/// Return the first unnamed positional entry on `node`, or `None` when
/// the node carries no positional argument.
fn first_positional(node: &KdlNode) -> Option<&KdlEntry> {
    node.entries().iter().find(|e| e.name().is_none())
}

/// Extract a usable `SourceSpan` from a KDL entry's raw span. Entries
/// without a recorded span (synthetic AST construction in tests) fall
/// back to a zero-length placeholder — the variant's label still
/// renders but without a caret.
fn handle_span(entry: &KdlEntry) -> SourceSpan {
    let span = entry.span();
    SourceSpan::new(span.offset().into(), span.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile_scene_with_registry;
    use crate::parse::parse_scene;
    use crate::rhai::Engine;

    /// Compile `src` with the supplied registry; panic on any error.
    fn compile(src: &str, registry: &ViewRegistry) -> CompiledScene {
        let ir = parse_scene(src, "test.kdl").expect("parse ok");
        let engine = Engine::new();
        compile_scene_with_registry(&engine, ir, registry).expect("compile ok")
    }

    #[test]
    fn spawn_into_matching_view_passes() {
        // Stack declares its child as `command`; spawn_into pushes a
        // `command` — must pass without diagnostic.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "FileEdited" {
        spawn_into "@subs" { command }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert!(errors.is_empty(), "expected no diagnostics, got {errors:?}");
    }

    #[test]
    fn spawn_into_wrong_view_emits_mismatch() {
        // Stack declares `command`; spawn_into pushes a `shell` — must
        // emit ViewTypeMismatch with expected="command" actual="shell".
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "FileEdited" {
        spawn_into "@subs" { shell }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert_eq!(errors.len(), 1, "expected 1 mismatch, got {errors:?}");
        match &errors[0] {
            SceneError::ViewTypeMismatch {
                op,
                expected_view,
                actual_view,
                ..
            } => {
                assert_eq!(op, "spawn_into");
                assert_eq!(expected_view, "command");
                assert_eq!(actual_view, "shell");
            }
            other => panic!("expected ViewTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn spawn_into_unknown_stack_handle_is_silent() {
        // Unknown `@ghost` — op_refs.rs owns `op-unresolved-ref`; this
        // pass must not double-emit.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "FileEdited" {
        spawn_into "@ghost" { shell }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert!(
            errors.is_empty(),
            "unknown handle must not surface view-type mismatch"
        );
    }

    #[test]
    fn spawn_into_unknown_inner_view_is_silent() {
        // Inner view alias unknown — T-031 owns `scene/unknown-view`;
        // this pass must not double-emit.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "FileEdited" {
        spawn_into "@subs" { mystery_view }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert!(
            errors.is_empty(),
            "unknown inner view must not surface view-type mismatch"
        );
    }

    #[test]
    fn spawn_into_pane_kind_handle_is_silent() {
        // `@editor` is a pane, not a stack. This is a kind mismatch
        // (op_refs.rs territory), not a view-type mismatch — so this
        // pass stays silent.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@editor" { command }
        }
    }
    on "FileEdited" {
        spawn_into "@editor" { shell }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert!(
            errors.is_empty(),
            "pane-kind handle for spawn_into must not surface view-type mismatch"
        );
    }

    #[test]
    fn no_spawn_into_no_diagnostics() {
        // A scene with no spawn_into ops has nothing for this pass to
        // check — it stays silent.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@editor" { command }
        }
    }
    on "FileEdited" {
        focus "@editor"
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn spawn_into_inside_bind_is_checked() {
        // The walker must descend into `bind` bodies — same ops live
        // there too.
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    bind "Alt q" {
        spawn_into "@subs" { shell }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert_eq!(
            errors.len(),
            1,
            "bind body spawn_into must be checked, got {errors:?}"
        );
    }

    #[test]
    fn diagnostic_code_is_view_type_mismatch() {
        use miette::Diagnostic;
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "FileEdited" {
        spawn_into "@subs" { shell }
    }
}
"#;
        let reg = ViewRegistry::with_primitives();
        let cs = compile(src, &reg);
        let errors = validate_view_types(&cs, &reg);
        assert_eq!(errors.len(), 1);
        let code = errors[0].code().map(|c| c.to_string()).unwrap_or_default();
        assert_eq!(code, "scene/view-type-mismatch");
    }
}
