//! Scope-rule validation tests (T-013).
//!
//! These tests exercise `validate_scope` from `ark_scene::validate::scope`.
//!
//! ## Type-system coverage note
//!
//! Many invalid nesting patterns are *already prevented by the type system*:
//! - `SceneBodyNode` has no `Tab` variant → tab at scene root is a parse error.
//! - `LayoutNode.tabs` is `Vec<TabNode>` → only tabs at layout root.
//! - `LayoutChild` is `Row | Col | Pane` → no tab inside row/col.
//!
//! The scope validation pass is mostly a safety net for patterns that COULD
//! arise from `include` splicing (T-074) or future AST manipulation. Tests
//! here validate both the normal (parser-produced) path and the
//! direct-construction helpers for bypass scenarios.

use ark_scene::parse::parse_scene;
use ark_scene::validate::scope::{
    reject_layout_child_at_layout_root, reject_tab_outside_layout_or_mode, validate_scope,
};

/// A well-formed scene with layout, tabs, nested row/col/pane passes
/// scope validation with zero errors.
#[test]
fn valid_scene_passes_scope_check() {
    let src = r#"
scene "dev" {
    layout {
        tab "@main" focus="true" {
            row {
                pane "@editor" span=2
                col {
                    pane "@term"
                    pane "@logs"
                }
            }
        }
        tab "@aux" {
            pane "@scratch"
        }
    }
    on "FileEdited" when="true" {
        focus "@editor"
    }
    bind "Alt q" {
        close "@term"
    }
}
"#;
    let ir = parse_scene(src, "valid.kdl").expect("well-formed scene should parse");
    let errors = validate_scope(&ir);
    assert!(errors.is_empty(), "expected no scope errors, got: {errors:?}");
}

/// A scene with a mode block containing tabs also passes.
#[test]
fn valid_scene_with_mode_passes() {
    let src = r#"
scene "modes" {
    layout {
        tab "@main" {
            pane "@editor"
        }
    }
    mode "review" {
        tab "@review" {
            pane "@diff"
        }
    }
}
"#;
    let ir = parse_scene(src, "mode.kdl").expect("mode scene should parse");
    let errors = validate_scope(&ir);
    assert!(errors.is_empty(), "expected no scope errors, got: {errors:?}");
}

/// Deeply nested row/col/pane is valid — the scope pass should not reject
/// `layout { tab @t { col { row { pane @p { shell } } } } }`.
#[test]
fn nested_row_col_pane_valid() {
    let src = r#"
scene "nested" {
    layout {
        tab "@t" {
            col {
                row {
                    pane "@p"
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "nested.kdl").expect("nested layout should parse");
    let errors = validate_scope(&ir);
    assert!(errors.is_empty(), "expected no scope errors, got: {errors:?}");
}

/// An empty scene (no body) passes scope validation trivially.
#[test]
fn empty_scene_passes() {
    let src = r#"scene "empty" { }"#;
    let ir = parse_scene(src, "empty.kdl").expect("empty scene should parse");
    let errors = validate_scope(&ir);
    assert!(errors.is_empty(), "expected no scope errors, got: {errors:?}");
}

// -------------------------------------------------------------------------
// Direct-construction tests — bypass the parser's type-system guardrails
// -------------------------------------------------------------------------

/// If somehow a tab node appeared outside layout/mode (e.g. via include
/// splicing or AST manipulation), the helper rejects it.
///
/// Since `LayoutChild` has no `Tab` variant, we cannot construct this via
/// parse. Instead we test the `reject_tab_outside_layout_or_mode` helper
/// that would be called during include-splice validation (T-074).
#[test]
fn tab_outside_layout_rejected() {
    let src = "scene \"x\" { }";
    let mut errors = Vec::new();

    // Simulate a tab appearing inside a "row" parent (invalid).
    reject_tab_outside_layout_or_mode(true, "row", src, "test.kdl", &mut errors);
    assert_eq!(errors.len(), 1, "tab inside row should be rejected");

    match &errors[0] {
        ark_scene::SceneError::MisplacedNode { node, parent, .. } => {
            assert_eq!(node, "tab");
            assert_eq!(parent, "row");
        }
        other => panic!("expected MisplacedNode, got: {other:?}"),
    }

    // Simulate a tab inside "layout" (valid) — should not produce an error.
    let mut errors2 = Vec::new();
    reject_tab_outside_layout_or_mode(true, "layout", src, "test.kdl", &mut errors2);
    assert!(errors2.is_empty(), "tab inside layout is valid");

    // Simulate a tab inside "mode" (valid).
    let mut errors3 = Vec::new();
    reject_tab_outside_layout_or_mode(true, "mode", src, "test.kdl", &mut errors3);
    assert!(errors3.is_empty(), "tab inside mode is valid");
}

/// If a pane node appeared directly at layout root (bypassing the tab
/// wrapper), the helper rejects it. Since `LayoutNode.tabs` is
/// `Vec<TabNode>`, the parser prevents this; the helper exists for
/// direct-construction / include-splice scenarios.
#[test]
fn pane_at_layout_root_rejected() {
    let src = "scene \"x\" { layout { } }";
    let mut errors = Vec::new();

    reject_layout_child_at_layout_root("pane", src, "test.kdl", &mut errors);
    assert_eq!(errors.len(), 1, "pane at layout root should be rejected");

    match &errors[0] {
        ark_scene::SceneError::MisplacedNode { node, parent, .. } => {
            assert_eq!(node, "pane");
            assert_eq!(parent, "layout");
        }
        other => panic!("expected MisplacedNode, got: {other:?}"),
    }
}

/// Row at layout root (without tab wrapper) is likewise rejected.
#[test]
fn row_at_layout_root_rejected() {
    let src = "scene \"x\" { layout { } }";
    let mut errors = Vec::new();

    reject_layout_child_at_layout_root("row", src, "test.kdl", &mut errors);
    assert_eq!(errors.len(), 1, "row at layout root should be rejected");

    match &errors[0] {
        ark_scene::SceneError::MisplacedNode { node, parent, .. } => {
            assert_eq!(node, "row");
            assert_eq!(parent, "layout");
        }
        other => panic!("expected MisplacedNode, got: {other:?}"),
    }
}

/// `when=` on tab, pane, row, col, and ops is valid — make sure the scope
/// pass does not reject these. (The actual `when=` compilation is T-023.)
#[test]
fn when_on_valid_nodes_passes() {
    let src = r#"
scene "conditional" {
    layout {
        tab "@t" when="true" {
            row when="true" {
                pane "@p" when="true"
            }
            col when="true" {
                pane "@q" when="true"
            }
        }
    }
    on "FileEdited" when="true" {
        focus "@p"
    }
}
"#;
    let ir = parse_scene(src, "when.kdl").expect("when scene should parse");
    let errors = validate_scope(&ir);
    assert!(errors.is_empty(), "expected no scope errors for when= attrs, got: {errors:?}");
}
