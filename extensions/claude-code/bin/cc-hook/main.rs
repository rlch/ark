//! `cc-hook` — Claude Code hook subprocess.
//!
//! Invoked by `~/.claude/settings.json` hook entries on each of the 9
//! Claude Code hook event kinds (`SessionStart`, `SessionEnd`,
//! `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`,
//! `SubagentStop`, `Stop`, `PreCompact`, `Notification`). POSTs a single
//! NDJSON line per invocation to the per-session ark socket at
//! `$STATE/sessions/<sid>/cc-hook.sock`, then exits. Write-only — no
//! reverse messages. See `cavekit-claude-code.md` R1 + R2.
//!
//! T-003 scaffolding: empty main. T-006 salvages the pre-deletion
//! `crates/hook/*` logic (cli / run / bridge / pipe / writer / allow)
//! into this binary and rewires it to NDJSON-over-unix-socket per R2.

fn main() {
    // TODO(T-006): salvage `crates/hook/src/{lib,main,cli,run,bridge,
    // pipe,writer,allow}.rs` into this binary; rewire to POST NDJSON to
    // `$STATE/sessions/<sid>/cc-hook.sock` per `cavekit-claude-code.md`
    // R2. Keep exit-0 on all failure paths (Claude Code hook spec
    // requires fast, silent exit).
}
