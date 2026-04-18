//! T-041 R5 compile-fail (KDL-level): scene declares `pane "ext.a.MyView"`
//! but no installed manifest declares `MyView`. The `validate_scene!`
//! proc-macro must emit `compile_error!` with a `.kdl:line:col`
//! pointer naming the offending view-type token in plain English.

use ark_scene::validate_scene;

validate_scene! {
    manifests: [
        r#"extension {
            name "ext.a"
            views {
                view "KnownView" { component "C"; kind "pane" }
            }
        }"#,
    ],
    scene_path: "tests/ui/fixtures/undeclared_view_type.kdl",
    scene: r#"
scene "s" {
    layout {
        pane "ext.a.MyView" @h1
    }
}
"#,
}

fn main() {}
