//! T-031 compile-fail (KDL-level): scene references a subagent view
//! token no manifest declares (typo-scenario — `claude-subagents`
//! instead of `claude-code-subagent`). `validate_scene!` must surface
//! this as `.kdl:line:col: unknown view type …` so scene authors catch
//! the typo at compile time rather than session launch.

use ark_scene::validate_scene;

validate_scene! {
    manifests: [
        r#"extension {
            name "claude-code"
            views {
                view "claude-code" { component "ClaudeCodeView"; kind "pane" }
                view "claude-code-subagent" { component "ClaudeCodeSubagentView"; kind "stack" }
            }
        }"#,
    ],
    scene_path: "tests/ui/fixtures/unknown_subagents_view.kdl",
    scene: r#"
scene "s" {
    layout {
        stack "claude-code.claude-subagents" @subs
    }
}
"#,
}

fn main() {}
