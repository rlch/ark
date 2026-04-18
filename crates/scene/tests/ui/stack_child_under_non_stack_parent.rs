//! T-041 R5 compile-fail (KDL-level): KDL nests `spawn_into @parent`
//! under a parent handle that resolves to a `Pane<V>`, not a
//! `Stack<V>`. `validate_scene!` must emit a `.kdl:line:col`
//! diagnostic that names the parent handle kind in plain English.

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
    scene_path: "tests/ui/fixtures/stack_child_under_non_stack_parent.kdl",
    scene: r#"
scene "s" {
    layout {
        pane "ext.a.EditorView" @parent
        spawn_into @parent { }
    }
}
"#,
}

fn main() {}
