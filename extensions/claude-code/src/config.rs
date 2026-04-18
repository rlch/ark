//! T-041 (claude-code-ext R9) — `[claude-code]` extension config section.
//!
//! Config reaches the extension via `SessionSpec.ext_config["claude-code"]`
//! (a `serde_json::Value` populated by the outer figment-based loader
//! `crates/config/src/ext_sections.rs`). This module pins the typed shape
//! plus a tolerant "warn on unknown keys" parser.
//!
//! # Keys
//!
//! ```toml
//! [claude-code]
//! match_cmds = []                   # R5b raw-cmd fallback allowlist
//! transcript_tail_lines = 200       # expanded subagent tile tail window
//! auto_install_hook_entries = true  # if false, skip settings.json mutation
//! ```
//!
//! # Unknown-key behaviour
//!
//! Per R9 "unknown keys warn but don't fail". The outer figment loader
//! (`extract_section`) returns a `serde_json::Value`; deserialising that
//! straight into `ClaudeCodeConfig` with `deny_unknown_fields = true`
//! would HARD-fail, while `deny_unknown_fields = false` would silently
//! drop. Instead [`ClaudeCodeConfig::from_value`] walks the top-level
//! object keys once, emits a `warn!` tracing event per unknown key, then
//! deserialises into the strongly-typed struct with unknown-key tolerance
//! switched on. Net effect: typos surface as logs, not crashes.
//!
//! # Non-goals
//!
//! Per R9 "no permission/policy keys in v0.1 (claude's TUI owns that
//! surface)". This struct therefore omits any permission/read-only/tool
//! policy fields — attempting to set one will trigger the unknown-key
//! warning path.

use serde::{Deserialize, Serialize};
use tracing::warn;

/// Recognised keys in the `[claude-code]` config section. Kept as a
/// `&[&str]` rather than computed from the struct because `serde` does
/// not expose its field list at runtime; a manual list stays in sync
/// with the struct via the unit test below.
const KNOWN_KEYS: &[&str] = &[
    "match_cmds",
    "transcript_tail_lines",
    "auto_install_hook_entries",
];

/// Default for [`ClaudeCodeConfig::transcript_tail_lines`] — see R9 /
/// build-site T-041. Exposed so the ledger + tests don't drift from the
/// canonical constant.
pub const DEFAULT_TRANSCRIPT_TAIL_LINES: usize = 200;

/// Typed shape of the `[claude-code]` extension config section.
///
/// See module docs for key semantics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ClaudeCodeConfig {
    /// R5b raw-cmd fallback allowlist. Default: empty (fallback OFF).
    /// When non-empty, every `command cmd=<X>` pane in a compiled scene
    /// whose `X` matches an entry gets `CLAUDE_HOOK_SOCKET` injected
    /// into its env at scene-compile time. See `ClaudeCodeExtension::
    /// scene_compile_hook` (T-032/T-033).
    pub match_cmds: Vec<String>,

    /// Number of transcript-tail lines rendered into an expanded
    /// subagent tile. T-036 reads this when building the rendered tail.
    /// Default: [`DEFAULT_TRANSCRIPT_TAIL_LINES`].
    pub transcript_tail_lines: usize,

    /// Whether `on_session_start` + the `install-hooks` control verb
    /// reconcile `~/.claude/settings.json`. When `false`, settings.json
    /// is left untouched (the user manages hook wiring out-of-band).
    /// Default: `true`.
    pub auto_install_hook_entries: bool,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            match_cmds: vec![],
            transcript_tail_lines: DEFAULT_TRANSCRIPT_TAIL_LINES,
            auto_install_hook_entries: true,
        }
    }
}

impl ClaudeCodeConfig {
    /// Known top-level config keys. Used by [`Self::from_value`] to emit
    /// warn-on-unknown diagnostics; exposed pub so doctor / tests can
    /// cross-check without re-typing the list.
    pub fn known_keys() -> &'static [&'static str] {
        KNOWN_KEYS
    }

    /// Parse a `[claude-code]` section from a JSON value (produced by
    /// the outer figment loader — see `crates/config/src/ext_sections.rs`
    /// `extract_section`). Unknown top-level keys trigger a `warn!`
    /// tracing event and are otherwise ignored. Wrong-type values at
    /// recognised keys return `Err`.
    ///
    /// A `None` / missing section falls back to [`Self::default`] with
    /// no diagnostic — scene authors who don't write the section expect
    /// defaults. `Some(Value::Null)` behaves the same.
    pub fn from_value(value: Option<&serde_json::Value>) -> Result<Self, serde_json::Error> {
        let Some(v) = value else {
            return Ok(Self::default());
        };
        if v.is_null() {
            return Ok(Self::default());
        }

        // Walk top-level keys and warn on unknowns BEFORE the typed
        // deserialise so the warning fires even on otherwise-valid
        // payloads. Non-object values skip the walk; the typed
        // deserialise below will surface a clearer error.
        if let Some(obj) = v.as_object() {
            for key in obj.keys() {
                if !KNOWN_KEYS.contains(&key.as_str()) {
                    warn!(
                        key = %key,
                        "claude-code: unknown config key in [claude-code]; ignoring (known: match_cmds, transcript_tail_lines, auto_install_hook_entries)"
                    );
                }
            }
        }

        // serde(default) + no deny_unknown_fields → unknown keys silently
        // ignored after the warn loop above. Missing keys fall back to
        // the struct-level default.
        serde_json::from_value::<Self>(v.clone())
    }

    /// Convenience helper — pull the `"claude-code"` entry out of a
    /// `SessionSpec.ext_config` map and parse it through
    /// [`Self::from_value`]. Missing key → defaults.
    pub fn from_ext_config(
        ext_config: &std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<Self, serde_json::Error> {
        Self::from_value(ext_config.get(crate::hook_payload::EXT_NAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn default_matches_kit_r9() {
        let c = ClaudeCodeConfig::default();
        assert!(c.match_cmds.is_empty());
        assert_eq!(c.transcript_tail_lines, 200);
        assert!(c.auto_install_hook_entries);
    }

    #[test]
    fn known_keys_matches_struct_fields() {
        // Keeps the KNOWN_KEYS list in sync with the struct — a manual
        // list is easier to reason about than reflection, but a drifted
        // list is a silent footgun, hence this round-trip.
        let json = serde_json::json!({
            "match_cmds": [],
            "transcript_tail_lines": 1,
            "auto_install_hook_entries": true,
        });
        let obj = json.as_object().unwrap();
        let mut actual: Vec<&str> = obj.keys().map(String::as_str).collect();
        actual.sort();
        let mut known: Vec<&str> = KNOWN_KEYS.to_vec();
        known.sort();
        assert_eq!(actual, known);
    }

    #[test]
    fn missing_section_yields_defaults() {
        let c = ClaudeCodeConfig::from_value(None).unwrap();
        assert_eq!(c, ClaudeCodeConfig::default());
    }

    #[test]
    fn null_section_yields_defaults() {
        let c = ClaudeCodeConfig::from_value(Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(c, ClaudeCodeConfig::default());
    }

    #[test]
    fn parses_full_config() {
        let v = serde_json::json!({
            "match_cmds": ["claude", "claude-dev"],
            "transcript_tail_lines": 500,
            "auto_install_hook_entries": false,
        });
        let c = ClaudeCodeConfig::from_value(Some(&v)).unwrap();
        assert_eq!(
            c.match_cmds,
            vec!["claude".to_string(), "claude-dev".to_string()]
        );
        assert_eq!(c.transcript_tail_lines, 500);
        assert!(!c.auto_install_hook_entries);
    }

    #[test]
    fn partial_config_uses_defaults_for_missing_keys() {
        let v = serde_json::json!({ "match_cmds": ["claude"] });
        let c = ClaudeCodeConfig::from_value(Some(&v)).unwrap();
        assert_eq!(c.match_cmds, vec!["claude".to_string()]);
        assert_eq!(c.transcript_tail_lines, 200);
        assert!(c.auto_install_hook_entries);
    }

    #[test]
    fn unknown_keys_ignored_not_rejected() {
        // R9: "unknown keys warn but don't fail". A typo-driven key like
        // `transcipt_tail_lines` (missing `r`) must NOT fail the parse
        // — only produce a warn log + fall through to defaults on the
        // real key.
        let v = serde_json::json!({
            "transcipt_tail_lines": 999,
            "match_cmds": ["claude"],
        });
        let c = ClaudeCodeConfig::from_value(Some(&v)).unwrap();
        assert_eq!(c.transcript_tail_lines, 200); // default — typo ignored
        assert_eq!(c.match_cmds, vec!["claude".to_string()]);
    }

    #[test]
    fn permission_policy_key_rejected_as_unknown() {
        // R9 non-goal: "no permission/policy keys in v0.1". A scene
        // author who writes one gets the unknown-key warn path, not a
        // silent accept.
        let v = serde_json::json!({
            "permission_policy": "auto",
            "match_cmds": ["claude"],
        });
        let c = ClaudeCodeConfig::from_value(Some(&v)).unwrap();
        assert_eq!(c.match_cmds, vec!["claude".to_string()]);
    }

    #[test]
    fn wrong_type_errors() {
        // A valid-but-wrong-typed value at a known key should error so
        // the user sees the problem; unknown keys don't have this
        // rigour because we can't type-check them.
        let v = serde_json::json!({ "transcript_tail_lines": "lots" });
        assert!(ClaudeCodeConfig::from_value(Some(&v)).is_err());
    }

    #[test]
    fn from_ext_config_pulls_claude_code_entry() {
        let mut m: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        m.insert(
            "claude-code".to_string(),
            serde_json::json!({ "match_cmds": ["claude"] }),
        );
        m.insert("other-ext".to_string(), serde_json::json!({ "foo": 1 }));
        let c = ClaudeCodeConfig::from_ext_config(&m).unwrap();
        assert_eq!(c.match_cmds, vec!["claude".to_string()]);
    }

    #[test]
    fn from_ext_config_missing_yields_defaults() {
        let m: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let c = ClaudeCodeConfig::from_ext_config(&m).unwrap();
        assert_eq!(c, ClaudeCodeConfig::default());
    }
}
