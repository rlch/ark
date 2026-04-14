//! Typed hook payload parser + translator (T-047).
//!
//! Claude Code invokes `ark-hook` with a single JSON document on stdin
//! whose shape is broadly:
//!
//! ```json
//! {
//!   "session_id": "abc123...",
//!   "cwd": "/path/to/cwd",
//!   "hook_event_name": "PostToolUse",
//!   "tool_name": "Edit",
//!   "tool_input": { "file_path": "/repo/src/lib.rs", "...": "..." }
//! }
//! ```
//!
//! [`HookPayload`] mirrors those required fields and keeps every other
//! key inside an [`extra`](HookPayload::extra) map via `#[serde(flatten)]`
//! so forward-compat is preserved. The translator
//! [`payload_to_events`] turns a parsed payload plus the CLI-supplied
//! [`HookEvent`] into one or more [`AgentEvent`] variants per
//! cavekit-hook-ipc.md R1.
//!
//! Scope note: this module **only** parses and translates. JSONL
//! persistence (T-048), zellij pipe forwarding (T-049), and the
//! `PermissionRequest` stdout allow payload (T-050) live elsewhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use ark_types::event::{AgentEvent, MessageRole, Outcome};
use ark_types::id::AgentId;

use crate::event::HookEvent;

/// Maximum characters (not bytes) kept in a `*_summary` field.
///
/// Truncation is performed on a char boundary so we never split a
/// UTF-8 code point.
pub const SUMMARY_MAX_CHARS: usize = 80;

/// Tool names that edit files on disk. Used to decide when a
/// [`PostToolUse`](HookEvent::PostToolUse) additionally emits a
/// [`FileEdited`](AgentEvent::FileEdited) event.
///
/// Additions/deletions default to zero here — real numstat comes from
/// the orchestrator git diff watcher in T-082.
pub const FILE_EDIT_TOOLS: &[&str] = &["Edit", "Write", "NotebookEdit", "MultiEdit"];

/// Typed Claude Code hook payload.
///
/// `extra` captures every field not enumerated above so future
/// Claude-side additions reach the translator without a crate rebuild.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayload {
    /// Claude's session id — consumed by T-053 transcript tailing.
    pub session_id: String,
    /// Working directory Claude was running in when the hook fired.
    pub cwd: PathBuf,
    /// Mirrors Claude's `hook_event_name` (PascalCase, e.g. `PostToolUse`).
    pub hook_event_name: String,
    /// Name of the tool the hook fired for (present for PostToolUse,
    /// PermissionRequest, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Raw tool input — shape is tool-dependent so we keep it loose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    /// Forward-compat bucket for unknown top-level keys.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Truncate `s` to at most [`SUMMARY_MAX_CHARS`] characters on a UTF-8
/// char boundary. Returns a new `String`.
pub fn truncate_summary(s: &str) -> String {
    truncate_chars(s, SUMMARY_MAX_CHARS)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    // Take the first `max_chars` chars (char-boundary-safe by construction).
    s.chars().take(max_chars).collect()
}

/// Build the `input_summary` string from an optional `tool_input` value.
///
/// `None` or serialization failure → empty string (parser must not
/// panic on exotic payloads per R3).
fn summarize_tool_input(tool_input: Option<&serde_json::Value>) -> String {
    let Some(v) = tool_input else {
        return String::new();
    };
    let s = serde_json::to_string(v).unwrap_or_default();
    truncate_summary(&s)
}

/// Pull `tool_input.file_path` as a [`PathBuf`] if it is a non-empty
/// string. Returns `None` for every other shape.
fn extract_file_path(tool_input: Option<&serde_json::Value>) -> Option<PathBuf> {
    let v = tool_input?.as_object()?.get("file_path")?.as_str()?;
    if v.is_empty() {
        return None;
    }
    Some(PathBuf::from(v))
}

/// Pull a string field out of `payload.extra` (forward-compat fields
/// that didn't make it into the typed struct).
fn extra_string<'a>(payload: &'a HookPayload, key: &str) -> Option<&'a str> {
    payload.extra.get(key).and_then(|v| v.as_str())
}

/// Translate a parsed hook payload into the matching [`AgentEvent`]
/// variants.
///
/// Mapping per cavekit-hook-ipc.md R1:
/// - `PostToolUse` → `ToolUse` (+ `FileEdited` when `tool_name` is one
///   of [`FILE_EDIT_TOOLS`] and `tool_input.file_path` extracts).
/// - `Stop` → `Done { outcome: Success { artifacts: [] } }`.
/// - `SessionEnd` → `Done { outcome: Success { artifacts: [] } }`
///   (caller dedupes against a prior `Stop`).
/// - `PermissionRequest` → `PermissionAsked`.
/// - `Notification` → `Message { role: System, .. }`.
/// - `TaskCompleted` → `TaskDone`.
///
/// Caller passes the `HookEvent` (from `--event`) rather than reading
/// `payload.hook_event_name` so clap-validated dispatch stays the
/// source of truth — the payload field is preserved verbatim as
/// [`HookPayload::hook_event_name`] for audit only.
pub fn payload_to_events(payload: &HookPayload, id: &AgentId, event: HookEvent) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match event {
        HookEvent::PostToolUse => {
            let tool = payload.tool_name.clone().unwrap_or_default();
            let input_summary = summarize_tool_input(payload.tool_input.as_ref());
            out.push(AgentEvent::ToolUse {
                id: id.clone(),
                tool: tool.clone(),
                input_summary,
            });
            if FILE_EDIT_TOOLS.iter().any(|t| *t == tool) {
                if let Some(path) = extract_file_path(payload.tool_input.as_ref()) {
                    out.push(AgentEvent::FileEdited {
                        id: id.clone(),
                        path,
                        additions: 0,
                        deletions: 0,
                    });
                }
            }
        }
        HookEvent::Stop => {
            out.push(AgentEvent::Done {
                id: id.clone(),
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        }
        HookEvent::SessionEnd => {
            out.push(AgentEvent::Done {
                id: id.clone(),
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        }
        HookEvent::PermissionRequest => {
            let tool = payload
                .tool_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let summary = summarize_tool_input(payload.tool_input.as_ref());
            out.push(AgentEvent::PermissionAsked {
                id: id.clone(),
                tool,
                summary,
            });
        }
        HookEvent::Notification => {
            // Prefer an explicit `message` field, fall back to serialized extras.
            let summary = extra_string(payload, "message")
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if payload.extra.is_empty() {
                        String::new()
                    } else {
                        serde_json::to_string(&payload.extra).unwrap_or_default()
                    }
                });
            out.push(AgentEvent::Message {
                id: id.clone(),
                role: MessageRole::System,
                summary: truncate_summary(&summary),
            });
        }
        HookEvent::TaskCompleted => {
            // TaskCompleted payloads aren't spec'd yet — best-effort pull
            // of `task_id` / label-ish fields from `extra`.
            let task_id = extra_string(payload, "task_id")
                .map(|s| s.to_string())
                .unwrap_or_default();
            let raw_label = extra_string(payload, "description")
                .or_else(|| extra_string(payload, "label"))
                .or_else(|| extra_string(payload, "message"))
                .map(|s| s.to_string())
                .unwrap_or_else(|| summarize_tool_input(payload.tool_input.as_ref()));
            let label = if raw_label.is_empty() {
                None
            } else {
                Some(truncate_summary(&raw_label))
            };
            out.push(AgentEvent::TaskDone {
                id: id.clone(),
                task_id,
                label,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use ark_types::event::{AgentEvent, MessageRole, Outcome};
    use ark_types::id::AgentId;

    fn id() -> AgentId {
        AgentId::new("cavekit", "hooktest")
    }

    fn base_payload(event_name: &str) -> HookPayload {
        HookPayload {
            session_id: "sess-1".into(),
            cwd: PathBuf::from("/tmp"),
            hook_event_name: event_name.into(),
            tool_name: None,
            tool_input: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn post_tool_use_edit_emits_tool_use_and_file_edited() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Edit".into());
        p.tool_input = Some(serde_json::json!({
            "file_path": "/repo/src/lib.rs",
            "old_string": "foo",
            "new_string": "bar",
        }));
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 2);
        match &events[0] {
            AgentEvent::ToolUse {
                tool,
                input_summary,
                ..
            } => {
                assert_eq!(tool, "Edit");
                assert!(!input_summary.is_empty());
                assert!(input_summary.chars().count() <= SUMMARY_MAX_CHARS);
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
        match &events[1] {
            AgentEvent::FileEdited {
                path,
                additions,
                deletions,
                ..
            } => {
                assert_eq!(path, &PathBuf::from("/repo/src/lib.rs"));
                assert_eq!(*additions, 0);
                assert_eq!(*deletions, 0);
            }
            other => panic!("expected FileEdited, got {other:?}"),
        }
    }

    #[test]
    fn post_tool_use_read_emits_tool_use_only() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Read".into());
        p.tool_input = Some(serde_json::json!({ "file_path": "/repo/src/lib.rs" }));
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgentEvent::ToolUse { .. }));
    }

    #[test]
    fn post_tool_use_write_emits_file_edited() {
        for tool in ["Write", "NotebookEdit", "MultiEdit"] {
            let mut p = base_payload("PostToolUse");
            p.tool_name = Some(tool.to_string());
            p.tool_input = Some(serde_json::json!({ "file_path": "/x" }));
            let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
            assert_eq!(events.len(), 2, "tool={tool}");
            assert!(matches!(events[1], AgentEvent::FileEdited { .. }));
        }
    }

    #[test]
    fn post_tool_use_edit_without_file_path_skips_file_edited() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Edit".into());
        p.tool_input = Some(serde_json::json!({ "no_path": true }));
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AgentEvent::ToolUse { .. }));
    }

    #[test]
    fn stop_emits_done_success() {
        let p = base_payload("Stop");
        let events = payload_to_events(&p, &id(), HookEvent::Stop);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Done {
                outcome: Outcome::Success { artifacts },
                ..
            } => {
                assert!(artifacts.is_empty());
            }
            other => panic!("expected Done Success, got {other:?}"),
        }
    }

    #[test]
    fn session_end_emits_done_success() {
        let p = base_payload("SessionEnd");
        let events = payload_to_events(&p, &id(), HookEvent::SessionEnd);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            AgentEvent::Done {
                outcome: Outcome::Success { .. },
                ..
            }
        ));
    }

    #[test]
    fn permission_request_emits_asked_with_tool_name() {
        let mut p = base_payload("PermissionRequest");
        p.tool_name = Some("Bash".into());
        p.tool_input = Some(serde_json::json!({ "command": "ls" }));
        let events = payload_to_events(&p, &id(), HookEvent::PermissionRequest);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::PermissionAsked { tool, summary, .. } => {
                assert_eq!(tool, "Bash");
                assert!(summary.contains("command"));
                assert!(summary.chars().count() <= SUMMARY_MAX_CHARS);
            }
            other => panic!("expected PermissionAsked, got {other:?}"),
        }
    }

    #[test]
    fn permission_request_missing_tool_name_falls_back_to_unknown() {
        let p = base_payload("PermissionRequest");
        let events = payload_to_events(&p, &id(), HookEvent::PermissionRequest);
        match &events[0] {
            AgentEvent::PermissionAsked { tool, .. } => assert_eq!(tool, "unknown"),
            other => panic!("expected PermissionAsked, got {other:?}"),
        }
    }

    #[test]
    fn notification_emits_message_system() {
        let mut p = base_payload("Notification");
        p.extra
            .insert("message".into(), serde_json::json!("hello world"));
        let events = payload_to_events(&p, &id(), HookEvent::Notification);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Message { role, summary, .. } => {
                assert_eq!(*role, MessageRole::System);
                assert_eq!(summary, "hello world");
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn task_completed_emits_task_done() {
        let mut p = base_payload("TaskCompleted");
        p.extra.insert("task_id".into(), serde_json::json!("T-123"));
        p.extra
            .insert("description".into(), serde_json::json!("refactor foo"));
        let events = payload_to_events(&p, &id(), HookEvent::TaskCompleted);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::TaskDone { task_id, label, .. } => {
                assert_eq!(task_id, "T-123");
                assert_eq!(label.as_deref(), Some("refactor foo"));
            }
            other => panic!("expected TaskDone, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_with_missing_input_does_not_panic() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("SomeUnknownTool".into());
        p.tool_input = None;
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::ToolUse {
                tool,
                input_summary,
                ..
            } => {
                assert_eq!(tool, "SomeUnknownTool");
                assert_eq!(input_summary, "");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn missing_tool_name_post_tool_use_uses_empty_string() {
        let p = base_payload("PostToolUse");
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::ToolUse {
                tool,
                input_summary,
                ..
            } => {
                assert_eq!(tool, "");
                assert_eq!(input_summary, "");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn summary_truncation_caps_at_80_chars_on_char_boundary() {
        // Build a >80-char payload with multi-byte chars to prove we cut
        // on a char boundary, not a byte boundary.
        let big = "é".repeat(200); // 200 chars, 400 bytes
        let summary = truncate_summary(&big);
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS);
        // Round-trip through to_string should not panic / mangle.
        assert!(summary.is_char_boundary(summary.len()));
    }

    #[test]
    fn summary_truncation_under_limit_is_unchanged() {
        let s = "hello";
        assert_eq!(truncate_summary(s), "hello");
    }

    #[test]
    fn serde_round_trip_preserves_extra_fields() {
        let raw = serde_json::json!({
            "session_id": "s1",
            "cwd": "/tmp",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/x" },
            "permission_mode": "auto",
            "transcript_path": "/tmp/tx.jsonl",
            "future_field": { "nested": true },
        });
        let p: HookPayload = serde_json::from_value(raw.clone()).expect("parse");
        assert_eq!(p.session_id, "s1");
        assert_eq!(p.cwd, PathBuf::from("/tmp"));
        assert_eq!(p.hook_event_name, "PostToolUse");
        assert_eq!(p.tool_name.as_deref(), Some("Edit"));
        assert!(p.extra.contains_key("permission_mode"));
        assert!(p.extra.contains_key("transcript_path"));
        assert!(p.extra.contains_key("future_field"));

        // Round-trip: serialize then deserialize, extra fields must survive.
        let encoded = serde_json::to_value(&p).expect("ser");
        let p2: HookPayload = serde_json::from_value(encoded).expect("reparse");
        assert_eq!(p, p2);
    }

    #[test]
    fn hook_event_name_field_preserved_verbatim() {
        let p = base_payload("PostToolUse");
        assert_eq!(p.hook_event_name, "PostToolUse");
    }

    #[test]
    fn post_tool_use_long_input_summary_truncated() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Bash".into());
        // Long command string so serialized tool_input clearly exceeds 80 chars.
        let long_cmd = "a".repeat(500);
        p.tool_input = Some(serde_json::json!({ "command": long_cmd }));
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        match &events[0] {
            AgentEvent::ToolUse { input_summary, .. } => {
                assert_eq!(input_summary.chars().count(), SUMMARY_MAX_CHARS);
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }
}
