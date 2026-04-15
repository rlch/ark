//! Integration tests for the `mock-claude` shim binary (T-126).
//!
//! These tests exercise the shipped scripts end-to-end by invoking the
//! compiled binary through `CARGO_BIN_EXE_mock-claude`, the Cargo-provided
//! path that guarantees the binary has been built before the test runs.

use std::path::PathBuf;
use std::process::Command;

use ark_test_fixtures::paths;
use tempfile::tempdir;

/// Path to the mock-claude binary that Cargo builds alongside this test.
fn mock_claude_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock-claude"))
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(paths::MOCK_CLAUDE_SCRIPTS).join(format!("{name}.json"))
}

#[test]
fn happy_path_script_emits_all_events_and_exits_clean() {
    let tmp = tempdir().expect("tempdir");
    let out = tmp.path().join("events.jsonl");

    let status = Command::new(mock_claude_bin())
        .arg("--script")
        .arg(script("happy-path"))
        .arg("--output")
        .arg(&out)
        .status()
        .expect("spawn mock-claude");
    assert!(
        status.success(),
        "happy-path script must exit 0, got {status:?}"
    );

    let body = std::fs::read_to_string(&out).expect("read events.jsonl");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 4, "happy-path emits 4 events, got:\n{body}");
    for line in &lines {
        let v: serde_json::Value =
            serde_json::from_str(line).expect("each emitted line is valid JSON");
        assert!(
            v.get("hook_event_name").is_some(),
            "each envelope must carry hook_event_name: {line}"
        );
    }
    // Final event in the happy-path script is a Stop.
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last.get("hook_event_name").and_then(|v| v.as_str()),
        Some("Stop")
    );
}

#[test]
fn stop_only_script_emits_single_event() {
    let tmp = tempdir().expect("tempdir");
    let out = tmp.path().join("events.jsonl");

    let status = Command::new(mock_claude_bin())
        .arg("--script")
        .arg(script("stop-only"))
        .arg("--output")
        .arg(&out)
        .status()
        .expect("spawn mock-claude");
    assert!(status.success(), "stop-only must exit 0");

    let body = std::fs::read_to_string(&out).expect("read events.jsonl");
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 1, "stop-only must emit exactly one event");
}

#[test]
fn missing_script_exits_two_with_clear_message() {
    let output = Command::new(mock_claude_bin())
        .arg("--script")
        .arg("/definitely/not/a/real/path/mock-script.json")
        .output()
        .expect("spawn mock-claude");
    assert_eq!(
        output.status.code(),
        Some(2),
        "missing script must exit 2, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to read script"),
        "stderr must name the failure, got: {stderr}"
    );
}

#[test]
fn malformed_script_exits_two() {
    let tmp = tempdir().expect("tempdir");
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "{not valid json").expect("write bad script");

    let output = Command::new(mock_claude_bin())
        .arg("--script")
        .arg(&bad)
        .output()
        .expect("spawn mock-claude");
    assert_eq!(
        output.status.code(),
        Some(2),
        "malformed script must exit 2"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to parse script"),
        "stderr must name the parse failure, got: {stderr}"
    );
}

#[test]
fn final_exit_override_is_honoured() {
    let tmp = tempdir().expect("tempdir");
    let script_path = tmp.path().join("exit7.json");
    std::fs::write(
        &script_path,
        r#"{"events":[{"hook_event_name":"Stop"}],"final_exit":7}"#,
    )
    .expect("write exit7 script");

    let status = Command::new(mock_claude_bin())
        .arg("--script")
        .arg(&script_path)
        .status()
        .expect("spawn mock-claude");
    assert_eq!(
        status.code(),
        Some(7),
        "final_exit in script must drive process exit code"
    );
}

#[test]
fn settings_hook_command_is_invoked_per_event() {
    // Build a settings.json whose PostToolUse hook is a shell command that
    // writes a marker file. If mock-claude resolves hooks correctly, the
    // marker should appear after the run.
    let tmp = tempdir().expect("tempdir");
    let marker = tmp.path().join("hook-ran");
    let settings = tmp.path().join("settings.json");
    let cmd = format!("cat >> {}", shell_quote(&marker.to_string_lossy()));
    let settings_json = serde_json::json!({
        "hooks": {
            "Stop": [ { "command": cmd } ]
        }
    });
    std::fs::write(&settings, settings_json.to_string()).expect("write settings");

    let status = Command::new(mock_claude_bin())
        .arg("--script")
        .arg(script("stop-only"))
        .arg("--settings")
        .arg(&settings)
        .status()
        .expect("spawn mock-claude");
    assert!(status.success(), "stop-only w/ hooks must exit 0");

    let body = std::fs::read_to_string(&marker).expect("marker file must exist");
    let v: serde_json::Value = serde_json::from_str(body.trim()).expect("marker JSON");
    assert_eq!(
        v.get("hook_event_name").and_then(|x| x.as_str()),
        Some("Stop"),
        "hook received the Stop event payload on stdin"
    );
}

/// Minimal single-quote shell escape — sufficient for tempdir paths used in
/// the hook dispatch test above.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
