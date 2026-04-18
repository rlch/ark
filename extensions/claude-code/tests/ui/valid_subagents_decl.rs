//! T-031 compile-pass (KDL-level): a scene that wires the
//! `claude-code` view's `subagents` attribute to a properly-declared
//! `Stack<ClaudeCodeSubagent>` handle. `validate_scene!` must accept
//! the scene without error.
//!
//! Manifest shape (two views):
//! * `claude-code.claude-code` — kind `pane`, the main Claude Code
//!   subprocess view.
//! * `claude-code.claude-code-subagent` — kind `stack`, the subagent
//!   fan-out view.
//!
//! Scene wires `@chat` as a pane bound to the main view AND `@subs`
//! as a stack bound to the subagent view. Both references validate
//! against their declared kinds.

use ark_scene::validate_scene;

fn main() {
    // `validate_scene!` expands to `()` on success. Wrap in a no-op
    // so the macro site is a valid expression position inside main.
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
        scene_path: "tests/ui/fixtures/valid_subagents_decl.kdl",
        scene: r#"
scene "s" {
    layout {
        pane "claude-code.claude-code" @chat
        stack "claude-code.claude-code-subagent" @subs
    }
}
"#,
    }
}
