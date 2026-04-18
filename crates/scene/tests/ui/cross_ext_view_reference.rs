//! T-041 compile-pass: two extensions, each declaring one view,
//! referenced through fully-qualified `<ext>.<view>` tokens. Exercises
//! namespaced lookup + the kind-mismatch / unknown-token error branches
//! on `validate_view_reference`.
//!
//! T-041 R5 extension: also invokes `validate_scene!` on a scene whose
//! pane references `ext-a.panel` and whose stack references
//! `ext-b.grid` — proving the KDL-level validator resolves view types
//! across multiple installed manifests and accepts a cross-extension
//! reference on the green path.

use ark_ext_metadata_types::{ExtensionMetadata, StringNode, ViewDecl};
use ark_scene::compile::view_types::{validate_view_reference, ViewTypeTable};
use ark_scene::validate_scene;

fn make(name: &str, view_name: &str, kind: &str) -> (String, ExtensionMetadata) {
    let meta = ExtensionMetadata {
        name: StringNode::new(name),
        version: StringNode::new("1.0.0"),
        ark_range: StringNode::new(">=0.1"),
        zellij_range: StringNode::new(""),
        requires: vec![],
        intents: vec![],
        events: vec![],
        config: Default::default(),
        views: vec![ViewDecl {
            name: view_name.into(),
            component: StringNode::new("C"),
            kind: Some(StringNode::new(kind)),
        }],
        capabilities: Default::default(),
        config_sections: vec![],
        reload_gates: vec![],
    };
    (name.to_string(), meta)
}

fn main() {
    let manifests = vec![make("ext-a", "panel", "pane"), make("ext-b", "grid", "stack")];
    let table = ViewTypeTable::from_manifests(manifests);
    assert!(validate_view_reference(&table, "ext-a.panel", "pane", None).is_ok());
    assert!(validate_view_reference(&table, "ext-b.grid", "stack", None).is_ok());
    // Kind mismatch: declared=pane but used as stack.
    assert!(validate_view_reference(&table, "ext-a.panel", "stack", None).is_err());
    // Unknown token: no ext-c installed.
    assert!(validate_view_reference(&table, "ext-c.missing", "pane", None).is_err());

    // KDL-level green path: scene in "ext a" references view from
    // "ext b". Macro expands to `()` when both manifests resolve.
    validate_scene! {
        manifests: [
            r#"extension {
                name "ext-a"
                views {
                    view "panel" { component "C"; kind "pane" }
                }
            }"#,
            r#"extension {
                name "ext-b"
                views {
                    view "grid" { component "C"; kind "stack" }
                }
            }"#,
        ],
        scene_path: "tests/ui/fixtures/cross_ext_view_reference.kdl",
        scene: r#"
scene "s" {
    layout {
        pane "ext-a.panel" @panel
        stack "ext-b.grid" @grid {
            spawn_into @grid { }
        }
    }
}
"#,
    }
}
