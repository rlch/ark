//! T-047 (claude-code-ext R13 unit-level integration tests).
//!
//! Offline-compiled scene path: instantiate [`ClaudeCodeView`] +
//! [`ClaudeCodeSubagentView`] directly with a mock
//! `Stack<ClaudeCodeSubagent>` handle, then exercise the view-logic
//! surface (fan-out, title rendering, state-cache dispatch) WITHOUT a
//! real zellij or a real scene compile.
//!
//! This covers R13 integration-strategy 1 ("unit-level: scene-compiled-
//! offline tests instantiate `ClaudeCodeView` + `ClaudeCodeSubagentView`
//! with mock `Stack<ClaudeCodeSubagent>` handles per scene kit R17 test
//! stubs"). The PTY-level gate (strategy 2) lives under
//! `tests/claude_code_smoke.rs` per T-048.

use ark_ext_claude_code::{
    ClaudeCodeSpawnSet, ClaudeCodeSubagent, ClaudeCodeSubagentAttrs, ClaudeCodeSubagentView,
    ClaudeCodeView, EXT_NAME, SubagentRegistry, SubagentState, SubagentStatus,
    format_subagent_title,
};
use ark_types::ExtEvent;
use ark_view::Stack;

/// Mock stack construction — the public path from outside ark-view is
/// serde round-trip. Mirrors the in-crate test helper at
/// `src/lib.rs::subagent_dispatch_tests::stub_stack`.
fn stub_stack(handle_bytes: &str) -> Stack<ClaudeCodeSubagent> {
    let json = format!("\"{handle_bytes}\"");
    serde_json::from_str::<Stack<ClaudeCodeSubagent>>(&json).expect("stub stack deserialisation")
}

fn subagent_start(agent_id: &str, agent_type: &str, transcript_path: &str) -> ExtEvent {
    ExtEvent {
        ext: EXT_NAME.to_string(),
        kind: "subagent.start".to_string(),
        payload: serde_json::json!({
            "agent_id": agent_id,
            "agent_type": agent_type,
            "agent_transcript_path": transcript_path,
        }),
    }
}

fn pre_tool_use(agent_id: &str, tool_name: &str) -> ExtEvent {
    ExtEvent {
        ext: EXT_NAME.to_string(),
        kind: "pre-tool-use".to_string(),
        payload: serde_json::json!({
            "agent_id": agent_id,
            "tool_name": tool_name,
        }),
    }
}

fn subagent_stop(agent_id: &str, success: bool) -> ExtEvent {
    ExtEvent {
        ext: EXT_NAME.to_string(),
        kind: "subagent.stop".to_string(),
        payload: serde_json::json!({
            "agent_id": agent_id,
            "success": success,
        }),
    }
}

// --------------------------------------------------------------------------
// Scene kit R17 test-stub instantiation — view construction without scene
// --------------------------------------------------------------------------

#[test]
fn claude_code_view_constructed_with_mock_subagents_stack() {
    // Directly instantiate the typed `ClaudeCodeView` as the scene
    // compiler would do — but without compiling a scene. The view
    // carries a mock stack handle representing the `@subs` ref.
    let stack = stub_stack("mock-stack-handle");
    let view = ClaudeCodeView {
        model: Some("claude-sonnet-4-6".to_string()),
        args: vec!["--verbose".to_string()],
        cwd: Some("/workspace".to_string()),
        subagents: Some(stack),
    };
    // Argv + env shape round-trip through the spawner contract.
    assert_eq!(
        view.build_argv(),
        vec![
            "claude".to_string(),
            "--model".to_string(),
            "claude-sonnet-4-6".to_string(),
            "--verbose".to_string(),
        ]
    );
    assert_eq!(
        view.resolve_cwd(std::path::Path::new("/home")),
        std::path::PathBuf::from("/workspace")
    );
    // Subagents handle is present and serde-stable.
    let s = view.subagents.as_ref().unwrap();
    let handle_json = serde_json::to_string(s).unwrap();
    assert_eq!(handle_json, "\"mock-stack-handle\"");
}

#[test]
fn claude_code_subagent_view_from_spawn_attrs() {
    // T-038 attrs → view round-trip. The spawner consumes the attrs and
    // stamps a `ClaudeCodeSubagentView` with matching fields — the view
    // itself is a pure data carrier.
    let attrs = ClaudeCodeSubagentAttrs {
        id: "agent-7".to_string(),
        transcript_path: "/tmp/sess/subagents/agent-7.jsonl".to_string(),
    };
    let sv = ClaudeCodeSubagentView {
        id: attrs.id.clone(),
        transcript_path: attrs.transcript_path.clone(),
    };
    assert_eq!(sv.id, "agent-7");
    assert_eq!(sv.transcript_path, "/tmp/sess/subagents/agent-7.jsonl");
}

// --------------------------------------------------------------------------
// T-038 fan-out: synthetic subagent.start → attrs emission
// --------------------------------------------------------------------------

#[test]
fn fan_out_on_subagent_start_emits_attrs_with_transcript_path() {
    let view = ClaudeCodeView {
        subagents: Some(stub_stack("subs-1")),
        ..Default::default()
    };
    let spawn_set = ClaudeCodeSpawnSet::new();
    let ev = subagent_start("agent-alpha", "code-writer", "/tmp/a/alpha.jsonl");
    let fanout = view.on_ext_event(&ev, &spawn_set).expect("fan-out");
    assert_eq!(fanout.attrs.id, "agent-alpha");
    assert_eq!(fanout.attrs.transcript_path, "/tmp/a/alpha.jsonl");
    // Spawn set records the claim.
    assert!(spawn_set.contains("agent-alpha"));
    assert_eq!(spawn_set.len(), 1);
}

#[test]
fn fan_out_is_idempotent_on_duplicate_subagent_start() {
    // T-038: duplicate `subagent.start` for the same agent_id must NOT
    // fan out twice. Second call returns None and the spawn set stays
    // size 1.
    let view = ClaudeCodeView {
        subagents: Some(stub_stack("subs-2")),
        ..Default::default()
    };
    let spawn_set = ClaudeCodeSpawnSet::new();
    let ev = subagent_start("dup-agent", "writer", "/tmp/d/dup.jsonl");
    assert!(view.on_ext_event(&ev, &spawn_set).is_some());
    assert!(view.on_ext_event(&ev, &spawn_set).is_none());
    assert_eq!(spawn_set.len(), 1);
}

#[test]
fn fan_out_when_subagents_is_none_noops_per_t040() {
    // T-040: view with no subagents handle → no fan-out. Events still
    // flow through the registry to user reactions (see the "decoupled"
    // test below).
    let view = ClaudeCodeView::default();
    assert!(view.subagents.is_none());
    let spawn_set = ClaudeCodeSpawnSet::new();
    let ev = subagent_start("orphan", "writer", "/tmp/o.jsonl");
    assert!(view.on_ext_event(&ev, &spawn_set).is_none());
    assert_eq!(spawn_set.len(), 0);
}

#[test]
fn fan_out_ignores_non_subagent_events() {
    let view = ClaudeCodeView {
        subagents: Some(stub_stack("subs-3")),
        ..Default::default()
    };
    let spawn_set = ClaudeCodeSpawnSet::new();
    let ev = pre_tool_use("agent-x", "Edit");
    assert!(view.on_ext_event(&ev, &spawn_set).is_none());
    // Stop does NOT fan out either (T-039).
    let stop = subagent_stop("agent-x", true);
    assert!(view.on_ext_event(&stop, &spawn_set).is_none());
    assert_eq!(spawn_set.len(), 0);
}

// --------------------------------------------------------------------------
// T-037 registry dispatch + T-035 RenamePane payload shape
// --------------------------------------------------------------------------

#[test]
fn registry_subagent_start_updates_state_cache_and_renders_title() {
    let reg = SubagentRegistry::new();
    let start = subagent_start("agent-1", "code-writer", "/tmp/a1.jsonl");
    let emission = reg.on_ext_event(&start).expect("emission");
    assert_eq!(emission.id, "agent-1");
    assert_eq!(emission.payload.get("kind").unwrap(), "RenamePane");
    assert_eq!(
        emission.payload.get("name").unwrap(),
        "code-writer · running · -"
    );

    let cached = reg.get("agent-1").unwrap();
    assert_eq!(cached.agent_type, "code-writer");
    assert_eq!(cached.status, SubagentStatus::Running);
    assert!(cached.last_tool.is_none());
}

#[test]
fn registry_pre_tool_use_then_stop_round_trip() {
    let reg = SubagentRegistry::new();
    // start
    reg.on_ext_event(&subagent_start("a", "w", "/t.jsonl"))
        .unwrap();
    // pre-tool-use → last_tool updated, title reflects it
    let pre = reg.on_ext_event(&pre_tool_use("a", "Edit")).unwrap();
    assert_eq!(pre.payload.get("name").unwrap(), "w · running · Edit");
    // stop success=true → Done
    let stop = reg.on_ext_event(&subagent_stop("a", true)).unwrap();
    assert_eq!(stop.payload.get("name").unwrap(), "w · done · Edit");
    // stop success=false path via a fresh agent
    reg.on_ext_event(&subagent_start("b", "r", "/b.jsonl"))
        .unwrap();
    let bad = reg.on_ext_event(&subagent_stop("b", false)).unwrap();
    assert_eq!(bad.payload.get("name").unwrap(), "r · failed · -");
}

#[test]
fn registry_ignores_events_from_other_extensions() {
    let reg = SubagentRegistry::new();
    let foreign = ExtEvent {
        ext: "other-ext".to_string(),
        kind: "subagent.start".to_string(),
        payload: serde_json::json!({
            "agent_id": "a",
            "agent_type": "t",
        }),
    };
    assert!(reg.on_ext_event(&foreign).is_none());
    assert!(reg.get("a").is_none());
}

// --------------------------------------------------------------------------
// R7 / T-039 guarantee: subagent.stop does NOT remove the stack child
// --------------------------------------------------------------------------

#[test]
fn subagent_stop_never_closes_the_stack_child() {
    // T-039 + R7: the view intentionally does not call
    // `stack.close_child(...)` on stop — the tile stays live for the
    // user to inspect. We model "stack child membership" via the spawn
    // set; after start+stop the set MUST still contain the id.
    let view = ClaudeCodeView {
        subagents: Some(stub_stack("subs-4")),
        ..Default::default()
    };
    let spawn_set = ClaudeCodeSpawnSet::new();
    view.on_ext_event(&subagent_start("keep", "t", "/k.jsonl"), &spawn_set)
        .expect("fan-out");
    assert_eq!(spawn_set.len(), 1);
    // Stop returns None (no fan-out on stop) AND the spawn set is
    // unchanged — the child tile stays live.
    assert!(
        view.on_ext_event(&subagent_stop("keep", true), &spawn_set)
            .is_none()
    );
    assert!(spawn_set.contains("keep"));
    assert_eq!(spawn_set.len(), 1);
}

// --------------------------------------------------------------------------
// Cross-surface: view fan-out + registry state update stay decoupled
// --------------------------------------------------------------------------

#[test]
fn fan_out_off_but_registry_still_records_state_per_t040() {
    // T-040 acceptance: even when the view fan-out is off (no
    // subagents handle), the registry still folds state so user
    // Rhai reactions see title updates via the broader bus.
    let view_off = ClaudeCodeView::default();
    let reg = SubagentRegistry::new();
    let spawn_set = ClaudeCodeSpawnSet::new();
    let ev = subagent_start("z", "t", "/z.jsonl");
    assert!(view_off.on_ext_event(&ev, &spawn_set).is_none());
    let emission = reg.on_ext_event(&ev).expect("registry emits");
    assert_eq!(emission.id, "z");
    // State cache has Running; title format is canonical.
    let cached = reg.get("z").unwrap();
    assert_eq!(cached.status, SubagentStatus::Running);
    assert_eq!(
        emission.payload.get("name").unwrap(),
        &serde_json::Value::String(format_subagent_title("t", SubagentStatus::Running, None))
    );
}

// --------------------------------------------------------------------------
// Pure title rendering — no IO, no handle, just SubagentState
// --------------------------------------------------------------------------

#[test]
fn title_rendering_across_status_transitions_matches_r6_format() {
    let mut s = SubagentState::new("agent-0", "writer");
    assert_eq!(s.render_title(), "writer · running · -");
    s.last_tool = Some("Edit".to_string());
    assert_eq!(s.render_title(), "writer · running · Edit");
    s.status = SubagentStatus::Done;
    assert_eq!(s.render_title(), "writer · done · Edit");
    s.status = SubagentStatus::Failed;
    s.last_tool = Some("Bash".to_string());
    assert_eq!(s.render_title(), "writer · failed · Bash");
}

// --------------------------------------------------------------------------
// Transcript rendering — pure function over formatted JSONL
// --------------------------------------------------------------------------

#[test]
fn transcript_tail_renders_assistant_and_tool_use_lines() {
    // R6 expanded rendering: minimal plain-text formatter. Exercises
    // `ClaudeCodeSubagentView::render_transcript_tail` via a real file
    // so the TailCursor path is covered too.
    use std::io::Write;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        writeln!(
            f,
            r#"{{"type":"message","role":"assistant","content":[{{"type":"text","text":"hi"}}]}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"tool_use","name":"Bash","input":{{"command":"ls"}}}}"#
        )
        .unwrap();
    }
    let mut cursor = ark_ext_claude_code::TailCursor::new(tmp.path());
    let out = ClaudeCodeSubagentView::render_transcript_tail(&mut cursor, 50).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], "assistant: hi");
    assert!(out[1].starts_with("tool_use: Bash("));
}
