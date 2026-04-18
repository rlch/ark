//! T-042 R6 compile-fail (KDL-level): scene references
//! `intent "ext.a.undeclared"` but no manifest declares it. The
//! `validate_scene!` proc-macro must emit a `compile_error!` with a
//! `.kdl:line:col` pointer naming the offending intent in plain
//! English — per decision #2, the manifest is the SOLE source of
//! truth for intent registration in v0.1.

use ark_scene::validate_scene;

validate_scene! {
    manifests: [
        r#"extension {
            name "ext.a"
            intents {
                intent "declared"
            }
        }"#,
    ],
    scene_path: "tests/ui/fixtures/undeclared_intent_reference.kdl",
    scene: r#"
scene "s" {
    on "FileEdited" {
        intent "ext.a.undeclared" "payload"
    }
}
"#,
}

fn main() {}
