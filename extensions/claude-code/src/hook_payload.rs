//! Typed Claude Code hook payload (T-005 salvage).
//!
//! Salvaged from the pre-2026-04-18 `crates/hook/src/payload.rs`;
//! adapted for cavekit-claude-code R2 + R3:
//!
//! - **Wire shape preserved** — [`HookPayload`] mirrors Claude Code's
//!   hook JSON verbatim (required fields typed, all unknown keys
//!   captured via `#[serde(flatten)]` in [`HookPayload::extra`]).
//!   Serde derives + field names match the legacy crate so the
//!   fixture-JSON tests in `mock-claude` (T-017, T-018) and any
//!   downstream bridge consumer continue to round-trip.
//! - **R2 wire shape wraps this** — `cc-hook` POSTs a single NDJSON
//!   line per invocation:
//!   ```json
//!   { "kind": "<HookEventName>",
//!     "session_id": "<sid>",
//!     "payload": { ... hook payload verbatim ... },
//!     "emitted_at": "<rfc3339>",
//!     "bridge_version": "<semver>" // first POST only; T-010
//!   }
//!   ```
//!   The `payload` field is this struct serialised to JSON. See
//!   [`NdjsonLine`] for the envelope shape.
//! - **R3 translator** — [`payload_to_ext_event`] turns a
//!   parsed payload + a [`HookEvent`] into the
//!   `claude-code.<kind>` ExtEvent the core bus emits. The payload is
//!   carried verbatim under [`ExtEvent::payload`] (R3 "Each ExtEvent
//!   carries the verbatim hook payload").
//! - **Non-goal marker** — the legacy crate also baked in
//!   `PermissionRequest` allow-payload logic + `FILE_EDIT_TOOLS` + a
//!   per-tool `tool.use` / `file.edited` split. Per cavekit-claude-code
//!   §Non-goals that behaviour is **not** salvaged: v0.1 carries
//!   payloads verbatim and lets the scene author drive policy through
//!   Rhai reactions. The permission surface returns in v0.2-stretch
//!   via the MCP server (kit §Stretch).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use ark_types::ExtEvent;

use crate::hook_event::HookEvent;

/// Extension name emitted as the `ext` field of every translated event.
/// Pairs with [`HookEvent::ext_kind`] to form `<ext>.<kind>` — the flat
/// event name consumed by `on "claude-code.<kind>"` scene reactions.
pub const EXT_NAME: &str = "claude-code";

/// Typed Claude Code hook payload.
///
/// `extra` captures every field not enumerated above so future
/// Claude-side additions reach the translator without a crate rebuild
/// — R3 mandates verbatim payload carry-through.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookPayload {
    /// Claude Code's session id (distinct from ark's `SessionId`).
    pub session_id: String,
    /// Working directory Claude was running in when the hook fired.
    pub cwd: PathBuf,
    /// Mirrors Claude's `hook_event_name` (PascalCase, e.g. `PostToolUse`).
    pub hook_event_name: String,
    /// Name of the tool the hook fired for (present for PreToolUse,
    /// PostToolUse, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Raw tool input — shape is tool-dependent so we keep it loose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    /// Forward-compat bucket for unknown top-level keys (agent_id,
    /// agent_type, agent_transcript_path, last_assistant_message,
    /// tool_response, and whatever future hook shapes add).
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// R2 NDJSON envelope posted by `cc-hook` on every hook invocation.
///
/// The ark-side socket reader (T-011) parses this, maps `kind` to
/// [`HookEvent`], and calls [`payload_to_ext_event`] with the inner
/// [`HookPayload`] to produce the broadcast [`ExtEvent`].
///
/// `bridge_version` rides along on the first POST per session only
/// (T-010); subsequent POSTs omit the field and the reader leaves the
/// cached value in place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NdjsonLine {
    /// Claude Code hook event name (PascalCase).
    pub kind: String,
    /// ark session id the cc-hook invocation is attributed to.
    pub session_id: String,
    /// Verbatim Claude Code hook payload.
    pub payload: HookPayload,
    /// RFC 3339 timestamp recorded by `cc-hook` at the moment of
    /// invocation. Flows through to the `ExtEvent` payload unchanged so
    /// scene reactions can read `event.payload.emitted_at`.
    pub emitted_at: String,
    /// Semver of the `cc-hook` binary that posted this line. Only
    /// present on the first POST per session — subsequent POSTs omit
    /// the field (R4 handshake).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_version: Option<String>,
}

/// Translate a parsed hook payload into the matching `claude-code.*`
/// ExtEvent (R3).
///
/// The payload is carried verbatim under [`ExtEvent::payload`] — no
/// per-kind restructuring, no truncation, no synthetic side-events.
/// Scene reactions drive everything downstream.
pub fn payload_to_ext_event(payload: &HookPayload, event: HookEvent) -> ExtEvent {
    let value = serde_json::to_value(payload).unwrap_or_else(|_| serde_json::json!({}));
    ExtEvent {
        ext: EXT_NAME.to_string(),
        kind: event.ext_kind().to_string(),
        payload: value,
    }
}

/// Convenience: build the full `<ext>.<kind>` flat event name Rhai
/// `on "<name>"` reactions match against.
pub fn flat_event_name(event: HookEvent) -> String {
    format!("{}.{}", EXT_NAME, event.ext_kind())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn post_tool_use_translates_to_claude_code_post_tool_use() {
        let mut p = base_payload("PostToolUse");
        p.tool_name = Some("Edit".into());
        p.tool_input = Some(serde_json::json!({
            "file_path": "/repo/src/lib.rs",
        }));
        let ev = payload_to_ext_event(&p, HookEvent::PostToolUse);
        assert_eq!(ev.ext, "claude-code");
        assert_eq!(ev.kind, "post-tool-use");
        assert_eq!(
            ev.payload.get("tool_name").and_then(|v| v.as_str()),
            Some("Edit")
        );
    }

    #[test]
    fn subagent_stop_verbatim_payload_preserves_extra_fields() {
        // R3 envelope test shape: SubagentStop payload carries
        // agent_id / agent_type / last_assistant_message /
        // agent_transcript_path through `extra`, all readable from the
        // resulting ExtEvent payload.
        let raw = serde_json::json!({
            "session_id": "s1",
            "cwd": "/tmp",
            "hook_event_name": "SubagentStop",
            "agent_id": "agent-123",
            "agent_type": "code-writer",
            "last_assistant_message": "all done",
            "agent_transcript_path": "/tmp/agent-123.jsonl",
        });
        let p: HookPayload = serde_json::from_value(raw).expect("parse");
        let ev = payload_to_ext_event(&p, HookEvent::SubagentStop);
        assert_eq!(ev.kind, "subagent.stop");
        for key in [
            "agent_id",
            "agent_type",
            "last_assistant_message",
            "agent_transcript_path",
        ] {
            assert!(
                ev.payload.get(key).is_some(),
                "payload missing {key}: {ev:?}"
            );
        }
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

    #[test]
    fn flat_event_name_matches_rhai_reaction_surface() {
        assert_eq!(
            flat_event_name(HookEvent::SubagentStop),
            "claude-code.subagent.stop"
        );
        assert_eq!(
            flat_event_name(HookEvent::SessionStart),
            "claude-code.session.start"
        );
    }

    #[test]
    fn ndjson_envelope_serde_round_trip() {
        let p = base_payload("SessionStart");
        let line = NdjsonLine {
            kind: "SessionStart".into(),
            session_id: "ark-sess".into(),
            payload: p,
            emitted_at: "2026-04-18T00:00:00Z".into(),
            bridge_version: Some("0.1.0".into()),
        };
        let s = serde_json::to_string(&line).expect("ser");
        let parsed: NdjsonLine = serde_json::from_str(&s).expect("de");
        assert_eq!(parsed, line);
    }

    #[test]
    fn ndjson_envelope_omits_bridge_version_when_none() {
        let p = base_payload("Stop");
        let line = NdjsonLine {
            kind: "Stop".into(),
            session_id: "ark-sess".into(),
            payload: p,
            emitted_at: "2026-04-18T00:00:00Z".into(),
            bridge_version: None,
        };
        let s = serde_json::to_string(&line).expect("ser");
        assert!(
            !s.contains("bridge_version"),
            "bridge_version should be skipped when None: {s}"
        );
    }
}
