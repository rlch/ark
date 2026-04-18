//! Integration tests for the `mock-claude-cc` fixture binary — T-017
//! timelines + T-018 transcript synth.
//!
//! These tests invoke the compiled binary via
//! `CARGO_BIN_EXE_mock-claude-cc` (cargo populates that env var for
//! each crate's own test targets) and assert on the NDJSON it writes
//! to stdout + the transcript JSONL it writes to
//! `--transcript-write PATH`. Both shapes are contracts other crates
//! rely on (T-036 expanded view, T-044 list columns), so the tests pin
//! every field downstream code reads.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

/// Absolute path to the compiled `mock-claude-cc` binary. Cargo sets
/// this env var on every test target in the binary's own package.
fn mock_claude_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock-claude-cc"))
}

/// Run the binary with the given args and return `(stdout, stderr)`.
/// Panics on non-zero exit so failing invocations surface the stderr
/// directly in the test output.
fn run(args: &[&str]) -> (String, String) {
    let out = Command::new(mock_claude_bin())
        .args(args)
        .output()
        .expect("spawn mock-claude");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8(out.stderr).expect("utf8 stderr");
    assert!(
        out.status.success(),
        "mock-claude exited non-zero: status={:?} stderr={stderr}",
        out.status
    );
    (stdout, stderr)
}

/// Parse `stdout` as a sequence of NDJSON frames, asserting each line
/// decodes successfully + returning the resulting `Vec<Value>`.
fn parse_ndjson(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("ndjson parse"))
        .collect()
}

// ---------- T-017: --emit-only ----------

#[test]
fn emit_only_writes_happy_path_timeline() {
    let (stdout, _) = run(&["--emit-only"]);
    let frames = parse_ndjson(&stdout);

    let kinds: Vec<String> = frames
        .iter()
        .map(|f| f["kind"].as_str().expect("kind string").to_string())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "SessionEnd",
        ]
    );

    // Every frame carries the deterministic default session id +
    // emitted_at stamp.
    for f in &frames {
        assert_eq!(f["session_id"], "mock-sess");
        assert_eq!(f["emitted_at"], "2026-04-18T00:00:00Z");
        // Payload includes hook_event_name matching `kind` for the
        // ark-side translator's sanity check.
        assert_eq!(f["payload"]["hook_event_name"], f["kind"]);
    }

    // PreToolUse + PostToolUse carry the T-015 tool_name / tool_input
    // fields.
    let pre = &frames[2];
    assert_eq!(pre["payload"]["tool_name"], "Edit");
    assert!(pre["payload"]["tool_input"]["file_path"].is_string());

    let post = &frames[3];
    assert_eq!(post["payload"]["tool_name"], "Edit");
    assert!(post["payload"]["tool_response"]["status"].is_string());
}

// ---------- T-017: --subagent-burst ----------

#[test]
fn subagent_burst_emits_n_pairs_inside_session_envelope() {
    let (stdout, _) = run(&["--subagent-burst", "3"]);
    let frames = parse_ndjson(&stdout);

    let kinds: Vec<&str> = frames
        .iter()
        .map(|f| f["kind"].as_str().expect("kind string"))
        .collect();

    // Session envelope + 3 Start/Stop pairs + SessionEnd = 8 frames.
    assert_eq!(kinds.len(), 8);
    assert_eq!(kinds.first(), Some(&"SessionStart"));
    assert_eq!(kinds.last(), Some(&"SessionEnd"));

    let inner: Vec<&str> = kinds[1..kinds.len() - 1].to_vec();
    assert_eq!(
        inner,
        vec![
            "SubagentStart",
            "SubagentStop",
            "SubagentStart",
            "SubagentStop",
            "SubagentStart",
            "SubagentStop",
        ]
    );

    // Agent ids index monotonically.
    let starts: Vec<&Value> = frames
        .iter()
        .filter(|f| f["kind"] == "SubagentStart")
        .collect();
    let stops: Vec<&Value> = frames
        .iter()
        .filter(|f| f["kind"] == "SubagentStop")
        .collect();
    for (i, f) in starts.iter().enumerate() {
        assert_eq!(f["payload"]["agent_id"], format!("agent-{i}"));
        assert_eq!(f["payload"]["agent_type"], "code-writer");
        assert!(f["payload"]["agent_transcript_path"].is_string());
    }
    for (i, f) in stops.iter().enumerate() {
        assert_eq!(f["payload"]["agent_id"], format!("agent-{i}"));
        assert_eq!(
            f["payload"]["last_assistant_message"],
            format!("subagent agent-{i} done")
        );
        assert!(f["payload"]["agent_transcript_path"].is_string());
    }
}

#[test]
fn subagent_burst_zero_still_emits_session_envelope() {
    let (stdout, _) = run(&["--subagent-burst", "0"]);
    let frames = parse_ndjson(&stdout);
    let kinds: Vec<&str> = frames.iter().map(|f| f["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["SessionStart", "SessionEnd"]);
}

// ---------- T-018: --transcript-write ----------

#[test]
fn transcript_write_appends_jsonl_in_real_shape() {
    let td = TempDir::new().unwrap();
    let tpath = td.path().join("deep/dir/transcript.jsonl");
    let (_, _) = run(&[
        "--subagent-burst",
        "2",
        "--transcript-write",
        tpath.to_str().unwrap(),
    ]);

    let raw = fs::read_to_string(&tpath).expect("transcript written");
    let lines: Vec<Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("ndjson parse"))
        .collect();
    assert_eq!(lines.len(), 2, "one line per subagent");

    for line in &lines {
        assert_eq!(line["type"], "message");
        assert_eq!(line["role"], "assistant");
        assert_eq!(line["model"], "claude-sonnet-4-6");
        assert!(line["usage"]["input_tokens"].is_number());
        assert!(line["usage"]["output_tokens"].is_number());
        assert!(line["cost_usd"].is_number());
        let content = line["content"].as_array().expect("content array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert!(content[0]["text"].is_string());
    }
}

#[test]
fn transcript_write_survives_parent_dir_missing() {
    // Parent dir doesn't exist — the binary must create it on demand.
    let td = TempDir::new().unwrap();
    let tpath = td.path().join("a/b/c/t.jsonl");
    assert!(!tpath.parent().unwrap().exists());
    let _ = run(&["--emit-only", "--transcript-write", tpath.to_str().unwrap()]);
    assert!(tpath.exists());
    let raw = fs::read_to_string(&tpath).unwrap();
    // Happy-path emits exactly one assistant message.
    assert_eq!(raw.lines().count(), 1);
}

// ---------- Error cases ----------

#[test]
fn conflicting_modes_error() {
    let out = Command::new(mock_claude_bin())
        .args(["--emit-only", "--subagent-burst", "1"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "conflicting modes should fail");
}

#[test]
fn no_mode_is_a_noop_exit_zero() {
    // Empty invocation: exit 0, no stdout, no transcript writer needed.
    let (stdout, _) = run(&[]);
    assert!(
        stdout.trim().is_empty(),
        "expected no stdout, got: {stdout}"
    );
}
