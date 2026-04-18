//! Integration tests for `parse_scene` — T-011 / T-012 / T-016.
//!
//! These exercise the full facet-kdl pipeline from KDL source text through
//! to typed `SceneIR`, covering minimal scenes, layout structures,
//! reactions, keybinds, and error diagnostics.
//!
//! T-012 tests verify single-top-level-scene enforcement (zero or multiple
//! `scene` nodes rejected). T-016 tests verify `on`/`bind` ordering
//! preservation.

use ark_scene::parse_scene;

#[test]
fn parses_minimal_scene() {
    let ir = parse_scene(r#"scene "x" { }"#, "test.kdl").expect("minimal scene should parse");
    assert_eq!(ir.scene.name, "x");
    assert!(ir.scene.body.is_empty());
    assert!(ir.scene.max_cascade_depth.is_none());
}

#[test]
fn parses_scene_with_layout() {
    let src = r#"
scene "dev" {
    layout {
        tab "@main" focus="true" {
            row {
                pane "@editor" span=2
                pane "@term"
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "layout.kdl").expect("layout scene should parse");
    assert_eq!(ir.scene.name, "dev");
    // Should have exactly one body node (layout).
    assert_eq!(ir.scene.body.len(), 1);
}

#[test]
fn parses_scene_with_reaction() {
    let src = r#"
scene "reactive" {
    on "FileEdited" when="true" {
        close "@x"
    }
}
"#;
    let ir = parse_scene(src, "react.kdl").expect("reaction scene should parse");
    assert_eq!(ir.scene.name, "reactive");
    assert_eq!(ir.scene.body.len(), 1);
}

#[test]
fn parses_scene_with_bind() {
    let src = r#"
scene "keys" {
    bind "Alt q" {
        close "@x"
    }
}
"#;
    let ir = parse_scene(src, "keys.kdl").expect("bind scene should parse");
    assert_eq!(ir.scene.name, "keys");
    assert_eq!(ir.scene.body.len(), 1);
}

#[test]
fn parse_error_surfaces_diagnostic() {
    let result = parse_scene("this is not valid kdl {{{", "bad.kdl");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        ark_scene::SceneError::Parse { message, .. } => {
            assert!(!message.is_empty(), "parse error should have a message");
        }
        other => panic!("expected SceneError::Parse, got: {other:?}"),
    }
}

#[test]
fn scene_id_content_hash_is_deterministic() {
    let ir1 = parse_scene(r#"scene "a" { }"#, "a.kdl").unwrap();
    let ir2 = parse_scene(r#"scene "a" { }"#, "a.kdl").unwrap();
    assert_eq!(ir1.id, ir2.id);
}

// ---------------------------------------------------------------------------
// T-012: Single-top-level-scene enforcement
// ---------------------------------------------------------------------------

/// An empty KDL document (no `scene` node at all) must be rejected.
#[test]
fn rejects_zero_scene_nodes() {
    let result = parse_scene("", "empty.kdl");
    assert!(result.is_err(), "empty document should be rejected");
    match result.unwrap_err() {
        ark_scene::SceneError::Parse { .. } => {} // expected
        other => panic!("expected SceneError::Parse, got: {other:?}"),
    }
}

/// Two `scene` nodes in the same document must be rejected — only one
/// top-level `scene` is legal per R1.1.
#[test]
fn rejects_multiple_scene_nodes() {
    let result = parse_scene(r#"scene "a" { } scene "b" { }"#, "multi.kdl");
    assert!(result.is_err(), "multiple scene nodes should be rejected");
    match result.unwrap_err() {
        ark_scene::SceneError::Parse { .. } => {} // expected
        other => panic!("expected SceneError::Parse, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// T-016: Node-ordering semantics — on/bind execute in textual order
// ---------------------------------------------------------------------------

/// Verify that `on` and `bind` nodes in the scene body preserve their
/// textual source order. A scene with `bind / on / bind` must yield
/// body\[0\] = Bind, body\[1\] = On, body\[2\] = Bind.
#[test]
fn on_bind_ordering_preserved() {
    let src = r#"
scene "ordered" {
    bind "Alt a" {
        close "@x"
    }
    on "FileEdited" {
        close "@y"
    }
    bind "Alt b" {
        close "@z"
    }
}
"#;
    let ir = parse_scene(src, "order.kdl").expect("ordering scene should parse");
    assert_eq!(ir.scene.body.len(), 3, "expected 3 body nodes");

    assert!(
        matches!(&ir.scene.body[0], ark_scene::ast::SceneBodyNode::Bind(_)),
        "body[0] should be Bind, got: {:?}",
        ir.scene.body[0]
    );
    assert!(
        matches!(&ir.scene.body[1], ark_scene::ast::SceneBodyNode::On(_)),
        "body[1] should be On, got: {:?}",
        ir.scene.body[1]
    );
    assert!(
        matches!(&ir.scene.body[2], ark_scene::ast::SceneBodyNode::Bind(_)),
        "body[2] should be Bind, got: {:?}",
        ir.scene.body[2]
    );
}
