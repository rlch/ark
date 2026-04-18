//! Smoke test: macro expands cleanly when scene references are valid.
//!
//! Compile-fail coverage lives under `crates/scene/tests/ui/` as
//! trybuild `.stderr` goldens — those are the T-041 R5 contract.

use ark_scene_macros::validate_scene;

#[test]
fn happy_path_compiles() {
    validate_scene! {
        manifests: [
            r#"extension {
                name "ext.a"
                views {
                    view "EditorView" {
                        component "EditorComponent"
                        kind "pane"
                    }
                }
            }"#,
        ],
        scene_path: "tests/ui/fixtures/happy.kdl",
        scene: r#"
            scene "s" {
                layout {
                    pane "ext.a.EditorView" @h1
                }
            }
        "#,
    }
}

#[test]
fn mixed_pane_and_stack_compiles() {
    validate_scene! {
        manifests: [
            r#"extension {
                name "ext.a"
                views {
                    view "EditorView" { component "C"; kind "pane" }
                    view "GridView"   { component "C"; kind "stack" }
                }
            }"#,
        ],
        scene_path: "tests/ui/fixtures/mixed.kdl",
        scene: r#"
            scene "s" {
                layout {
                    pane "ext.a.EditorView" @editor
                    stack "ext.a.GridView" @grid {
                        spawn_into @grid { }
                    }
                }
            }
        "#,
    }
}
