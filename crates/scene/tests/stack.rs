//! Integration tests for the `stack` layout primitive (scene-2026-04-18
//! T-005..T-008, T-011, T-012, T-025).
//!
//! Covers:
//!
//! - T-005/T-006: `stack @h { … }` parses into a `StackNode` under the
//!   `LayoutChild::Stack` variant.
//! - T-008: empty-body `stack @h { }` is legal.
//! - T-011: stack handles participate in the flat scene-scoped handle
//!   namespace (tab-vs-stack + pane-vs-stack clashes).
//! - T-012: `row` / `col` inside a stack body rejected via
//!   `error[scene/misplaced-node]`; child-level sizing attrs on stack
//!   children rejected via `error[scene/sizing-on-stack-child]` (R-9).
//! - T-025: stack renders in the zellij KDL emitter as
//!   `pane stacked=true { … }`.
//!
//! Compile-time view-type validator goldens (T-017..T-020) live in
//! `view_types_trybuild.rs` or dedicated miette snapshot tests
//! landed alongside T-027.

use ark_scene::ast::layout::{Handle, LayoutChild};
use ark_scene::ast::{LayoutNode, SceneBodyNode};
use ark_scene::compile::compile_layout_kdl;
use ark_scene::error::SceneError;
use ark_scene::parse_scene;
use ark_scene::validate::validate_scope;
use ark_scene::view::ViewRegistry;

/// Extract the first tab's body from a scene IR.
fn tab_body(ir: &ark_scene::parse::SceneIR) -> &Vec<LayoutChild> {
    for node in &ir.scene.body {
        if let SceneBodyNode::Layout(layout) = node {
            return &layout.tabs[0].body;
        }
    }
    panic!("expected a layout block")
}

/// Extract the first `layout { … }` block.
fn first_layout(ir: &ark_scene::parse::SceneIR) -> &LayoutNode {
    for node in &ir.scene.body {
        if let SceneBodyNode::Layout(layout) = node {
            return layout;
        }
    }
    panic!("expected a layout block")
}

/// Build a view registry that resolves the `shell` primitive alias so
/// test scenes with `pane { shell }` bodies pass through emitter.
fn registry_with_shell() -> ViewRegistry {
    let mut r = ViewRegistry::new();
    ark_scene::view::register_primitives(&mut r);
    r
}

#[test]
fn stack_parses_with_empty_body() {
    // T-005 / T-006 / T-008: empty stack body is legal; grammar
    // materialises a `StackNode` under `LayoutChild::Stack`.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@claude"
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("empty stack body should parse");
    let body = tab_body(&ir);
    assert_eq!(body.len(), 1);
    match &body[0] {
        LayoutChild::Stack(s) => {
            assert_eq!(s.handle, "@claude");
            assert!(
                s.body.is_empty(),
                "empty stack body should contain zero children"
            );
            assert!(s.when.is_none());
        }
        other => panic!("expected Stack, got {other:?}"),
    }
}

#[test]
fn stack_parses_with_pane_children() {
    // T-005 / T-006: heterogeneous children at the grammar level —
    // homogeneity is enforced by the view-type validator (T-018).
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@grid" span=2 {
                pane "@a" {
                    shell
                }
                pane "@b" {
                    shell
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("stack with panes should parse");
    let body = tab_body(&ir);
    match &body[0] {
        LayoutChild::Stack(s) => {
            assert_eq!(s.handle, "@grid");
            assert_eq!(s.span, Some(2));
            assert_eq!(s.body.len(), 2);
            for child in &s.body {
                assert!(matches!(child, LayoutChild::Pane(_)));
            }
        }
        other => panic!("expected Stack, got {other:?}"),
    }
}

#[test]
fn stack_rejects_row_child() {
    // T-012: row inside a stack body is a misplaced-node error.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@s" {
                row {
                    pane "@p"
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = validate_scope(&ir);
    assert!(
        errors.iter().any(
            |e| matches!(e, SceneError::MisplacedNode { node, parent, .. }
                if node == "row" && parent == "stack")
        ),
        "expected row-inside-stack misplaced error, got: {errors:?}"
    );
}

#[test]
fn stack_rejects_col_child() {
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@s" {
                col {
                    pane "@p"
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = validate_scope(&ir);
    assert!(
        errors.iter().any(
            |e| matches!(e, SceneError::MisplacedNode { node, parent, .. }
                if node == "col" && parent == "stack")
        ),
        "expected col-inside-stack misplaced error, got: {errors:?}"
    );
}

#[test]
fn stack_rejects_sizing_on_pane_child() {
    // R-9: pane-level sizing attrs inside a stack body are rejected.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@s" {
                pane "@p" span=2 {
                    shell
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = validate_scope(&ir);
    assert!(
        errors.iter().any(
            |e| matches!(e, SceneError::SizingOnStackChild { attr, child_handle, .. }
                if attr == "span" && child_handle == "@p")
        ),
        "expected sizing-on-stack-child for span, got: {errors:?}"
    );
}

#[test]
fn stack_rejects_sizing_on_nested_stack_child() {
    // R-9: nested stack containers inside another stack cannot carry
    // sizing attrs either.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@outer" {
                stack "@inner" cells=4 { }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = validate_scope(&ir);
    assert!(
        errors.iter().any(
            |e| matches!(e, SceneError::SizingOnStackChild { attr, child_handle, .. }
                if attr == "cells" && child_handle == "@inner")
        ),
        "expected sizing-on-stack-child for cells on nested stack, got: {errors:?}"
    );
}

#[test]
fn stack_allows_sizing_on_container() {
    // R-9 DOES allow sizing attrs on the stack container itself when it
    // appears as a child of a row / col.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            row {
                stack "@s" span=3 {
                    pane "@a"
                }
                pane "@b"
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = validate_scope(&ir);
    assert!(
        errors.is_empty(),
        "stack container-level sizing should be legal, got: {errors:?}"
    );
}

#[test]
fn stack_handle_participates_in_flat_namespace_tab_clash() {
    // T-011: tab vs stack handle clash surfaces as scene/handle-clash.
    let src = r#"
scene "s" {
    layout {
        tab "@dup" {
            stack "@dup" { }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = ark_scene::validate::validate_handles(&ir);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, SceneError::HandleClash { .. })),
        "expected handle-clash for tab/stack dup, got: {errors:?}"
    );
}

#[test]
fn stack_handle_participates_in_flat_namespace_pane_clash() {
    // T-011: pane vs stack handle clash surfaces as scene/handle-clash.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            row {
                pane "@twin"
                stack "@twin" { }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let errors = ark_scene::validate::validate_handles(&ir);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, SceneError::HandleClash { .. })),
        "expected handle-clash for pane/stack dup, got: {errors:?}"
    );
}

#[test]
fn stack_renders_as_stacked_pane_in_zellij_kdl() {
    // T-025: zellij-KDL emitter renders stack as `pane stacked=true { … }`.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@s" {
                pane "@a" {
                    shell
                }
                pane "@b" {
                    shell
                }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let registry = registry_with_shell();
    let kdl = compile_layout_kdl(first_layout(&ir), &registry).expect("layout should compile");
    let s = kdl.to_string();
    assert!(
        s.contains("stacked=#true") || s.contains("stacked=true"),
        "expected stacked=true in emitted KDL, got:\n{s}"
    );
    // Both child pane handles should still appear.
    assert!(s.contains("\"a\""), "expected child pane name `a` in:\n{s}");
    assert!(s.contains("\"b\""), "expected child pane name `b` in:\n{s}");
}

#[test]
fn empty_stack_compiles_to_zellij_kdl() {
    // T-008 + T-025: empty stack emits a zellij pane stack with no
    // children. Compiler must not reject it.
    let src = r#"
scene "s" {
    layout {
        tab "@main" {
            stack "@dyn"
        }
    }
}
"#;
    let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
    let registry = registry_with_shell();
    let kdl = compile_layout_kdl(first_layout(&ir), &registry)
        .expect("empty-stack layout should compile");
    let s = kdl.to_string();
    assert!(s.contains("stacked"), "expected stacked prop in:\n{s}");
}

#[test]
fn handle_valid_on_stack() {
    // T-011 supporting: `@` prefix validation applies to stack handles.
    let ok = Handle::new("@claude-1").is_ok() || Handle::new("@claude_1").is_ok();
    assert!(ok, "stack handle parsing should work for valid identifiers");
}
