//! T-031 compile-fail (KDL-level): scene uses the subagent view
//! under a `pane` context, but the manifest declares
//! `claude-code.claude-code-subagent` with kind `stack`. The
//! `subagents` attribute on [`ClaudeCodeView`] expects a
//! `Stack<ClaudeCodeSubagent>` handle — binding the same view to a
//! pane handle crosses that boundary and MUST produce a
//! `.kdl:line:col` compile error naming the mismatch in plain
//! English (per R5 handle-type validation).

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
    scene_path: "tests/ui/fixtures/handle_kind_mismatch.kdl",
    scene: r#"
scene "s" {
    layout {
        pane "claude-code.claude-code-subagent" @subs
    }
}
"#,
}

fn main() {}
