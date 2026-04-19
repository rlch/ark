//! T-044 + T-045 (claude-code-ext R11) — `list_columns` contributions.
//!
//! The extension contributes three `ark list` columns derived from the
//! active session's transcript tail:
//!
//! * `cc model` — most recent `message.model` on an assistant line.
//! * `cc tokens` — rolling sum of `message.usage.input_tokens +
//!   message.usage.output_tokens`.
//! * `cc cost` — most recent `message.cost_usd` (when present), rendered
//!   as `$<n.nn>`.
//!
//! # State shape
//!
//! The live values live on [`CcListColumnState`], a JSON-serialisable
//! struct that round-trips through `SessionStatus.ext_state["claude-code"]`
//! (see R11 "Column state is per-session, persisted under
//! `SessionStatus.ext_state["claude-code"]`"). An extension-side cache
//! under [`ClaudeCodeExtension`] holds the same struct for the CURRENT
//! session; the persistence path is supervisor-side (T-044 wiring
//! deliberately defers to an on-ext cache + opt-in snapshot dump, since
//! the supervisor crate is out-of-scope for this tier per the task
//! brief).
//!
//! # Response shape
//!
//! The `ListColumnsResponse.columns` field is [`ark_ext_proto::OpaqueJson`]
//! (a JSON string). This module serialises a
//! `{ "columns": [<ColumnContribution>, ...] }` envelope so the future
//! supervisor-side dispatcher can decode without a per-extension dialect.
//!
//! # Zero-event regression (T-045)
//!
//! With no transcript lines observed:
//! * `cc model` → `""` (empty string)
//! * `cc tokens` → `"0"`
//! * `cc cost` → `""`

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Manifest name of the claude-code extension. Canonical key under
/// `SessionStatus.ext_state` + filename basename of the v0.2-backlog #5
/// ext-state sentinel (`$STATE/sessions/<sid>/ext-state/claude-code.json`).
pub const EXT_STATE_FILE_STEM: &str = "claude-code";

/// Stable column name for the model field. Pinned as a `&'static str` so
/// the list-command side can match without string-allocating.
pub const COLUMN_MODEL: &str = "cc model";
/// Stable column name for the rolling-sum token field.
pub const COLUMN_TOKENS: &str = "cc tokens";
/// Stable column name for the cost field.
pub const COLUMN_COST: &str = "cc cost";

/// Per-session rolling state the extension maintains for its three
/// contributed `ark list` columns. Round-trips through
/// `SessionStatus.ext_state["claude-code"]` as a JSON object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CcListColumnState {
    /// Most-recent observed `message.model` on an assistant line.
    /// Empty string when no claude event has been seen.
    #[serde(default)]
    pub model: String,
    /// Rolling sum of `input_tokens + output_tokens` across every
    /// observed assistant message.
    #[serde(default)]
    pub tokens: u64,
    /// Most-recent observed `message.cost_usd` (USD float). `None` when
    /// no cost field has been seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// One contributed column: stable name + rendered value for the current
/// session. Rendering lives ext-side (v0.1 convention); the host
/// `ark list` layer concatenates per-ext contributions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnContribution {
    /// Column header name (e.g. `"cc model"`).
    pub name: String,
    /// Rendered cell value. Empty string on "no data yet".
    pub value: String,
}

/// Top-level envelope carried in
/// [`ark_ext_proto::ListColumnsResponse::columns`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnsEnvelope {
    /// All three R11 contributions, in declaration order.
    pub columns: Vec<ColumnContribution>,
}

impl CcListColumnState {
    /// Zero-state factory (identical to `Default::default` but named to
    /// match R11 regression prose).
    pub fn zero() -> Self {
        Self::default()
    }

    /// Render the three contributed columns per R11 acceptance.
    pub fn to_columns(&self) -> Vec<ColumnContribution> {
        vec![
            ColumnContribution {
                name: COLUMN_MODEL.into(),
                value: self.model.clone(),
            },
            ColumnContribution {
                name: COLUMN_TOKENS.into(),
                value: self.tokens.to_string(),
            },
            ColumnContribution {
                name: COLUMN_COST.into(),
                value: match self.cost_usd {
                    Some(v) => format!("${:.2}", v),
                    None => String::new(),
                },
            },
        ]
    }

    /// Fold one transcript JSONL line into the state. Tolerant of:
    /// * Missing lines (non-object, non-message) — no-op.
    /// * Non-assistant messages — no-op.
    /// * Missing `model` / `usage` / `cost_usd` — skip that field.
    ///
    /// Claude Code's transcript JSONL shape (per R8 / observed corpus):
    ///
    /// ```json
    /// {"type":"message","role":"assistant","model":"claude-…",
    ///  "content":[...],"usage":{"input_tokens":N,"output_tokens":M},
    ///  "cost_usd":0.0123}
    /// ```
    ///
    /// Assistant-only filtering keeps user/tool-result lines from
    /// inflating the token counter (`usage` only appears on assistant
    /// turns in Claude's schema, but belt-and-braces).
    pub fn fold_line(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        // Only assistant messages carry the three fields. The outer
        // shape is `{"type":"message","role":"assistant", ...}` in the
        // observed corpus; some dumps use nested `{"message":{...}}`
        // (Anthropic SDK transcript form). Support both.
        let msg = v.get("message").filter(|m| m.is_object()).unwrap_or(&v);
        let kind = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if kind != "message" || role != "assistant" {
            return;
        }
        if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
            self.model = m.to_string();
        }
        if let Some(usage) = msg.get("usage") {
            let inp = usage
                .get("input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            let out = usage
                .get("output_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            self.tokens = self.tokens.saturating_add(inp).saturating_add(out);
        }
        if let Some(c) = msg.get("cost_usd").and_then(|c| c.as_f64()) {
            self.cost_usd = Some(c);
        }
    }

    /// Fold every line in a transcript JSONL blob. Used by both T-044
    /// (live poll) and T-045 (zero-line regression — returns unchanged
    /// state).
    pub fn fold_blob(&mut self, blob: &str) {
        for line in blob.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.fold_line(trimmed);
        }
    }

    /// Build a state from a transcript file on disk, reading the whole
    /// file. Missing file → zero state. Intended for T-044 populate-on-
    /// demand when a CLI `ark list` invocation requests columns for a
    /// session without a live extension cache to consult.
    pub fn from_transcript_path(path: &std::path::Path) -> Self {
        let mut state = Self::default();
        if let Ok(blob) = std::fs::read_to_string(path) {
            state.fold_blob(&blob);
        }
        state
    }

    /// Read a persisted [`CcListColumnState`] from an ext-state sentinel
    /// path (v0.2-backlog #5). Missing / unreadable / malformed file →
    /// `None` so the caller can decide whether to fall back to a zero
    /// state or skip the row entirely.
    pub fn read_from_file(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice::<Self>(&bytes).ok()
    }

    /// Atomically write this state to an ext-state sentinel file (v0.2-
    /// backlog #5). Creates the parent directory if missing; writes to
    /// a tmp file in the same directory then renames into place so
    /// concurrent `ark list` reads never observe a partial write.
    ///
    /// Errors collapse to `std::io::Error` — caller decides whether to
    /// log + continue (the `list_columns` RPC path does) or surface
    /// upstream (the doctor fix path does).
    pub fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut tmp = path.to_path_buf();
        let mut name = tmp
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("ext-state.json"));
        name.push(".tmp");
        tmp.set_file_name(name);
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- T-045: zero-event regression -----------------------------------------

    #[test]
    fn zero_state_columns_match_t045_spec() {
        let cols = CcListColumnState::zero().to_columns();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].name, "cc model");
        assert_eq!(cols[0].value, "");
        assert_eq!(cols[1].name, "cc tokens");
        assert_eq!(cols[1].value, "0");
        assert_eq!(cols[2].name, "cc cost");
        assert_eq!(cols[2].value, "");
    }

    #[test]
    fn fold_empty_blob_leaves_zero_state() {
        let mut s = CcListColumnState::default();
        s.fold_blob("");
        assert_eq!(s, CcListColumnState::default());
    }

    #[test]
    fn fold_non_assistant_lines_ignored() {
        let mut s = CcListColumnState::default();
        s.fold_line(r#"{"type":"message","role":"user","model":"x"}"#);
        s.fold_line(r#"{"type":"tool_result","role":"assistant"}"#);
        s.fold_line("not json at all");
        assert_eq!(s, CcListColumnState::default());
    }

    // -- T-044: live fold ----------------------------------------------------

    #[test]
    fn fold_single_assistant_line_populates_all_three() {
        let mut s = CcListColumnState::default();
        s.fold_line(
            r#"{"type":"message","role":"assistant","model":"claude-4-7-opus","usage":{"input_tokens":100,"output_tokens":50},"cost_usd":0.125}"#,
        );
        assert_eq!(s.model, "claude-4-7-opus");
        assert_eq!(s.tokens, 150);
        assert_eq!(s.cost_usd, Some(0.125));
    }

    #[test]
    fn fold_multiple_lines_accumulates_tokens_and_takes_latest_model_cost() {
        let mut s = CcListColumnState::default();
        s.fold_line(
            r#"{"type":"message","role":"assistant","model":"claude-a","usage":{"input_tokens":10,"output_tokens":5},"cost_usd":0.01}"#,
        );
        s.fold_line(
            r#"{"type":"message","role":"assistant","model":"claude-b","usage":{"input_tokens":7,"output_tokens":3},"cost_usd":0.02}"#,
        );
        assert_eq!(s.model, "claude-b"); // latest
        assert_eq!(s.tokens, 25); // 10+5+7+3
        assert_eq!(s.cost_usd, Some(0.02)); // latest
    }

    #[test]
    fn fold_nested_message_shape_also_handled() {
        // Some Claude Code dumps nest: `{"message":{...},"type":"assistant"}`
        // where the inner message carries the fields.
        let mut s = CcListColumnState::default();
        s.fold_line(
            r#"{"message":{"type":"message","role":"assistant","model":"claude-x","usage":{"input_tokens":2,"output_tokens":3}}}"#,
        );
        assert_eq!(s.model, "claude-x");
        assert_eq!(s.tokens, 5);
    }

    #[test]
    fn fold_missing_cost_keeps_prior_none() {
        let mut s = CcListColumnState::default();
        s.fold_line(
            r#"{"type":"message","role":"assistant","model":"x","usage":{"input_tokens":1,"output_tokens":1}}"#,
        );
        assert_eq!(s.cost_usd, None);
        assert_eq!(s.to_columns()[2].value, "");
    }

    #[test]
    fn cost_rendered_as_dollar_format() {
        let s = CcListColumnState {
            model: "m".into(),
            tokens: 1,
            cost_usd: Some(1.5),
        };
        let cols = s.to_columns();
        assert_eq!(cols[2].value, "$1.50");
    }

    #[test]
    fn ext_state_round_trips_as_json() {
        let s = CcListColumnState {
            model: "claude-4-7".into(),
            tokens: 123,
            cost_usd: Some(0.07),
        };
        let j = serde_json::to_value(&s).unwrap();
        let back: CcListColumnState = serde_json::from_value(j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn from_transcript_path_missing_file_yields_zero() {
        let tmp = tempfile::TempDir::new().unwrap();
        let missing = tmp.path().join("nope.jsonl");
        let s = CcListColumnState::from_transcript_path(&missing);
        assert_eq!(s, CcListColumnState::default());
    }

    // -- v0.2-backlog #5: ext-state file round-trip --------------------------

    #[test]
    fn write_then_read_file_round_trips_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("ext-state").join("claude-code.json");
        let state = CcListColumnState {
            model: "claude-4-7-opus".into(),
            tokens: 9999,
            cost_usd: Some(1.23),
        };
        state.write_to_file(&path).expect("write");
        assert!(path.exists(), "file must exist after write");
        let back = CcListColumnState::read_from_file(&path).expect("read");
        assert_eq!(back, state);
    }

    #[test]
    fn read_missing_file_yields_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        assert_eq!(CcListColumnState::read_from_file(&path), None);
    }

    #[test]
    fn read_malformed_file_yields_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(CcListColumnState::read_from_file(&path), None);
    }

    #[test]
    fn write_creates_parent_dir_when_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Nested parent that does not yet exist.
        let path = tmp
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("claude-code.json");
        let state = CcListColumnState::zero();
        state.write_to_file(&path).expect("write creates parents");
        assert!(path.exists());
    }

    #[test]
    fn write_is_atomic_no_tmp_file_left_behind() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("claude-code.json");
        let state = CcListColumnState::default();
        state.write_to_file(&path).expect("write");
        // No .tmp sibling should remain.
        let tmp_path = tmp.path().join("claude-code.json.tmp");
        assert!(!tmp_path.exists(), "tmp file should be renamed into place");
    }

    #[test]
    fn from_transcript_path_reads_lines_from_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"message\",\"role\":\"assistant\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"hi\"}\n",
        )
        .unwrap();
        let s = CcListColumnState::from_transcript_path(&path);
        assert_eq!(s.model, "m");
        assert_eq!(s.tokens, 3);
    }
}
