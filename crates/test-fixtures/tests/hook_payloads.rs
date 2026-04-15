//! T-113 — contract tests for the committed hook-payload fixtures.
//!
//! These tests assert that every event-type fixture in
//! `tests/fixtures/hook-payloads/` exists, parses as JSON, and carries the
//! envelope fields the `ark-hook` payload parser requires
//! (`session_id`, `cwd`, `hook_event_name`) plus the event-specific
//! `hook_event_name` value expected for that fixture name.
//!
//! If Claude Code's hook envelope shape evolves, updating the fixture JSON
//! here should be enough — the parser contract is covered in-crate by
//! `crates/hook/src/payload.rs`.

use std::path::Path;

use ark_test_fixtures::{loaders, paths};
use serde_json::Value;

/// (fixture stem, expected `hook_event_name` string).
///
/// Order mirrors the build-site T-113 enumeration: PostToolUse, Stop,
/// PermissionRequest, Notification, SessionEnd, TaskCompleted, plus two
/// variant fixtures exercising error/denied paths.
const FIXTURES: &[(&str, &str)] = &[
    ("post-tool-use", "PostToolUse"),
    ("post-tool-use-error", "PostToolUse"),
    ("stop", "Stop"),
    ("permission-request", "PermissionRequest"),
    ("permission-request-denied", "PermissionRequest"),
    ("notification", "Notification"),
    ("session-end", "SessionEnd"),
    ("task-completed", "TaskCompleted"),
];

fn parse_fixture(stem: &str) -> Value {
    let raw = loaders::load_hook_payload(stem);
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("fixture `{stem}` is not valid JSON: {err}"))
}

#[test]
fn every_fixture_file_exists_on_disk() {
    for (stem, _) in FIXTURES {
        let p = Path::new(paths::HOOK_PAYLOADS).join(format!("{stem}.json"));
        assert!(p.is_file(), "missing fixture: {}", p.display());
    }
}

#[test]
fn every_fixture_parses_as_json_object() {
    for (stem, _) in FIXTURES {
        let v = parse_fixture(stem);
        assert!(v.is_object(), "fixture `{stem}` must be a JSON object");
    }
}

#[test]
fn every_fixture_carries_required_envelope_fields() {
    for (stem, _) in FIXTURES {
        let v = parse_fixture(stem);
        let obj = v.as_object().expect("object");
        for field in ["session_id", "cwd", "hook_event_name"] {
            assert!(
                obj.contains_key(field),
                "fixture `{stem}` missing required field `{field}`"
            );
            assert!(
                obj[field].is_string(),
                "fixture `{stem}` field `{field}` must be a string"
            );
        }
    }
}

#[test]
fn every_fixture_declares_expected_event_name() {
    for (stem, expected_event) in FIXTURES {
        let v = parse_fixture(stem);
        let actual = v["hook_event_name"]
            .as_str()
            .unwrap_or_else(|| panic!("fixture `{stem}` hook_event_name not a string"));
        assert_eq!(
            actual, *expected_event,
            "fixture `{stem}` hook_event_name mismatch"
        );
    }
}

#[test]
fn post_tool_use_fixture_has_tool_name_and_tool_input() {
    let v = parse_fixture("post-tool-use");
    assert!(v["tool_name"].is_string(), "tool_name must be a string");
    assert!(v["tool_input"].is_object(), "tool_input must be an object");
    // file edit payloads in the ark hook parser extract `file_path` when
    // the tool is one of the FILE_EDIT_TOOLS list. The canonical fixture
    // targets `Edit` so it should exercise that branch.
    assert_eq!(v["tool_name"].as_str(), Some("Edit"));
    assert!(
        v["tool_input"]["file_path"].is_string(),
        "Edit fixtures should carry tool_input.file_path"
    );
}

#[test]
fn permission_request_fixture_has_tool_name() {
    let v = parse_fixture("permission-request");
    assert_eq!(v["tool_name"].as_str(), Some("Bash"));
    assert!(v["tool_input"].is_object());
}

#[test]
fn notification_fixture_has_message_field() {
    let v = parse_fixture("notification");
    // The translator prefers an explicit `message` extra, so this
    // mirrors that expectation.
    assert!(
        v["message"].is_string(),
        "notification fixture should expose a `message` string extra"
    );
}

#[test]
fn task_completed_fixture_has_task_id_and_description() {
    let v = parse_fixture("task-completed");
    assert!(v["task_id"].is_string(), "task_id must be a string");
    assert!(v["description"].is_string(), "description must be a string");
}
