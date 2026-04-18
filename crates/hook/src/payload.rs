//! Typed hook payload parser + translator.
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
//! [`HookEvent`] into one or more [`CoreEvent::Ext`] envelopes carrying
//! the `"claude-code"` extension tag.
//!
//! Under cavekit-soul Phase 1 the old `AgentEvent` enum is gone; hook
//! payloads now ride as extension-owned JSON inside the
//! `CoreEvent::Ext(ExtEvent { ext: "claude-code", kind: … })` envelope.
//! Methodology-flavoured consumers re-home inside extensions in Phase 4+.
//!
//! Scope note: this module **only** parses and translates. JSONL
//! persistence, zellij pipe forwarding, and the `PermissionRequest`
//! stdout allow payload live elsewhere.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use ark_types::{CoreEvent, ExtEvent, SessionId};

use crate::event::HookEvent;

/// Extension name emitted as the `ext` field of every translated event.
pub const EXT_NAME: &str = "claude-code";

/// Maximum characters (not bytes) kept in a `*_summary` field.
///
/// Truncation is performed on a char boundary so we never split a
/// UTF-8 code point.
pub const SUMMARY_MAX_CHARS: usize = 80;

/// Tool names that edit files on disk. Used to decide when a
/// [`PostToolUse`](HookEvent::PostToolUse) additionally emits a
/// `file_edited` event.
pub const FILE_EDIT_TOOLS: &[&str] = &["Edit", "Write", "NotebookEdit", "MultiEdit"];

/// Typed Claude Code hook payload.
///
/// `extra` captures every field not enumerated above so future
/// Claude-side additions reach the translator without a crate rebuild.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayload {
    /// Claude's session id.
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

/// Build a `CoreEvent::Ext` envelope tagged with `ext="claude-code"`.
fn ext_event(kind: &str, payload: serde_json::Value) -> CoreEvent {
    CoreEvent::Ext(ExtEvent {
        ext: EXT_NAME.to_string(),
        kind: kind.to_string(),
        payload,
    })
}

/// Translate a parsed hook payload into the matching [`CoreEvent::Ext`]
/// envelopes.
///
/// Mapping (extension kinds, snake_case):
/// - `PostToolUse` → `tool.use` (+ `file.edited` when `tool_name` is one
///   of [`FILE_EDIT_TOOLS`] and `tool_input.file_path` extracts).
/// - `Stop` → `task.stopped`.
/// - `SessionEnd` → `session.ended`.
/// - `PermissionRequest` → `permission.asked`.
/// - `Notification` → `message.system`.
/// - `TaskCompleted` → `task.done`.
pub fn payload_to_events(
    payload: &HookPayload,
    id: &SessionId,
    event: HookEvent,
) -> Vec<CoreEvent> {
    let id_str = id.as_str();
    let mut out = Vec::new();
    match event {
        HookEvent::PostToolUse => {
            let tool = payload.tool_name.clone().unwrap_or_default();
            let input_summary = summarize_tool_input(payload.tool_input.as_ref());
            out.push(ext_event(
                "tool.use",
                serde_json::json!({
                    "id": id_str,
                    "tool": tool,
                    "input_summary": input_summary,
                }),
            ));
            if FILE_EDIT_TOOLS.iter().any(|t| *t == tool) {
                if let Some(path) = extract_file_path(payload.tool_input.as_ref()) {
                    out.push(ext_event(
                        "file.edited",
                        serde_json::json!({
                            "id": id_str,
                            "path": path,
                            "additions": 0,
                            "deletions": 0,
                        }),
                    ));
                }
            }
        }
        HookEvent::Stop => {
            out.push(ext_event(
                "task.stopped",
                serde_json::json!({ "id": id_str }),
            ));
        }
        HookEvent::SessionEnd => {
            out.push(ext_event(
                "session.ended",
                serde_json::json!({ "id": id_str }),
            ));
        }
        HookEvent::PermissionRequest => {
            let tool = payload
                .tool_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let summary = summarize_tool_input(payload.tool_input.as_ref());
            out.push(ext_event(
                "permission.asked",
                serde_json::json!({
                    "id": id_str,
                    "tool": tool,
                    "summary": summary,
                }),
            ));
        }
        HookEvent::Notification => {
            let summary = extra_string(payload, "message")
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    if payload.extra.is_empty() {
                        String::new()
                    } else {
                        serde_json::to_string(&payload.extra).unwrap_or_default()
                    }
                });
            out.push(ext_event(
                "message.system",
                serde_json::json!({
                    "id": id_str,
                    "summary": truncate_summary(&summary),
                }),
            ));
        }
        HookEvent::TaskCompleted => {
            let task_id = extra_string(payload, "task_id")
                .map(|s| s.to_string())
                .unwrap_or_default();
            let raw_label = extra_string(payload, "description")
                .or_else(|| extra_string(payload, "label"))
                .or_else(|| extra_string(payload, "message"))
                .map(|s| s.to_string())
                .unwrap_or_else(|| summarize_tool_input(payload.tool_input.as_ref()));
            let label = if raw_label.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(truncate_summary(&raw_label))
            };
            out.push(ext_event(
                "task.done",
                serde_json::json!({
                    "id": id_str,
                    "task_id": task_id,
                    "label": label,
                }),
            ));
        }
    }
    out
}

/// Short kind discriminant from a translated `CoreEvent::Ext`. Used by
/// `run.rs` for tracing labels.
pub fn event_kind(ev: &CoreEvent) -> &str {
    match ev {
        CoreEvent::Ext(e) => e.kind.as_str(),
        CoreEvent::Log { .. } => "log",
        CoreEvent::Error { .. } => "error",
        CoreEvent::SessionStarted { .. } => "session.started",
        CoreEvent::SessionEnded { .. } => "session.ended",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> SessionId {
        SessionId::new("hooktest")
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

    fn first_ext(ev: &CoreEvent) -> &ExtEvent {
        match ev {
            CoreEvent::Ext(e) => e,
            other => panic!("expected Ext, got {other:?}"),
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
        let a = first_ext(&events[0]);
        assert_eq!(a.kind, "tool.use");
        assert_eq!(a.payload.get("tool").and_then(|v| v.as_str()), Some("Edit"));
        let b = first_ext(&events[1]);
        assert_eq!(b.kind, "file.edited");
        assert_eq!(
            b.payload.get("path").and_then(|v| v.as_str()),
            Some("/repo/src/lib.rs")
        );
    }

    #[test]
    fn post_tool_use_read_emits_tool_use_only() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Read".into());
        p.tool_input = Some(serde_json::json!({ "file_path": "/repo/src/lib.rs" }));
        let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
        assert_eq!(events.len(), 1);
        assert_eq!(first_ext(&events[0]).kind, "tool.use");
    }

    #[test]
    fn post_tool_use_write_emits_file_edited() {
        for tool in ["Write", "NotebookEdit", "MultiEdit"] {
            let mut p = base_payload("PostToolUse");
            p.tool_name = Some(tool.to_string());
            p.tool_input = Some(serde_json::json!({ "file_path": "/x" }));
            let events = payload_to_events(&p, &id(), HookEvent::PostToolUse);
            assert_eq!(events.len(), 2, "tool={tool}");
            assert_eq!(first_ext(&events[1]).kind, "file.edited");
        }
    }

    #[test]
    fn stop_emits_task_stopped() {
        let p = base_payload("Stop");
        let events = payload_to_events(&p, &id(), HookEvent::Stop);
        assert_eq!(events.len(), 1);
        assert_eq!(first_ext(&events[0]).kind, "task.stopped");
    }

    #[test]
    fn session_end_emits_session_ended() {
        let p = base_payload("SessionEnd");
        let events = payload_to_events(&p, &id(), HookEvent::SessionEnd);
        assert_eq!(events.len(), 1);
        assert_eq!(first_ext(&events[0]).kind, "session.ended");
    }

    #[test]
    fn permission_request_emits_asked_with_tool_name() {
        let mut p = base_payload("PermissionRequest");
        p.tool_name = Some("Bash".into());
        p.tool_input = Some(serde_json::json!({ "command": "ls" }));
        let events = payload_to_events(&p, &id(), HookEvent::PermissionRequest);
        assert_eq!(events.len(), 1);
        let e = first_ext(&events[0]);
        assert_eq!(e.kind, "permission.asked");
        assert_eq!(e.payload.get("tool").and_then(|v| v.as_str()), Some("Bash"));
    }

    #[test]
    fn permission_request_missing_tool_name_falls_back_to_unknown() {
        let p = base_payload("PermissionRequest");
        let events = payload_to_events(&p, &id(), HookEvent::PermissionRequest);
        let e = first_ext(&events[0]);
        assert_eq!(
            e.payload.get("tool").and_then(|v| v.as_str()),
            Some("unknown")
        );
    }

    #[test]
    fn notification_emits_message_system() {
        let mut p = base_payload("Notification");
        p.extra
            .insert("message".into(), serde_json::json!("hello world"));
        let events = payload_to_events(&p, &id(), HookEvent::Notification);
        let e = first_ext(&events[0]);
        assert_eq!(e.kind, "message.system");
        assert_eq!(
            e.payload.get("summary").and_then(|v| v.as_str()),
            Some("hello world")
        );
    }

    #[test]
    fn task_completed_emits_task_done() {
        let mut p = base_payload("TaskCompleted");
        p.extra.insert("task_id".into(), serde_json::json!("T-123"));
        p.extra
            .insert("description".into(), serde_json::json!("refactor foo"));
        let events = payload_to_events(&p, &id(), HookEvent::TaskCompleted);
        let e = first_ext(&events[0]);
        assert_eq!(e.kind, "task.done");
        assert_eq!(
            e.payload.get("task_id").and_then(|v| v.as_str()),
            Some("T-123")
        );
        assert_eq!(
            e.payload.get("label").and_then(|v| v.as_str()),
            Some("refactor foo")
        );
    }

    #[test]
    fn summary_truncation_caps_at_80_chars_on_char_boundary() {
        let big = "é".repeat(200);
        let summary = truncate_summary(&big);
        assert_eq!(summary.chars().count(), SUMMARY_MAX_CHARS);
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
        let p: HookPayload = serde_json::from_value(raw).expect("parse");
        assert!(p.extra.contains_key("permission_mode"));
        assert!(p.extra.contains_key("transcript_path"));
        assert!(p.extra.contains_key("future_field"));
    }
}
