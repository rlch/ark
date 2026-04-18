//! T-041 compile-pass: well-formed pane + stack `ViewDecl`s round-
//! tripped through `ViewTypeTable::from_manifests` + exercised via
//! `validate_view_reference`. Locks the happy-path Rust surface of the
//! view-type validator.
//!
//! T-041 R5 extension: also invokes `validate_scene!` so the KDL-level
//! compile-time validator exercises the green path (mixed pane +
//! stack, matching manifests, spawn_into under a real stack). This
//! guards against a regression where the macro starts rejecting valid
//! scenes.

use ark_ext_metadata_types::{ExtensionMetadata, StringNode, ViewDecl};
use ark_scene::compile::view_types::{validate_view_reference, ViewTypeTable};
use ark_scene::validate_scene;

fn make_meta(name: &str, views: Vec<ViewDecl>) -> ExtensionMetadata {
    ExtensionMetadata {
        name: StringNode::new(name),
        version: StringNode::new("1.0.0"),
        ark_range: StringNode::new(">=0.1"),
        zellij_range: StringNode::new(""),
        requires: vec![],
        intents: vec![],
        events: vec![],
        config: Default::default(),
        views,
        capabilities: Default::default(),
        config_sections: vec![],
        reload_gates: vec![],
    }
}

fn main() {
    let views = vec![
        ViewDecl {
            name: "editor".into(),
            component: StringNode::new("EditorView"),
            kind: Some(StringNode::new("pane")),
        },
        ViewDecl {
            name: "split".into(),
            component: StringNode::new("SplitView"),
            kind: Some(StringNode::new("stack")),
        },
    ];
    let meta = make_meta("my-ext", views);
    let table = ViewTypeTable::from_manifests([("my-ext".to_string(), meta)]);
    assert!(validate_view_reference(&table, "my-ext.editor", "pane", None).is_ok());
    assert!(validate_view_reference(&table, "my-ext.split", "stack", None).is_ok());

    // KDL-level green path: pane + stack with `spawn_into` rooted on a
    // real stack parent. Macro expands to `()` on success.
    validate_scene! {
        manifests: [
            r#"extension {
                name "my-ext"
                views {
                    view "editor" { component "EditorView"; kind "pane" }
                    view "split"  { component "SplitView";  kind "stack" }
                }
            }"#,
        ],
        scene_path: "tests/ui/fixtures/valid_pane_and_stack_decls.kdl",
        scene: r#"
scene "s" {
    layout {
        pane "my-ext.editor" @editor
        stack "my-ext.split" @split {
            spawn_into @split { }
        }
    }
}
"#,
    }
}
