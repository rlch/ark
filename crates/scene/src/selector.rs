//! Event selector parser + matcher (T-5.2).
//!
//! Scene authors write R4 selectors in three shapes:
//!
//! * `"<EventKind>"` — match every event of that kind.
//! * `"<EventKind> k1=\"v1\" k2=\"v2\""` — sugar for "match every event
//!   of that kind whose top-level fields k1/k2 (compared as strings)
//!   satisfy the glob patterns v1/v2".
//! * `"UserEvent:<namespaced-name>"` — match `UserEvent` specifically,
//!   narrowed to the named event in the secondary index.
//!
//! This module provides:
//!
//! * [`EventSelector`] — the parsed shape.
//! * [`parse_selector`] — string → `EventSelector`.
//! * [`EventSelector::matches`] — does a live `AgentEvent` satisfy this
//!   selector's field patterns?
//!
//! Field matching uses `globset` globs (reused from T-2.3) so authors can
//! write `to="review*"` or `to="*"` without reaching for CEL. For
//! structured (non-string) fields the comparison runs against the field's
//! JSON string rendering (per serde serialization of `AgentEvent`). Every
//! field referenced by a selector that isn't present on the event is a
//! non-match — we do NOT error on missing fields so broad selectors like
//! `"* to=\"review\""` quietly skip irrelevant variants.
//!
//! # Grammar
//!
//! ```text
//! selector := head ( WS field )*
//! head     := kind | "UserEvent:" IDENT
//! field    := IDENT "=" STRING
//! ```
//!
//! Unquoted values are rejected (KDL escapes values by default). The
//! `*` wildcard in head position (i.e. matching any kind) is NOT
//! supported at this tier — the registry's primary index keys on
//! EventKind; wildcards would require a third index slot. Deferred
//! until a concrete need surfaces.

use std::collections::BTreeMap;

use ark_types::event::AgentEvent;
use globset::{Glob, GlobMatcher};

use crate::reactions::EventKind;

// ---------------------------------------------------------------------------
// EventSelector
// ---------------------------------------------------------------------------

/// Parsed selector: a kind, an optional UserEvent name, and a map of
/// field→glob patterns.
///
/// Constructed by [`parse_selector`]; evaluated against live events via
/// [`EventSelector::matches`].
#[derive(Debug, Clone)]
pub struct EventSelector {
    /// Primary-index key — the event kind this selector targets.
    pub kind: EventKind,

    /// Secondary-index key when the selector was `UserEvent:<name>`.
    /// `None` for bare-kind selectors.
    pub user_event_name: Option<String>,

    /// Field patterns parsed from the selector tail (the `k="v"`
    /// trailing pairs). Key = field name flat-mapped from the event's
    /// serde JSON representation; value = compiled glob matcher. An
    /// empty map means "kind-only, match any".
    pub field_patterns: BTreeMap<String, GlobMatcher>,
}

/// Selector parse error. The scene compile pipeline wraps this into a
/// `SceneError::Grammar` (with a proper NamedSource) when it surfaces
/// to the user; this surface is deliberately narrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorParseError {
    /// The head token didn't match any known event kind (or the
    /// `UserEvent:` prefix form). Carries the offending token.
    UnknownKind(String),

    /// A field pattern couldn't be tokenised — missing `=`, empty key,
    /// unquoted value, etc.
    MalformedField(String),

    /// An otherwise-parseable field value failed `globset::Glob::new`
    /// (malformed glob syntax).
    BadGlob {
        /// The field whose pattern was bad.
        field: String,
        /// The glob source.
        pattern: String,
        /// The globset error message.
        message: String,
    },
}

impl std::fmt::Display for SelectorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectorParseError::UnknownKind(k) => {
                write!(
                    f,
                    "unknown event kind `{k}` in selector (expected a snake_case AgentEvent variant or `UserEvent:<name>`)"
                )
            }
            SelectorParseError::MalformedField(s) => {
                write!(f, "malformed field pattern `{s}` (expected `field=\"value\"`)")
            }
            SelectorParseError::BadGlob {
                field,
                pattern,
                message,
            } => {
                write!(
                    f,
                    "invalid glob pattern for field `{field}`: `{pattern}` ({message})"
                )
            }
        }
    }
}

impl std::error::Error for SelectorParseError {}

/// Parse a selector string into an [`EventSelector`].
///
/// See the module docs for the grammar.
pub fn parse_selector(input: &str) -> Result<EventSelector, SelectorParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(SelectorParseError::UnknownKind(String::new()));
    }

    // Tokenise: the head is the first whitespace-delimited token, the
    // remainder is zero-or-more `k="v"` pairs. KDL 2 strings use
    // double-quote pairs, and scene authors can embed quoted field
    // values verbatim (we don't strip extra escapes — globset patterns
    // have their own grammar).
    let (head, tail) = split_head(trimmed);

    // Resolve kind.
    let (kind, user_event_name) = if let Some(rest) = head.strip_prefix("UserEvent:") {
        if rest.is_empty() {
            return Err(SelectorParseError::UnknownKind(head.to_string()));
        }
        (EventKind::UserEvent, Some(rest.to_string()))
    } else if let Some(rest) = head.strip_prefix("user_event:") {
        if rest.is_empty() {
            return Err(SelectorParseError::UnknownKind(head.to_string()));
        }
        (EventKind::UserEvent, Some(rest.to_string()))
    } else {
        (
            EventKind::parse(head).ok_or_else(|| SelectorParseError::UnknownKind(head.to_string()))?,
            None,
        )
    };

    // Parse field patterns.
    let field_patterns = parse_field_patterns(tail)?;

    Ok(EventSelector {
        kind,
        user_event_name,
        field_patterns,
    })
}

/// Split a trimmed selector string into (head, tail). Head is the first
/// whitespace-delimited token; tail is the rest, with leading whitespace
/// stripped.
fn split_head(s: &str) -> (&str, &str) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        Some(idx) => (&s[..idx], s[idx..].trim_start()),
        None => (s, ""),
    }
}

/// Tokenise `k1="v1" k2="v2" …` into a glob map. Fails on malformed
/// pairs; skips empty tail (returns an empty map).
fn parse_field_patterns(tail: &str) -> Result<BTreeMap<String, GlobMatcher>, SelectorParseError> {
    let mut map = BTreeMap::new();
    let mut cursor = tail.trim_start();

    while !cursor.is_empty() {
        // Key — run of alphanum / dot / underscore / hyphen.
        let key_end = cursor
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'))
            .unwrap_or(cursor.len());
        if key_end == 0 {
            return Err(SelectorParseError::MalformedField(cursor.to_string()));
        }
        let key = &cursor[..key_end];
        cursor = &cursor[key_end..];

        // Expect `=`.
        if !cursor.starts_with('=') {
            return Err(SelectorParseError::MalformedField(format!(
                "{key}{cursor}"
            )));
        }
        cursor = &cursor[1..];

        // Expect opening `"`.
        if !cursor.starts_with('"') {
            return Err(SelectorParseError::MalformedField(format!(
                "{key}={cursor}"
            )));
        }
        cursor = &cursor[1..];

        // Scan until unescaped closing `"`.
        let mut value = String::new();
        let mut closed = false;
        let mut chars = cursor.char_indices();
        let mut consumed_upto = 0;
        while let Some((i, c)) = chars.next() {
            consumed_upto = i + c.len_utf8();
            if c == '\\' {
                if let Some((_, next_c)) = chars.next() {
                    consumed_upto += next_c.len_utf8();
                    value.push(next_c);
                }
                continue;
            }
            if c == '"' {
                closed = true;
                break;
            }
            value.push(c);
        }
        if !closed {
            return Err(SelectorParseError::MalformedField(format!(
                "{key}=\"{value}"
            )));
        }
        cursor = &cursor[consumed_upto..];
        cursor = cursor.trim_start();

        // Compile glob.
        let glob = Glob::new(&value).map_err(|e| SelectorParseError::BadGlob {
            field: key.to_string(),
            pattern: value.clone(),
            message: e.to_string(),
        })?;
        map.insert(key.to_string(), glob.compile_matcher());
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

impl EventSelector {
    /// Evaluate this selector against a live event.
    ///
    /// Returns `true` when:
    ///
    /// 1. The event's [`EventKind`] matches `self.kind`.
    /// 2. When `self.user_event_name` is `Some(name)`, the event is a
    ///    `UserEvent` with a matching `name` field.
    /// 3. Every field pattern in `self.field_patterns` finds the
    ///    referenced field on the event and the field's value
    ///    matches the compiled glob.
    ///
    /// The event is serde-serialised once per matcher call and the
    /// top-level fields walked. Serialisation failure (practically
    /// impossible for `AgentEvent`) drops to "no match" quietly.
    pub fn matches(&self, event: &AgentEvent) -> bool {
        // Kind gate.
        if EventKind::of(event) != self.kind {
            return false;
        }

        // UserEvent name gate.
        if let Some(expected_name) = &self.user_event_name {
            match event {
                AgentEvent::UserEvent { name, .. } => {
                    if name != expected_name {
                        return false;
                    }
                }
                _ => return false,
            }
        }

        // Field-pattern gate. Empty → skip.
        if self.field_patterns.is_empty() {
            return true;
        }

        let json = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let obj = match json.as_object() {
            Some(m) => m,
            None => return false,
        };

        for (key, matcher) in &self.field_patterns {
            let value = match obj.get(key) {
                Some(v) => v,
                None => return false,
            };
            let haystack = value_as_haystack(value);
            if !matcher.is_match(haystack.as_str()) {
                return false;
            }
        }
        true
    }
}

/// Flatten a JSON value into a string the glob matcher can operate on.
///
/// Strings stay as-is (no surrounding quotes). Booleans + numbers use
/// their `Display` rendering. Null renders as the empty string. Nested
/// objects + arrays use their compact JSON rendering so selectors like
/// `tab_handle="*builder*"` still work when the matched field is a
/// struct.
fn value_as_haystack(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::event::{AgentEvent, LogLevel, MessageRole, Severity, TabHandle, TabRole};
    use ark_types::id::AgentId;
    use std::path::PathBuf;

    fn id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    // -- parse happy paths ----------------------------------------------

    #[test]
    fn parse_bare_kind() {
        let sel = parse_selector("Started").unwrap();
        assert_eq!(sel.kind, EventKind::Started);
        assert!(sel.user_event_name.is_none());
        assert!(sel.field_patterns.is_empty());
    }

    #[test]
    fn parse_snake_case_kind() {
        let sel = parse_selector("phase_transition").unwrap();
        assert_eq!(sel.kind, EventKind::PhaseTransition);
    }

    #[test]
    fn parse_user_event_with_name() {
        let sel = parse_selector("UserEvent:user.hello").unwrap();
        assert_eq!(sel.kind, EventKind::UserEvent);
        assert_eq!(sel.user_event_name, Some("user.hello".into()));
    }

    #[test]
    fn parse_kind_with_field_patterns() {
        let sel = parse_selector(r#"PhaseTransition to="review""#).unwrap();
        assert_eq!(sel.kind, EventKind::PhaseTransition);
        assert_eq!(sel.field_patterns.len(), 1);
        assert!(sel.field_patterns.contains_key("to"));
    }

    #[test]
    fn parse_kind_with_multiple_fields() {
        let sel = parse_selector(r#"Message role="assistant" summary="hi*""#).unwrap();
        assert_eq!(sel.field_patterns.len(), 2);
    }

    #[test]
    fn parse_glob_in_value() {
        let sel = parse_selector(r#"Log line="error: *""#).unwrap();
        assert_eq!(sel.field_patterns.len(), 1);
    }

    // -- parse error paths ----------------------------------------------

    #[test]
    fn parse_unknown_kind_errors() {
        let err = parse_selector("Bogus").expect_err("unknown kind");
        assert!(matches!(err, SelectorParseError::UnknownKind(_)));
    }

    #[test]
    fn parse_empty_string_errors() {
        let err = parse_selector("").expect_err("empty");
        assert!(matches!(err, SelectorParseError::UnknownKind(_)));
    }

    #[test]
    fn parse_empty_user_event_errors() {
        let err = parse_selector("UserEvent:").expect_err("empty name");
        assert!(matches!(err, SelectorParseError::UnknownKind(_)));
    }

    #[test]
    fn parse_malformed_field_no_equals() {
        let err = parse_selector(r#"Progress done"#).expect_err("no =");
        assert!(matches!(err, SelectorParseError::MalformedField(_)));
    }

    #[test]
    fn parse_malformed_field_unquoted_value() {
        let err = parse_selector(r#"Progress done=5"#).expect_err("unquoted");
        assert!(matches!(err, SelectorParseError::MalformedField(_)));
    }

    #[test]
    fn parse_malformed_field_unclosed_quote() {
        let err = parse_selector(r#"Progress done="5"#).expect_err("unclosed");
        assert!(matches!(err, SelectorParseError::MalformedField(_)));
    }

    #[test]
    fn parse_bad_glob_surfaces() {
        let err = parse_selector(r#"Log line="[unclosed""#).expect_err("bad glob");
        assert!(matches!(err, SelectorParseError::BadGlob { .. }));
    }

    // -- matcher: per AgentEvent variant --------------------------------

    #[test]
    fn match_started() {
        let sel = parse_selector("Started").unwrap();
        let ev = AgentEvent::Log {
            id: id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        assert!(!sel.matches(&ev), "Log is not Started");
    }

    #[test]
    fn match_phase_transition_field() {
        let sel = parse_selector(r#"PhaseTransition to="review""#).unwrap();
        let matching = AgentEvent::PhaseTransition {
            id: id(),
            from: None,
            to: "review".into(),
        };
        let non_matching = AgentEvent::PhaseTransition {
            id: id(),
            from: None,
            to: "running".into(),
        };
        assert!(sel.matches(&matching));
        assert!(!sel.matches(&non_matching));
    }

    #[test]
    fn match_phase_transition_wildcard_glob() {
        let sel = parse_selector(r#"PhaseTransition to="review*""#).unwrap();
        let matching = AgentEvent::PhaseTransition {
            id: id(),
            from: None,
            to: "reviewing".into(),
        };
        assert!(sel.matches(&matching));
    }

    #[test]
    fn match_progress_numeric_field_as_string() {
        let sel = parse_selector(r#"Progress done="3""#).unwrap();
        let ev = AgentEvent::Progress {
            id: id(),
            done: 3,
            total: 10,
            label: None,
        };
        assert!(sel.matches(&ev));
    }

    #[test]
    fn match_message_multi_field() {
        let sel = parse_selector(r#"Message role="assistant" summary="hi*""#).unwrap();
        let matching = AgentEvent::Message {
            id: id(),
            role: MessageRole::Assistant,
            summary: "hi there".into(),
        };
        let non_matching = AgentEvent::Message {
            id: id(),
            role: MessageRole::User,
            summary: "hi there".into(),
        };
        assert!(sel.matches(&matching));
        assert!(!sel.matches(&non_matching));
    }

    #[test]
    fn match_user_event_name_gate() {
        let sel = parse_selector("UserEvent:user.hello").unwrap();
        let matching = AgentEvent::UserEvent {
            name: "user.hello".into(),
            payload: serde_json::Value::Null,
            source: "scene".into(),
        };
        let non_matching = AgentEvent::UserEvent {
            name: "user.world".into(),
            payload: serde_json::Value::Null,
            source: "scene".into(),
        };
        assert!(sel.matches(&matching));
        assert!(!sel.matches(&non_matching));
    }

    #[test]
    fn match_user_event_against_other_kind_fails() {
        let sel = parse_selector("UserEvent:user.hello").unwrap();
        let ev = AgentEvent::Log {
            id: id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        assert!(!sel.matches(&ev));
    }

    #[test]
    fn match_user_event_field_on_payload_source() {
        let sel = parse_selector(r#"UserEvent:user.hello source="scene""#).unwrap();
        let matching = AgentEvent::UserEvent {
            name: "user.hello".into(),
            payload: serde_json::Value::Null,
            source: "scene".into(),
        };
        let non_matching = AgentEvent::UserEvent {
            name: "user.hello".into(),
            payload: serde_json::Value::Null,
            source: "core".into(),
        };
        assert!(sel.matches(&matching));
        assert!(!sel.matches(&non_matching));
    }

    #[test]
    fn match_unknown_field_is_no_match() {
        let sel = parse_selector(r#"Log nonexistent="foo""#).unwrap();
        let ev = AgentEvent::Log {
            id: id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        assert!(!sel.matches(&ev));
    }

    #[test]
    fn match_against_struct_field_uses_json() {
        // `tab_handle` is a struct; glob matches against its JSON form.
        let sel = parse_selector(r#"TabOpened tab_handle="*builder*""#).unwrap();
        let ev = AgentEvent::TabOpened {
            id: id(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: TabHandle::new("ark-session", 2, "builder"),
            label: "main".into(),
        };
        assert!(sel.matches(&ev));
    }

    #[test]
    fn match_bool_and_number_fields_render_verbatim() {
        let sel = parse_selector(r#"Iteration n="2""#).unwrap();
        let ev = AgentEvent::Iteration {
            id: id(),
            n: 2,
            max: Some(5),
        };
        assert!(sel.matches(&ev));
    }

    #[test]
    fn match_severity_enum_snake_case() {
        let sel = parse_selector(r#"ReviewComment severity="p1""#).unwrap();
        let ev = AgentEvent::ReviewComment {
            id: id(),
            reviewer: id(),
            severity: Severity::P1,
            path: PathBuf::from("x"),
            line: None,
            body: "b".into(),
        };
        assert!(sel.matches(&ev));
    }

    // -- roundtrip: every EventKind can be used in a bare selector -----

    #[test]
    fn bare_kind_parses_for_every_variant() {
        for kind in EventKind::ALL {
            let sel = parse_selector(kind.as_str()).unwrap();
            assert_eq!(&sel.kind, kind);
        }
    }

    #[test]
    fn field_pattern_with_escaped_quote_in_value() {
        let sel = parse_selector(r#"Error message="he said \"hi\"""#).unwrap();
        let matching = AgentEvent::Error {
            id: id(),
            message: r#"he said "hi""#.into(),
        };
        assert!(sel.matches(&matching));
    }
}
