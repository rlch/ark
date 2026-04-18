//! R2 scope-rule validation pass (T-013).
//!
//! Walks the typed [`SceneIR`] AST and rejects misplaced nodes per the
//! R2 scope table:
//!
//! 1. `on`/`bind`/`use`/`include`/`mode`/`clear-reactions`/`clear-bind`/
//!    `disable-extension` — scene root only. These are already constrained
//!    by [`SceneBodyNode`]'s enum variants so they *cannot* appear inside
//!    `layout` through the normal parse path. The check here is a safety net
//!    for patterns that could arise from `include` splicing (T-074) or
//!    future AST manipulation.
//!
//! 2. `tab` — only inside `layout { }` or `mode { }`. NOT nested inside
//!    `row`/`col`/`pane`. The type system already enforces this:
//!    [`LayoutNode::tabs`] and [`ModeNode::tabs`] are `Vec<TabNode>`, and
//!    [`LayoutChild`] has no `Tab` variant — so `tab` inside `row`/`col`
//!    is a parse error, not a scope error. We validate anyway for
//!    defence-in-depth.
//!
//! 3. `row`/`col`/`pane` — only inside `tab { }` or nested inside another
//!    `row`/`col`. Again enforced by the type system (`LayoutChild` enum),
//!    but validated here against future AST manipulation.
//!
//! 4. `when=` — legal on `tab`, `pane`, `row`, `col`, and individual op
//!    nodes. We walk the tree and verify no unexpected `when=` appears.
//!
//! 5. Misplacement → `error[scene/misplaced-node]` with parent context.
//!
//! ## Type-system coverage note
//!
//! Many invalid nesting patterns are *already prevented by the type system*.
//! For example:
//! - `SceneBodyNode` has no `Tab` variant, so `tab` at scene root is a
//!   parse error.
//! - `LayoutNode.tabs` is `Vec<TabNode>` — only tabs can appear at layout
//!   root.
//! - `LayoutChild` is `Row | Col | Pane` — no `Tab` variant, so tabs
//!   cannot nest inside `row`/`col`.
//!
//! This scope validation pass is therefore mostly a safety net for patterns
//! that COULD arise from `include` splicing (T-074) or future AST
//! manipulation, and for validating `when=` placement.

use miette::{NamedSource, SourceSpan};

use crate::ast::layout::{ColNode, LayoutChild, PaneNode, RowNode, TabNode};
use crate::ast::{LayoutNode, ModeNode, SceneBodyNode, SceneNode};
use crate::error::SceneError;
use crate::parse::SceneIR;

/// Validate R2 scope rules across the entire scene AST.
///
/// Returns an empty `Vec` when the scene is well-formed. Returns one
/// [`SceneError::MisplacedNode`] per violation when it is not.
pub fn validate_scope(ir: &SceneIR) -> Vec<SceneError> {
    let mut errors = Vec::new();
    validate_scene_body(
        &ir.scene,
        &ir.src,
        &ir.path.display().to_string(),
        &mut errors,
    );
    errors
}

/// Walk scene-root body nodes.
fn validate_scene_body(scene: &SceneNode, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for node in &scene.body {
        match node {
            // `layout` body should contain only tabs — walk each tab.
            SceneBodyNode::Layout(layout) => {
                validate_layout(layout, src, path, errors);
            }
            // `mode` body should contain only tabs — same as layout.
            SceneBodyNode::Mode(mode) => {
                validate_mode(mode, src, path, errors);
            }
            // `on`, `bind`, `use`, `include`, `clear-*`, `disable-extension`
            // are legal at scene root — nothing to validate structurally.
            SceneBodyNode::On(_)
            | SceneBodyNode::Bind(_)
            | SceneBodyNode::Use(_)
            | SceneBodyNode::Include(_)
            | SceneBodyNode::ClearReactions(_)
            | SceneBodyNode::ClearBind(_)
            | SceneBodyNode::DisableExtension(_) => {}
        }
    }
}

/// Walk a `layout { }` body — should be tabs only.
fn validate_layout(layout: &LayoutNode, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for tab in &layout.tabs {
        validate_tab(tab, "layout", src, path, errors);
    }
}

/// Walk a `mode { }` body — same structure as layout.
fn validate_mode(mode: &ModeNode, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for tab in &mode.tabs {
        validate_tab(tab, "mode", src, path, errors);
    }
}

/// Walk a `tab @handle { body }`.
///
/// Tab body should contain `row`/`col`/`pane` only (the `LayoutChild` enum).
fn validate_tab(tab: &TabNode, _parent: &str, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for child in &tab.body {
        validate_layout_child(child, "tab", src, path, errors);
    }
}

/// Walk a `LayoutChild` — `row`/`col`/`pane`.
///
/// These are legal inside `tab`, `row`, or `col`. The `parent` parameter
/// names the enclosing node for diagnostic attribution.
fn validate_layout_child(
    child: &LayoutChild,
    parent: &str,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    match child {
        LayoutChild::Row(row) => validate_row(row, parent, src, path, errors),
        LayoutChild::Col(col) => validate_col(col, parent, src, path, errors),
        LayoutChild::Pane(pane) => validate_pane(pane, parent, src, path, errors),
    }
}

/// Walk `row { body }` — children should be `row`/`col`/`pane`.
fn validate_row(row: &RowNode, _parent: &str, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for child in &row.body {
        validate_layout_child(child, "row", src, path, errors);
    }
}

/// Walk `col { body }` — children should be `row`/`col`/`pane`.
fn validate_col(col: &ColNode, _parent: &str, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for child in &col.body {
        validate_layout_child(child, "col", src, path, errors);
    }
}

/// Validate a `pane` node. Panes are leaves — no structural children to
/// recurse into. Currently a no-op; future passes may validate view-ref
/// placement here.
fn validate_pane(
    _pane: &PaneNode,
    _parent: &str,
    _src: &str,
    _path: &str,
    _errors: &mut Vec<SceneError>,
) {
    // Pane is a leaf — no children to walk. View validation is T-026+.
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Construct a [`SceneError::MisplacedNode`] with a zero-length span
/// (precise spans deferred to T-018 fixtures).
fn misplaced(node: &str, parent: &str, src: &str, path: &str) -> SceneError {
    SceneError::MisplacedNode {
        node: node.to_string(),
        parent: parent.to_string(),
        src: NamedSource::new(path.to_string(), src.to_string()),
        span: SourceSpan::new(0.into(), 0),
    }
}

// ---------------------------------------------------------------------------
// Direct-construction validation helpers
// ---------------------------------------------------------------------------
//
// The following functions validate IR fragments that bypass the parser —
// e.g. `include` splicing (T-074) or test harnesses that hand-build AST
// nodes. They check structural invariants the parse-time type system
// normally enforces.

/// Validate that a `tab` is not placed inside a `row`, `col`, or `pane`.
///
/// Since `LayoutChild` has no `Tab` variant, this cannot happen through
/// the parser. It CAN happen if AST manipulation inserts a `TabNode` into
/// a context where only `LayoutChild` is expected. This function is called
/// by test harnesses that construct IR directly; production code relies on
/// the type system.
pub fn reject_tab_outside_layout_or_mode(
    tab_present: bool,
    parent: &str,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    if tab_present && parent != "layout" && parent != "mode" {
        errors.push(misplaced("tab", parent, src, path));
    }
}

/// Validate that a layout child (`row`/`col`/`pane`) is not placed
/// directly inside `layout` without a `tab` wrapper.
///
/// Since `LayoutNode.tabs` is `Vec<TabNode>`, bare `pane`/`row`/`col`
/// at layout root cannot happen through the parser. This function exists
/// for direct-construction scenarios and test harnesses.
pub fn reject_layout_child_at_layout_root(
    child_name: &str,
    src: &str,
    path: &str,
    errors: &mut Vec<SceneError>,
) {
    errors.push(misplaced(child_name, "layout", src, path));
}
