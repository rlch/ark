//! T-041 R5 compile-fail (KDL-level): the scene declares a `pane`
//! bound to a view-type whose declared kind is `"stack"`. The handler
//! expects `Pane<EditorView>` semantics but the scene passes a stack
//! view-type. `validate_scene!` must emit a `.kdl:line:col` diagnostic
//! naming both the expected pane kind and the actually-declared stack
//! kind in plain English.

use ark_scene::validate_scene;

validate_scene! {
    manifests: [
        r#"extension {
            name "ext.a"
            views {
                view "EditorView"   { component "C"; kind "pane" }
                view "TerminalView" { component "C"; kind "stack" }
            }
        }"#,
    ],
    scene_path: "tests/ui/fixtures/view_type_mismatch.kdl",
    scene: r#"
scene "s" {
    layout {
        pane "ext.a.TerminalView" @h1
    }
}
"#,
}

fn main() {}
