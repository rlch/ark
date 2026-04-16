//! Integration tests for `parse_scene` — T-011.
//!
//! These exercise the full facet-kdl pipeline from KDL source text through
//! to typed `SceneIR`, covering minimal scenes, layout structures,
//! reactions, keybinds, and error diagnostics.

use ark_scene::parse_scene;

#[test]
fn parses_minimal_scene() {
    let ir = parse_scene(r#"scene "x" { }"#, "test.kdl")
        .expect("minimal scene should parse");
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
