//! Op reference validation (T-052).
//!
//! Walks every `on { ops }` and `bind { ops }` body, checking that each
//! op's `@handle` argument resolves to a tab or pane declared in the
//! layout (or any mode) of the same scene. Also enforces the R7
//! handle-type rules:
//!
//! * `focus` / `close`        — polymorphic: accept either tab or pane.
//! * `rename` / `new_tab`     — tab-only.
//! * `resize` / `move`        — pane-only.
//! * `pin`   / `unpin`        — pane-only (overlay pane in practice;
//!                              the AST layer does not distinguish).
//!
//! Unresolved refs surface as [`SceneError::OpUnresolvedRef`] with a
//! "did you mean `@X`?" suggestion built via [`crate::suggest`]. Type
//! mismatches surface as [`SceneError::OpHandleTypeMismatch`].
//!
//! Declarations are collected from the scene's `layout { }` block AND
//! every `mode { }` block so ops that target mode-introduced handles
//! still validate.

use std::collections::HashMap;

use miette::{NamedSource, SourceSpan};

use crate::ast::layout::{ColNode, LayoutChild, PaneNode, RowNode, StackNode, TabNode};
use crate::ast::ops::OpNode;
use crate::ast::{BindNode, LayoutNode, ModeNode, OnNode, SceneBodyNode};
use crate::error::SceneError;
use crate::parse::SceneIR;
use crate::suggest::{format_suggestions, suggest};

/// Handle classification collected during the declaration walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Tab,
    Pane,
    /// scene-2026-04-18 T-011: stack handles share the flat namespace
    /// with tab + pane for handle-clash detection and join the
    /// `@handle` resolver so ops that accept polymorphic targets
    /// (focus/close + future stack-specific ops) can route via
    /// [`HandleKind`].
    Stack,
}

impl DeclKind {
    fn as_str(self) -> &'static str {
        match self {
            DeclKind::Tab => "tab",
            DeclKind::Pane => "pane",
            DeclKind::Stack => "stack",
        }
    }
}

/// Validate op handle references + handle-type rules across the entire
/// scene AST.
///
/// Returns an empty `Vec` when every op's handle references resolve
/// and match the required kind. Emits one diagnostic per unresolved
/// ref + one per type mismatch otherwise.
pub fn validate_op_refs(ir: &SceneIR) -> Vec<SceneError> {
    let mut errors = Vec::new();
    let decls = collect_declarations(&ir.scene.body);
    let path = ir.path.display().to_string();
    let src = &ir.src;

    for node in &ir.scene.body {
        match node {
            SceneBodyNode::On(on) => walk_on(on, &decls, src, &path, &mut errors),
            SceneBodyNode::Bind(bind) => walk_bind(bind, &decls, src, &path, &mut errors),
            _ => {}
        }
    }

    // scene-2026-04-18 T-019: walk the raw KDL doc once more to pick
    // up `spawn_into @stack` / `clear @stack` ops, which land in the
    // typed AST as `OpNode::Unknown` and are therefore skipped by the
    // `op_metadata`-driven pass above. Pulling them off the raw KDL
    // gives us both the positional handle arg AND a span for the
    // caret that the opaque `OpNode::Unknown.args` doesn't carry.
    //
    // T-053 (scene-v3 S-D): the same raw walker also emits
    // `scene/unknown-op` diagnostics for ANY op verb outside the known
    // vocabulary — this catches typos like `fokus` or removed verbs
    // that facet-kdl routes into `OpNode::Unknown` without a diagnostic.
    // `OpNode::Unknown` carries no span (foreign KdlDocument is
    // `#[facet(opaque)]`), so the raw pass is the only path with
    // caret-accurate error labels.
    if let Some(doc) = ir.kdl_doc.as_ref() {
        walk_stack_ops_raw(doc, &decls, src, &path, &mut errors);
        walk_unknown_ops_raw(doc, src, &path, &mut errors);
    }

    errors
}

/// Set of op verbs (unqualified, no `ark.core.` prefix) that the typed
/// [`OpNode`] enum understands. Any verb outside this set — plus the
/// stack ops handled by [`walk_stack_ops_raw`] — surfaces as
/// `scene/unknown-op` via [`walk_unknown_ops_raw`].
///
/// Kept sorted by source location of the matching `OpNode` arm so that
/// adding a verb mechanically updates the set.
const KNOWN_OP_VERBS: &[&str] = &[
    "focus",
    "close",
    "rename",
    "resize",
    "move",
    "pin",
    "unpin",
    "spawn",
    "new_tab",
    "use_mode",
    "pipe",
    "emit",
    "set_status",
    "exec",
    "reload_scene",
    // scene-2026-04-18 stack ops — handled via walk_stack_ops_raw for
    // handle-ref validation, but still KNOWN for unknown-op diagnosis.
    "spawn_into",
    "clear",
];

/// Walk every op node inside `on`/`bind` blocks and emit
/// `scene/unknown-op` for any verb not in [`KNOWN_OP_VERBS`]. The
/// walker descends through `on`/`bind` containers (whose own names
/// aren't ops) but treats anything inside their bodies as an op
/// position. Siblings of ops (e.g. `when="…"` attrs) are KDL
/// properties, not child nodes, so they never trigger this path.
fn walk_unknown_ops_raw(
    doc: &::kdl::KdlDocument,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for node in doc.nodes() {
        let name = node.name().value();
        // Recurse into `on`/`bind` bodies — their children ARE op
        // positions. Everything else at the top level of the walk is
        // layout/mode/reactions scaffolding that this pass does not
        // inspect.
        if name == "on" || name == "bind" {
            if let Some(body) = node.children() {
                check_ops_in_body(body, src, path, errors);
            }
        }
        // Also recurse via the top-level walker so nested `on`/`bind`
        // under `mode { }` blocks reach the op-position check.
        if let Some(children) = node.children() {
            if name != "on" && name != "bind" {
                walk_unknown_ops_raw(children, src, path, errors);
            }
        }
    }
}

/// Check each child node of an `on`/`bind` body: if its verb is not in
/// [`KNOWN_OP_VERBS`], emit `scene/unknown-op` with a "did you mean …?"
/// suggestion rendered via [`crate::suggest::suggest`] + the full
/// `KNOWN_OP_VERBS` list for discoverability.
fn check_ops_in_body(
    body: &::kdl::KdlDocument,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for op in body.nodes() {
        let verb = op.name().value();
        if KNOWN_OP_VERBS.contains(&verb) {
            // Known op. Nested `on`/`bind` aren't legal inside an op
            // body per grammar (caught by scope.rs), so no recursion
            // needed here.
            continue;
        }
        // Unknown verb — synthesize a diagnostic. `suggest` surfaces
        // the closest match via Jaro-Winkler (T-015); fall back to
        // listing the full vocabulary if no suggestion clears the
        // threshold.
        let suggestion = suggest(verb, KNOWN_OP_VERBS, 0.75, 3);
        let hints = format_suggestions(&suggestion);
        let help = if hints.is_empty() {
            format!("available ops: {}", KNOWN_OP_VERBS.join(", "))
        } else {
            format!("{hints}. available ops: {}", KNOWN_OP_VERBS.join(", "))
        };
        let span: SourceSpan = op.name().span().into();
        errors.push(SceneError::UnknownOp {
            op: verb.to_string(),
            help,
            src: NamedSource::new(path.to_string(), src.to_string()),
            span,
        });
    }
}

// ---------------------------------------------------------------------------
// Declaration collection
// ---------------------------------------------------------------------------

/// Walk layout + mode blocks and build the `@handle → DeclKind` map.
fn collect_declarations(body: &[SceneBodyNode]) -> HashMap<String, DeclKind> {
    let mut decls = HashMap::new();
    for node in body {
        match node {
            SceneBodyNode::Layout(layout) => collect_layout(layout, &mut decls),
            SceneBodyNode::Mode(mode) => collect_mode(mode, &mut decls),
            _ => {}
        }
    }
    decls
}

fn collect_layout(layout: &LayoutNode, decls: &mut HashMap<String, DeclKind>) {
    for tab in &layout.tabs {
        collect_tab(tab, decls);
    }
}

fn collect_mode(mode: &ModeNode, decls: &mut HashMap<String, DeclKind>) {
    for tab in &mode.tabs {
        collect_tab(tab, decls);
    }
}

fn collect_tab(tab: &TabNode, decls: &mut HashMap<String, DeclKind>) {
    if !tab.handle.is_empty() {
        decls.insert(tab.handle.clone(), DeclKind::Tab);
    }
    for child in &tab.body {
        collect_layout_child(child, decls);
    }
}

fn collect_layout_child(child: &LayoutChild, decls: &mut HashMap<String, DeclKind>) {
    match child {
        LayoutChild::Row(row) => collect_row(row, decls),
        LayoutChild::Col(col) => collect_col(col, decls),
        LayoutChild::Pane(pane) => collect_pane(pane, decls),
        LayoutChild::Stack(stack) => collect_stack(stack, decls),
    }
}

fn collect_row(row: &RowNode, decls: &mut HashMap<String, DeclKind>) {
    for child in &row.body {
        collect_layout_child(child, decls);
    }
}

fn collect_col(col: &ColNode, decls: &mut HashMap<String, DeclKind>) {
    for child in &col.body {
        collect_layout_child(child, decls);
    }
}

fn collect_pane(pane: &PaneNode, decls: &mut HashMap<String, DeclKind>) {
    if !pane.handle.is_empty() {
        decls.insert(pane.handle.clone(), DeclKind::Pane);
    }
}

fn collect_stack(stack: &StackNode, decls: &mut HashMap<String, DeclKind>) {
    if !stack.handle.is_empty() {
        decls.insert(stack.handle.clone(), DeclKind::Stack);
    }
    for child in &stack.body {
        collect_layout_child(child, decls);
    }
}

// ---------------------------------------------------------------------------
// Op walking
// ---------------------------------------------------------------------------

fn walk_on(
    on: &OnNode,
    decls: &HashMap<String, DeclKind>,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for op in &on.ops {
        validate_op(op, decls, src, path, errors);
    }
}

fn walk_bind(
    bind: &BindNode,
    decls: &HashMap<String, DeclKind>,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for op in &bind.ops {
        validate_op(op, decls, src, path, errors);
    }
}

/// Expected handle kind for an op.
#[derive(Debug, Clone, Copy)]
enum ExpectedKind {
    /// Tab-only (e.g. `rename`, `new_tab`).
    Tab,
    /// Pane-only (e.g. `resize`, `move`, `pin`, `unpin`).
    Pane,
    /// Stack-only (scene-2026-04-18 T-019 — `spawn_into`, `clear`).
    Stack,
    /// Polymorphic (either tab or pane accepted: `focus`, `close`).
    Any,
    /// Op carries no handle reference — skip the validation entirely.
    None,
    /// Op carries a handle but the kind is unconstrained by R7 (e.g.
    /// `spawn` / `new_tab` where the handle introduces a NEW name).
    /// These are validated by the handles pass (T-014), not here.
    Introduce,
}

fn op_metadata(op: &OpNode) -> (&'static str, Option<&str>, ExpectedKind) {
    match op {
        OpNode::Focus(o) => ("focus", Some(&o.handle), ExpectedKind::Any),
        OpNode::Close(o) => ("close", Some(&o.handle), ExpectedKind::Any),
        OpNode::Rename(o) => ("rename", Some(&o.handle), ExpectedKind::Tab),
        OpNode::Resize(o) => ("resize", Some(&o.handle), ExpectedKind::Pane),
        OpNode::Move(o) => ("move", Some(&o.handle), ExpectedKind::Pane),
        OpNode::Pin(o) => ("pin", Some(&o.handle), ExpectedKind::Pane),
        OpNode::Unpin(o) => ("unpin", Some(&o.handle), ExpectedKind::Pane),
        // `spawn` / `new_tab` INTRODUCE a handle — validated by the
        // handles pass (T-014) for clash detection, not by this pass.
        OpNode::Spawn(o) => ("spawn", Some(&o.handle), ExpectedKind::Introduce),
        OpNode::NewTab(o) => ("new_tab", Some(&o.handle), ExpectedKind::Introduce),
        // Use-mode / pipe / emit / set_status / exec /
        // reload_scene carry no direct handle ref in their first
        // argument. Pipe's `from=`/`to=` are pane handles but live as
        // properties — we validate them specially below.
        OpNode::UseMode(_) => ("use_mode", None, ExpectedKind::None),
        OpNode::Pipe(_) => ("pipe", None, ExpectedKind::None),
        OpNode::Emit(_) => ("emit", None, ExpectedKind::None),
        OpNode::SetStatus(_) => ("set_status", None, ExpectedKind::None),
        OpNode::Exec(_) => ("exec", None, ExpectedKind::None),
        OpNode::ReloadScene(_) => ("reload_scene", None, ExpectedKind::None),
        // scene-2026-04-18 T-019: `spawn_into` / `clear` aren't in the
        // facet-derived `OpNode` enum yet (AST tier task pending) — they
        // reach `validate_op_refs` through the `Unknown` catch-all and
        // are handled specially via [`walk_stack_ops_raw`] which
        // re-parses them off the raw KDL doc for both positional-arg
        // access and span-accurate diagnostics. The typed path here
        // treats them as "no handle ref to validate" to avoid a
        // double-emit when the raw pass also fires.
        OpNode::Unknown { .. } => ("unknown", None, ExpectedKind::None),
    }
}

fn validate_op(
    op: &OpNode,
    decls: &HashMap<String, DeclKind>,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    let (name, handle, expected) = op_metadata(op);
    if let (Some(raw_handle), expected_kind) = (handle, expected) {
        match expected_kind {
            ExpectedKind::None | ExpectedKind::Introduce => {}
            ExpectedKind::Tab | ExpectedKind::Pane | ExpectedKind::Stack | ExpectedKind::Any => {
                validate_handle_ref(name, raw_handle, expected_kind, decls, src, path, errors);
            }
        }
    }

    // `pipe from=@h to=@h` — both sides must be pane refs.
    if let OpNode::Pipe(p) = op {
        validate_handle_ref(
            "pipe",
            &p.from,
            ExpectedKind::Pane,
            decls,
            src,
            path,
            errors,
        );
        validate_handle_ref("pipe", &p.to, ExpectedKind::Pane, decls, src, path, errors);
    }
}

fn validate_handle_ref(
    op: &'static str,
    raw: &str,
    expected: ExpectedKind,
    decls: &HashMap<String, DeclKind>,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    if raw.is_empty() {
        return; // Missing handle is caught by the per-op args parser.
    }
    match decls.get(raw) {
        None => {
            let names: Vec<&str> = decls.keys().map(|s| s.as_str()).collect();
            let hints = suggest(raw, &names, 0.70, 3);
            let help = if names.is_empty() {
                format!("no handles declared in this scene")
            } else {
                let list = names
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("available handles: {list}{}", format_suggestions(&hints))
            };
            errors.push(SceneError::OpUnresolvedRef {
                op: op.to_string(),
                kind: "handle".to_string(),
                name: raw.to_string(),
                help,
                src: NamedSource::new(path.to_string(), src.to_string()),
                span: SourceSpan::new(0.into(), 0),
            });
        }
        Some(actual_kind) => {
            let expected_str = match expected {
                ExpectedKind::Tab => Some("tab"),
                ExpectedKind::Pane => Some("pane"),
                ExpectedKind::Stack => Some("stack"),
                _ => None,
            };
            if let Some(expected_static) = expected_str {
                let matches = match (expected, actual_kind) {
                    (ExpectedKind::Tab, DeclKind::Tab) => true,
                    (ExpectedKind::Pane, DeclKind::Pane) => true,
                    (ExpectedKind::Stack, DeclKind::Stack) => true,
                    _ => false,
                };
                if !matches {
                    errors.push(SceneError::OpHandleTypeMismatch {
                        op: op.to_string(),
                        arg: "handle".to_string(),
                        handle: raw.to_string(),
                        expected: expected_static,
                        actual: actual_kind.as_str(),
                        src: NamedSource::new(path.to_string(), src.to_string()),
                        span: SourceSpan::new(0.into(), 0),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Raw-KDL walker for stack ops (scene-2026-04-18 T-019)
// ---------------------------------------------------------------------------

/// Walk the raw KDL document picking up `spawn_into` and `clear` op
/// nodes wherever they appear (inside `on { … }` / `bind { … }` / nested
/// in `mode { … }`). For each, pull the first positional argument (the
/// `@stack` handle ref) and validate it via [`validate_handle_ref`]
/// with [`ExpectedKind::Stack`].
///
/// The raw walker is necessary because these verbs aren't in the
/// facet-derived `OpNode` enum yet — they land as `OpNode::Unknown`,
/// and `OpNode::Unknown.args` stores only the BODY of the op, not the
/// op's positional arguments. Walking the raw doc sidesteps the
/// typed-AST gap cleanly and also gives us real spans for the
/// `@stack` token.
fn walk_stack_ops_raw(
    doc: &::kdl::KdlDocument,
    decls: &HashMap<String, DeclKind>,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    for node in doc.nodes() {
        let name = node.name().value();
        if name == "spawn_into" || name == "clear" {
            // Static verb string so the op-name field stays `'static`.
            let op_name: &'static str = if name == "spawn_into" {
                "spawn_into"
            } else {
                "clear"
            };
            if let Some(entry) = node.entries().iter().find(|e| e.name().is_none()) {
                if let Some(raw) = entry.value().as_string() {
                    validate_handle_ref(
                        op_name,
                        raw,
                        ExpectedKind::Stack,
                        decls,
                        src,
                        path,
                        errors,
                    );
                }
            }
        }
        if let Some(children) = node.children() {
            walk_stack_ops_raw(children, decls, src, path, errors);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn validate(src: &str) -> Vec<SceneError> {
        let ir = parse_scene(src, "test.kdl").expect("parse ok");
        validate_op_refs(&ir)
    }

    #[test]
    fn resolved_ref_passes() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@editor"
        }
    }
    on "FileEdited" { focus "@editor" }
}
"#;
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn unresolved_ref_errors() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "FileEdited" { focus "@ghost" }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpUnresolvedRef { op, name, help, .. } => {
                assert_eq!(op, "focus");
                assert_eq!(name, "@ghost");
                // Suggestion should contain the declared handles.
                assert!(help.contains("@editor") || help.contains("@main"));
            }
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
    }

    #[test]
    fn rename_on_pane_is_type_mismatch() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "FileEdited" { rename "@editor" to="foo" }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpHandleTypeMismatch {
                op,
                expected,
                actual,
                ..
            } => {
                assert_eq!(op, "rename");
                assert_eq!(*expected, "tab");
                assert_eq!(*actual, "pane");
            }
            other => panic!("expected OpHandleTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn resize_on_tab_is_type_mismatch() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "FileEdited" { resize "@main" direction="up" by="inc" }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpHandleTypeMismatch {
                op,
                expected,
                actual,
                ..
            } => {
                assert_eq!(op, "resize");
                assert_eq!(*expected, "pane");
                assert_eq!(*actual, "tab");
            }
            other => panic!("expected OpHandleTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn polymorphic_focus_accepts_tab_or_pane() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "TabOpened" { focus "@main" }
    on "FileEdited" { focus "@editor" }
}
"#;
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn bind_ops_are_validated() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    bind "Alt q" { focus "@ghost" }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        assert!(matches!(&errors[0], SceneError::OpUnresolvedRef { .. }));
    }

    #[test]
    fn pipe_validates_both_endpoints() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@a"
            pane "@b"
        }
    }
    on "FileEdited" { pipe from="@a" to="@ghost" payload="x" }
}
"#;
        let errors = validate(src);
        // Only the `to=@ghost` fails.
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpUnresolvedRef { name, .. } => assert_eq!(name, "@ghost"),
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
    }

    #[test]
    fn spawn_and_new_tab_do_not_require_prior_decl() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "FileEdited" {
        spawn "@new_pane" { shell }
        new_tab "@new_tab" name="x"
    }
}
"#;
        let errors = validate(src);
        assert!(
            errors.is_empty(),
            "introduced handles should not fail: {errors:?}"
        );
    }

    #[test]
    fn ops_without_handles_are_skipped() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" }
    }
    on "FileEdited" {
        emit "user.x"
        set_status text="hi"
        exec script="true"
        reload_scene
    }
}
"#;
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    // -----------------------------------------------------------------
    // scene-2026-04-18 T-019 — `spawn_into` / `clear` stack-kind checks
    // -----------------------------------------------------------------

    #[test]
    fn spawn_into_on_stack_passes() {
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
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn spawn_into_on_pane_is_type_mismatch() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@editor" { command }
        }
    }
    on "FileEdited" {
        spawn_into "@editor" { command }
    }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpHandleTypeMismatch {
                op,
                expected,
                actual,
                ..
            } => {
                assert_eq!(op, "spawn_into");
                assert_eq!(*expected, "stack");
                assert_eq!(*actual, "pane");
            }
            other => panic!("expected OpHandleTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn spawn_into_on_tab_is_type_mismatch() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            pane "@editor" { command }
        }
    }
    on "FileEdited" {
        spawn_into "@main" { command }
    }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpHandleTypeMismatch {
                op,
                expected,
                actual,
                ..
            } => {
                assert_eq!(op, "spawn_into");
                assert_eq!(*expected, "stack");
                assert_eq!(*actual, "tab");
            }
            other => panic!("expected OpHandleTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn spawn_into_unknown_handle_is_unresolved() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" { command } }
    }
    on "FileEdited" {
        spawn_into "@ghost" { command }
    }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpUnresolvedRef { op, name, .. } => {
                assert_eq!(op, "spawn_into");
                assert_eq!(name, "@ghost");
            }
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
    }

    #[test]
    fn clear_on_stack_passes() {
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
        clear "@subs"
    }
}
"#;
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn clear_on_pane_is_type_mismatch() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@editor" { command } }
    }
    on "FileEdited" {
        clear "@editor"
    }
}
"#;
        let errors = validate(src);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::OpHandleTypeMismatch {
                op,
                expected,
                actual,
                ..
            } => {
                assert_eq!(op, "clear");
                assert_eq!(*expected, "stack");
                assert_eq!(*actual, "pane");
            }
            other => panic!("expected OpHandleTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn stack_ops_in_bind_body_are_checked() {
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
        spawn_into "@subs" { command }
        clear "@subs"
    }
}
"#;
        let errors = validate(src);
        assert!(errors.is_empty(), "unexpected: {errors:?}");
    }

    #[test]
    fn unknown_op_verb_emits_diagnostic() {
        // T-053 (scene-v3 S-D): typo `fokus` is routed into
        // `OpNode::Unknown` by facet-kdl. The raw walker catches it
        // and emits `scene/unknown-op` with a "did you mean `focus`?"
        // hint rendered via suggest().
        let src = r#"
scene "s" {
    layout {
        tab "@main" { pane "@p" { command } }
    }
    bind "Alt x" {
        fokus "@p"
    }
}
"#;
        let errors = validate(src);
        let unknown = errors
            .iter()
            .find(|e| matches!(e, SceneError::UnknownOp { op, .. } if op == "fokus"))
            .expect("expected UnknownOp for `fokus`");
        match unknown {
            SceneError::UnknownOp { help, .. } => {
                assert!(
                    help.contains("focus"),
                    "expected `focus` suggestion in help: {help}"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn known_op_verb_does_not_emit_unknown_op() {
        // T-053 regression: every verb in KNOWN_OP_VERBS must NOT
        // trigger the diagnostic, even when typed-OpNode parsing
        // produces `Unknown` for some of them (spawn_into/clear).
        let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { command }
            }
        }
    }
    on "ark.core.pane_closed" {
        focus "@seed"
        close "@seed"
        spawn_into "@subs" { command }
        clear "@subs"
    }
}
"#;
        let errors = validate(src);
        let unknown_ops: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, SceneError::UnknownOp { .. }))
            .collect();
        assert!(
            unknown_ops.is_empty(),
            "known verbs triggered UnknownOp: {unknown_ops:?}"
        );
    }
}
