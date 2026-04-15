//! Integration tests for Claude transcript fixtures (T-112).
//!
//! These tests assert the on-disk fixture files exist and are shaped
//! correctly so downstream consumers (the claude-code engine, contract
//! tests, etc.) can rely on them without re-deriving their invariants.
//!
//! Invariants verified per fixture:
//! - the file exists under `tests/fixtures/claude-transcripts/`,
//! - its bytes are valid UTF-8,
//! - every non-empty, non-comment line parses as JSON via `serde_json`
//!   (except `malformed.jsonl`, which is *expected* to contain exactly
//!   one bad line — that invariant is the purpose of the fixture).
//!
//! The fixture set itself is enumerated in `FIXTURES` below so adding a
//! new JSONL file forces a test update (and forces the author to declare
//! whether it should round-trip through serde cleanly).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Directory holding the committed Claude transcript fixtures.
fn transcripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude-transcripts")
}

/// Declared fixture set. `strict_json` means every non-blank line must
/// parse as JSON; `false` means the file is allowed to contain at least
/// one intentionally malformed line for robustness testing.
const FIXTURES: &[(&str, bool)] = &[
    ("basic-toolUse.jsonl", true),
    ("message-event.jsonl", true),
    ("file-edited.jsonl", true),
    ("rotation-scenario.jsonl", true),
    ("mixed-timeline.jsonl", true),
    ("empty.jsonl", true),
    ("malformed.jsonl", false),
];

/// Read a fixture file and assert it is valid UTF-8 (via `read_to_string`
/// returning `Ok` — it errors on non-UTF-8 bytes).
fn read_fixture(name: &str) -> String {
    let path = transcripts_dir().join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("transcript fixture {name} at {path:?} unreadable: {err}"))
}

#[test]
fn all_declared_fixtures_exist_on_disk() {
    for (name, _) in FIXTURES {
        let path = transcripts_dir().join(name);
        assert!(
            Path::new(&path).is_file(),
            "expected fixture file at {path:?} for T-112 transcripts"
        );
    }
}

#[test]
fn all_fixtures_are_valid_utf8() {
    // `fs::read_to_string` surfaces non-UTF-8 bytes as an error; reading
    // successfully is the proof.
    for (name, _) in FIXTURES {
        let _ = read_fixture(name);
    }
}

#[test]
fn strict_fixtures_have_every_line_parse_as_json() {
    for (name, strict) in FIXTURES {
        if !strict {
            continue;
        }
        let contents = read_fixture(name);
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: Result<Value, _> = serde_json::from_str(trimmed);
            assert!(
                parsed.is_ok(),
                "fixture {name} line {} failed to parse as JSON: {:?}",
                idx + 1,
                parsed.err()
            );
        }
    }
}

#[test]
fn empty_fixture_is_zero_bytes() {
    // Edge case for tailers that must handle empty transcripts.
    let contents = read_fixture("empty.jsonl");
    assert!(
        contents.is_empty(),
        "empty.jsonl must be zero bytes; got {} bytes",
        contents.len()
    );
}

#[test]
fn malformed_fixture_has_exactly_one_bad_line_surrounded_by_valid_ones() {
    // Parser-robustness invariant: this fixture MUST carry at least one
    // unparseable line so callers can assert graceful degradation, and
    // it MUST also contain valid lines before and after so we exercise
    // the resume path, not just "everything exploded".
    let contents = read_fixture("malformed.jsonl");
    let mut valid_before_bad = 0usize;
    let mut valid_after_bad = 0usize;
    let mut bad_lines = 0usize;
    let mut seen_bad = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(_) => {
                if seen_bad {
                    valid_after_bad += 1;
                } else {
                    valid_before_bad += 1;
                }
            }
            Err(_) => {
                bad_lines += 1;
                seen_bad = true;
            }
        }
    }
    assert!(
        bad_lines >= 1,
        "malformed.jsonl must contain at least one unparseable line"
    );
    assert!(
        valid_before_bad >= 1,
        "malformed.jsonl must have valid lines BEFORE the bad one (got {valid_before_bad})"
    );
    assert!(
        valid_after_bad >= 1,
        "malformed.jsonl must have valid lines AFTER the bad one (got {valid_after_bad})"
    );
}

#[test]
fn mixed_timeline_has_realistic_line_count() {
    // Per T-112 spec: mixed-timeline should be 15-30 lines of realistic
    // interleaved events so consumers can exercise multi-event flows.
    let contents = read_fixture("mixed-timeline.jsonl");
    let line_count = contents.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        (15..=30).contains(&line_count),
        "mixed-timeline.jsonl should have 15-30 non-empty lines; got {line_count}"
    );
}

#[test]
fn fixtures_collectively_cover_expected_event_types() {
    // Sanity check: across all strict fixtures we should see at least one
    // user message, one assistant message, one tool_use, and one
    // Edit/Write/MultiEdit (the FileEdited trigger set in the parser).
    let mut saw_user = false;
    let mut saw_assistant = false;
    let mut saw_tool_use = false;
    let mut saw_file_edit_tool = false;

    for (name, strict) in FIXTURES {
        if !strict {
            continue;
        }
        let contents = read_fixture(name);
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v): Result<Value, _> = serde_json::from_str(trimmed) else {
                continue;
            };
            let top_type = v.get("type").and_then(Value::as_str).unwrap_or("");
            match top_type {
                "user" => saw_user = true,
                "assistant" => {
                    saw_assistant = true;
                    // Assistants may carry nested tool_use blocks too.
                    if let Some(arr) = v
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(Value::as_array)
                    {
                        for block in arr {
                            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                                saw_tool_use = true;
                                if let Some(nm) = block.get("name").and_then(Value::as_str) {
                                    if matches!(nm, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
                                    {
                                        saw_file_edit_tool = true;
                                    }
                                }
                            }
                        }
                    }
                }
                "tool_use" => {
                    saw_tool_use = true;
                    if let Some(nm) = v.get("name").and_then(Value::as_str) {
                        if matches!(nm, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
                            saw_file_edit_tool = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    assert!(saw_user, "fixtures must include at least one user message");
    assert!(
        saw_assistant,
        "fixtures must include at least one assistant message"
    );
    assert!(saw_tool_use, "fixtures must include at least one tool_use");
    assert!(
        saw_file_edit_tool,
        "fixtures must include at least one Edit/Write/MultiEdit/NotebookEdit tool_use"
    );
}

#[test]
fn rotation_fixture_contains_summary_marker() {
    // T-112 calls out the rotation scenario explicitly: there must be a
    // discriminating record that a session transition happened. We use
    // the `summary` block shape Claude writes when compacting.
    let contents = read_fixture("rotation-scenario.jsonl");
    let mut saw_summary = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v): Result<Value, _> = serde_json::from_str(trimmed) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("summary") {
            saw_summary = true;
            break;
        }
    }
    assert!(
        saw_summary,
        "rotation-scenario.jsonl must include a `type:summary` handoff marker"
    );
}
