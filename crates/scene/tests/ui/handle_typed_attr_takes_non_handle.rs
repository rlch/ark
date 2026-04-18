//! T-041 R5 compile-fail (KDL-level): the manifest-declared typed
//! `Pane<V>` attribute (`handle_attr`) receives a plain string literal
//! instead of an `@handle` reference. `validate_scene!` must emit a
//! `.kdl:line:col` diagnostic naming the `Pane<V>` handle-kind
//! expectation in plain English.

use ark_scene::validate_scene;

validate_scene! {
    manifests: [
        r#"extension {
            name "ext.a"
            views {
                view "EditorView" { component "C"; kind "pane" }
            }
        }"#,
    ],
    scene_path: "tests/ui/fixtures/handle_typed_attr_takes_non_handle.kdl",
    scene: r#"
scene "s" {
    layout {
        pane "ext.a.EditorView" @h1
        handle_attr @h1 value="some-literal-string"
    }
}
"#,
}

fn main() {}
