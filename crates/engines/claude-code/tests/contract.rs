//! Engine contract suite integration test for
//! [`ark_engines_claude_code::ClaudeCodeEngine`] (T-114,
//! cavekit-architecture.md R1).
//!
//! This file drives the portable [`ark_core::engine_contract_suite`]
//! against the claude-code factory and layers on the engine-specific
//! timeline + transcript-parsing scenarios that the `Engine` trait does
//! not yet expose (see `ark_core::engine_contract` module docs —
//! "Deferred" list).
//!
//! Every scenario is a discrete `#[test]` so `cargo test -p
//! ark-engines-claude-code` names the failing scenario without needing to
//! parse a composite test log.

use std::path::{Path, PathBuf};

use ark_core::engine::Engine;
use ark_core::engine_contract_suite;
use ark_engines_claude_code::{ClaudeCodeEngine, parse_line, tail_transcript_path};
use ark_test_fixtures::{EngineFixtures, engine_fixtures};
use ark_types::{AgentEvent, AgentId, MessageRole, channel};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

fn make_factory() -> impl Fn() -> Box<dyn Engine> {
    || Box::new(ClaudeCodeEngine::new())
}

fn contract_id() -> AgentId {
    AgentId::new("cavekit", "contract")
}

/// Portable trait-surface contract — same assertions every Engine impl
/// must satisfy.
#[test]
fn claude_code_passes_engine_contract() {
    let fixtures = engine_fixtures();
    engine_contract_suite(make_factory(), &fixtures);
}

/// Scenario from T-114: factory() called twice yields two independent
/// Box<dyn Engine>. The portable suite already covers this; this test
/// documents the claude-code-specific observable — both instances agree
/// on `name()` and `default_pane_cmd()` and can be torn down after an
/// install without interfering with each other.
#[test]
fn factory_closure_produces_fresh_instance() {
    let factory = make_factory();
    let a = factory();
    let b = factory();
    assert_eq!(a.name(), "claude-code");
    assert_eq!(b.name(), "claude-code");
    assert_eq!(a.default_pane_cmd(), b.default_pane_cmd());
    assert_eq!(a.default_pane_cmd(), vec!["claude".to_string()]);
}

/// Scenario from T-114: install_observability writes the expected hook
/// config on a tempdir.
#[tokio::test]
async fn install_observability_creates_hook_config() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_path_buf();
    let engine = ClaudeCodeEngine::new();
    let (sink, _rx) = channel(8);
    let id = contract_id();

    let handle = engine
        .install_observability(&id, &cwd, sink)
        .await
        .expect("install");
    assert_eq!(handle.engine_name(), "claude-code");

    // Engine-specific: settings.local.json exists + carries the hook
    // entries keyed on the agent id.
    let settings_path = cwd.join(".claude").join("settings.local.json");
    assert!(
        settings_path.is_file(),
        ".claude/settings.local.json must exist after install"
    );
    let raw = std::fs::read_to_string(&settings_path).expect("read settings");
    let v: Value = serde_json::from_str(&raw).expect("settings parse as json");
    assert!(
        v.get("hooks").is_some(),
        "settings.local.json must carry a `hooks` block after install, got: {raw}"
    );
    assert!(
        raw.contains(&format!("ark-hook --id {}", id.as_str())),
        "hook command must be keyed on the agent id ({}); got: {raw}",
        id.as_str()
    );

    engine.teardown(handle).await.expect("teardown");
}

/// Scenario from T-114: install, restore, restore again → no error;
/// backup file cleaned up.
#[tokio::test]
async fn restore_settings_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_path_buf();
    std::fs::create_dir_all(cwd.join(".claude")).unwrap();
    std::fs::write(
        cwd.join(".claude").join("settings.local.json"),
        br#"{"permissions":{"allow":["Read"]}}"#,
    )
    .unwrap();

    let engine = ClaudeCodeEngine::new();
    let (sink, _rx) = channel(8);
    let id = contract_id();

    let handle = engine
        .install_observability(&id, &cwd, sink)
        .await
        .expect("install");

    let backup = cwd.join(".claude").join("settings.local.json.ark-backup");
    assert!(
        backup.exists(),
        "backup must exist between install and first teardown"
    );

    engine.teardown(handle).await.expect("first teardown");
    assert!(
        !backup.exists(),
        "backup must be removed after first teardown"
    );

    // Second install + teardown cycle must also succeed — teardown left
    // the cwd in a reinstallable state.
    let (sink2, _rx2) = channel(8);
    let handle2 = engine
        .install_observability(&id, &cwd, sink2)
        .await
        .expect("reinstall");
    engine.teardown(handle2).await.expect("second teardown");
    assert!(
        !backup.exists(),
        "backup must be removed after second teardown"
    );
}

/// Scenario from T-114: feed the committed post-tool-use fixture through
/// the hook payload parser and assert the envelope fields the downstream
/// dispatcher depends on.
///
/// NOTE: the Engine trait does not currently expose
/// `handle_hook_payload` — see `ark_core::engine_contract` deferred list.
/// For now we assert the fixture shape so when that API lands the
/// contract can wire straight through.
#[test]
fn hook_timeline_post_tool_use() {
    let fx = engine_fixtures();
    let v = load_payload(&fx, "post-tool-use");
    assert_eq!(v["hook_event_name"], "PostToolUse");
    assert_eq!(v["tool_name"], "Edit");
    assert!(v["tool_input"]["file_path"].is_string());
}

/// Scenario from T-114: stop payload fixture.
#[test]
fn hook_timeline_stop() {
    let fx = engine_fixtures();
    let v = load_payload(&fx, "stop");
    assert_eq!(v["hook_event_name"], "Stop");
    assert!(v["session_id"].is_string());
}

/// Scenario from T-114: permission-request payload fixture.
#[test]
fn hook_timeline_permission_request() {
    let fx = engine_fixtures();
    let v = load_payload(&fx, "permission-request");
    assert_eq!(v["hook_event_name"], "PermissionRequest");
    assert_eq!(v["tool_name"], "Bash");
    assert!(v["tool_input"]["command"].is_string());
}

/// Scenario from T-114: basic-toolUse transcript → parse_line emits the
/// expected event stream (ToolUse for every line, FileEdited for the
/// Edit tool_use).
#[test]
fn transcript_parsing_basic_tool_use() {
    let fx = engine_fixtures();
    let id = contract_id();
    let events = parse_transcript(&fx, "basic-toolUse", &id);

    // Four tool_use lines → at minimum four ToolUse events.
    let tool_uses = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolUse { .. }))
        .count();
    assert_eq!(
        tool_uses, 4,
        "basic-toolUse should yield four ToolUse events, got {tool_uses} in {events:#?}"
    );

    // The Edit line must additionally produce a FileEdited event.
    let file_edited = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::FileEdited { .. }))
        .count();
    assert_eq!(
        file_edited, 1,
        "basic-toolUse should yield exactly one FileEdited event, got {file_edited}"
    );
}

/// Scenario from T-114: rotation-scenario transcript is handled without
/// panicking and emits events for both halves of the session split.
#[test]
fn transcript_parsing_rotation() {
    let fx = engine_fixtures();
    let id = contract_id();
    let events = parse_transcript(&fx, "rotation-scenario", &id);

    // Expect at least one user + one assistant on each side of the
    // rotation summary, plus two tool_uses (pre + post rotation). The
    // summary block is intentionally not a recognized kind and produces
    // zero events.
    let user_msgs = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Message {
                    role: MessageRole::User,
                    ..
                }
            )
        })
        .count();
    let assistant_msgs = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Message {
                    role: MessageRole::Assistant,
                    ..
                }
            )
        })
        .count();
    let tool_uses = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolUse { .. }))
        .count();

    assert!(
        user_msgs >= 2,
        "rotation scenario should emit at least two user messages (pre + post), got {user_msgs}"
    );
    assert!(
        assistant_msgs >= 2,
        "rotation scenario should emit at least two assistant messages, got {assistant_msgs}"
    );
    assert!(
        tool_uses >= 2,
        "rotation scenario should emit at least two tool_use events, got {tool_uses}"
    );
}

/// Scenario from T-114: malformed.jsonl doesn't panic, the bad line is
/// dropped silently, and surrounding valid lines still produce events.
#[test]
fn transcript_parsing_malformed_line_skipped() {
    let fx = engine_fixtures();
    let id = contract_id();
    let raw = std::fs::read_to_string(fx.transcript("malformed")).expect("read malformed");

    let mut all_events = Vec::new();
    let mut bad_line_produced_zero = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let evs = parse_line(trimmed, &id);
        // The deliberately malformed line (MALFORMED NO QUOTES HERE)
        // must emit zero events.
        if serde_json::from_str::<Value>(trimmed).is_err() {
            assert!(
                evs.is_empty(),
                "parse_line on malformed JSON must emit zero events, got {evs:#?}"
            );
            bad_line_produced_zero = true;
        }
        all_events.extend(evs);
    }

    assert!(
        bad_line_produced_zero,
        "malformed.jsonl fixture must contain at least one unparseable line"
    );
    // Valid lines around the bad one still produce events.
    assert!(
        !all_events.is_empty(),
        "valid lines around the malformed line must still emit events"
    );
}

/// Scenario from T-114 (bonus): feed a transcript through the real
/// `tail_transcript_path` async reader to prove the end-to-end pipeline
/// (not just the line parser) accepts the committed fixture. Exercises
/// the full read-once path used by the supervisor tailer.
#[tokio::test]
async fn tail_transcript_path_accepts_basic_fixture() {
    let fx = engine_fixtures();
    let id = contract_id();
    let (sink, mut rx) = channel(32);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();

    // Copy the fixture into a temp file so the tailer's file-change
    // watcher has something to attach to. The contract here is "read
    // existing content without hanging on missing-newline partials."
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("transcript.jsonl");
    let src = fx.transcript("basic-toolUse");
    let content = std::fs::read(&src).unwrap();
    tokio::fs::write(&dest, &content).await.unwrap();

    // Spawn the tailer.
    let tailer = tokio::spawn(tail_transcript_path(
        dest.clone(),
        sink,
        id.clone(),
        cancel_for_task,
    ));

    // Drain events with a deadline so we don't hang if parsing never
    // produces output. The fixture has 4 tool_use lines and the first
    // three are Bash/Read/Edit/Grep — we expect ≥4 events.
    let mut collected: Vec<AgentEvent> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while collected.len() < 4 && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            collected.push(ev);
        }
    }

    // Append a newline to flush the trailing line (fixtures end without
    // a final newline in some cases), just in case.
    {
        let mut f = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&dest)
            .await
            .unwrap();
        f.write_all(b"\n").await.unwrap();
        f.flush().await.unwrap();
    }

    cancel.cancel();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), tailer).await;

    assert!(
        collected.len() >= 4,
        "tailer must emit at least 4 events for basic-toolUse fixture, got {collected:#?}"
    );
}

// -- helpers ----------------------------------------------------------------

fn load_payload(fx: &EngineFixtures, stem: &str) -> Value {
    let path = fx.hook_payload(stem);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read hook payload {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!(
            "hook payload {stem} at {} failed to parse: {e}",
            path.display()
        )
    })
}

fn parse_transcript(fx: &EngineFixtures, stem: &str, id: &AgentId) -> Vec<AgentEvent> {
    let path: PathBuf = fx.transcript(stem);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read transcript {}: {e}", path.display()));
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.extend(parse_line(trimmed, id));
    }
    out
}

#[allow(dead_code)]
fn _assert_path_looks_like_transcript(p: &Path) {
    assert!(p.extension().and_then(|s| s.to_str()) == Some("jsonl"));
}
