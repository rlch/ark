//! T-015: verbatim-payload preservation for the R3 translator.
//!
//! For each of the payload field keys Claude Code writes into hook JSON
//! (those the kit R3 envelope test calls out plus a handful of "unknown
//! field" passthrough probes), constructing a [`HookPayload`] with that
//! field set MUST round-trip through [`payload_to_ext_event`] with the
//! byte-identical JSON value living under `ExtEvent.payload.<field>`.
//!
//! The translator is pure — no per-kind restructuring, no truncation,
//! no synthetic side-events — so every test here is essentially a
//! structural identity check. If a future translator change decides to
//! lift a specific field out of the payload onto ExtEvent (e.g. for
//! Rhai ergonomics), one of these tests MUST be updated in lockstep
//! with the kit R3 table so the contract can't silently drift.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_ext_claude_code::{HookEvent, HookPayload, payload_to_ext_event};
use serde_json::{Value, json};

/// Build a minimal [`HookPayload`] carrying one top-level `extra` key +
/// value, with the other fields at neutral defaults. Factored out so
/// every test reads as "given this extra field, translator preserves
/// it" without the boilerplate.
fn payload_with_extra(key: &str, value: Value) -> HookPayload {
    let mut extra = BTreeMap::new();
    extra.insert(key.to_string(), value);
    HookPayload {
        session_id: "sess-t015".into(),
        cwd: PathBuf::from("/tmp"),
        hook_event_name: "SubagentStop".into(),
        tool_name: None,
        tool_input: None,
        extra,
    }
}

/// Translate + fetch the field's JSON back out. Centralised so every
/// test asserts on the value the translator produced rather than
/// shadowing it in local state.
fn translate_and_fetch(p: &HookPayload, key: &str, event: HookEvent) -> Value {
    let ev = payload_to_ext_event(p, event);
    assert_eq!(ev.ext, "claude-code");
    ev.payload
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("field `{key}` missing from payload: {ev:?}"))
}

// ---------- SubagentStop payload fields (R3 envelope test) ----------

#[test]
fn agent_id_preserved_verbatim() {
    let p = payload_with_extra("agent_id", json!("agent-deadbeef-42"));
    let v = translate_and_fetch(&p, "agent_id", HookEvent::SubagentStop);
    assert_eq!(v, json!("agent-deadbeef-42"));
}

#[test]
fn agent_type_preserved_verbatim() {
    let p = payload_with_extra("agent_type", json!("code-writer"));
    let v = translate_and_fetch(&p, "agent_type", HookEvent::SubagentStop);
    assert_eq!(v, json!("code-writer"));
}

#[test]
fn agent_transcript_path_preserved_verbatim() {
    let p = payload_with_extra(
        "agent_transcript_path",
        json!("/home/user/.claude/projects/abc/agent-42.jsonl"),
    );
    let v = translate_and_fetch(&p, "agent_transcript_path", HookEvent::SubagentStop);
    assert_eq!(v, json!("/home/user/.claude/projects/abc/agent-42.jsonl"));
}

#[test]
fn last_assistant_message_preserved_verbatim() {
    // Include newline + punctuation to rule out any accidental text
    // transformation (trim, escape, Unicode normalisation).
    let body = "Done — wrote 3 files.\nSee diff.";
    let p = payload_with_extra("last_assistant_message", json!(body));
    let v = translate_and_fetch(&p, "last_assistant_message", HookEvent::SubagentStop);
    assert_eq!(v, json!(body));
}

// ---------- tool_name / tool_input / tool_response ----------

#[test]
fn tool_name_preserved_verbatim_typed_field() {
    // `tool_name` is a first-class `HookPayload` field (not `extra`),
    // so this test pins that the translator still surfaces it at the
    // same key in the output payload.
    let p = HookPayload {
        session_id: "s".into(),
        cwd: PathBuf::from("/tmp"),
        hook_event_name: "PreToolUse".into(),
        tool_name: Some("Edit".into()),
        tool_input: None,
        extra: BTreeMap::new(),
    };
    let v = translate_and_fetch(&p, "tool_name", HookEvent::PreToolUse);
    assert_eq!(v, json!("Edit"));
}

#[test]
fn tool_input_preserved_verbatim_nested_json() {
    // `tool_input` is `Option<serde_json::Value>` — nested object with
    // mixed types must pass through byte-identically.
    let input = json!({
        "file_path": "/repo/src/lib.rs",
        "old_string": "foo",
        "new_string": "bar\n\tbaz",
        "replace_all": false,
        "nested": { "deep": [1, 2, { "k": null }] },
    });
    let p = HookPayload {
        session_id: "s".into(),
        cwd: PathBuf::from("/tmp"),
        hook_event_name: "PreToolUse".into(),
        tool_name: Some("Edit".into()),
        tool_input: Some(input.clone()),
        extra: BTreeMap::new(),
    };
    let v = translate_and_fetch(&p, "tool_input", HookEvent::PreToolUse);
    assert_eq!(v, input);
}

#[test]
fn tool_response_preserved_verbatim() {
    // `tool_response` is not a typed field on HookPayload (R2 kept the
    // typed shape minimal); it rides through `extra`. Important: some
    // real hook payloads wrap the response under nested `content`
    // arrays that the translator MUST NOT flatten.
    let resp = json!({
        "content": [
            {"type": "text", "text": "file written"},
            {"type": "image", "url": "data:image/png;base64,AAAA"},
        ],
        "is_error": false,
    });
    let p = payload_with_extra("tool_response", resp.clone());
    let v = translate_and_fetch(&p, "tool_response", HookEvent::PostToolUse);
    assert_eq!(v, resp);
}

// ---------- Unknown/future field passthrough ----------

#[test]
fn unknown_future_scalar_field_preserved() {
    // Drive home that `extra` is forward-compat: any unknown key
    // reaches the ExtEvent payload unchanged, so future Claude-side
    // additions reach reactions without a crate rebuild.
    let p = payload_with_extra("future_scalar_v2", json!("opaque"));
    let v = translate_and_fetch(&p, "future_scalar_v2", HookEvent::Notification);
    assert_eq!(v, json!("opaque"));
}

#[test]
fn unknown_future_nested_object_preserved() {
    let obj = json!({
        "v": 7,
        "labels": ["a", "b"],
        "flag": true,
        "inner": {"x": 1.25, "y": null},
    });
    let p = payload_with_extra("future_object_v2", obj.clone());
    let v = translate_and_fetch(&p, "future_object_v2", HookEvent::Notification);
    assert_eq!(v, obj);
}

// ---------- Structural checks ----------

#[test]
fn translator_produces_claude_code_ext_name() {
    // Pin the `ext` field so a refactor that renames the extension
    // surfaces here before it breaks every scene reaction in the wild.
    let p = payload_with_extra("agent_id", json!("x"));
    let ev = payload_to_ext_event(&p, HookEvent::SubagentStop);
    assert_eq!(ev.ext, "claude-code");
    assert_eq!(ev.kind, "subagent.stop");
}

#[test]
fn all_hook_kinds_map_via_translator() {
    // T-014: the translator handles every variant of HookEvent. Pins
    // the R3 mapping table against drift by walking HookEvent::ALL.
    let cases: &[(HookEvent, &str)] = &[
        (HookEvent::SessionStart, "session.start"),
        (HookEvent::SessionEnd, "session.end"),
        (HookEvent::UserPromptSubmit, "user.prompt-submit"),
        (HookEvent::PreToolUse, "pre-tool-use"),
        (HookEvent::PostToolUse, "post-tool-use"),
        (HookEvent::SubagentStart, "subagent.start"),
        (HookEvent::SubagentStop, "subagent.stop"),
        (HookEvent::Stop, "stop"),
        (HookEvent::PreCompact, "pre-compact"),
        (HookEvent::Notification, "notification"),
    ];
    for (ev, want_kind) in cases {
        let p = HookPayload {
            session_id: "s".into(),
            cwd: PathBuf::from("/tmp"),
            hook_event_name: ev.as_str().into(),
            tool_name: None,
            tool_input: None,
            extra: BTreeMap::new(),
        };
        let out = payload_to_ext_event(&p, *ev);
        assert_eq!(out.ext, "claude-code");
        assert_eq!(
            out.kind,
            *want_kind,
            "kind drift for {}: got `{}`, want `{}`",
            ev.as_str(),
            out.kind,
            want_kind
        );
    }
}
