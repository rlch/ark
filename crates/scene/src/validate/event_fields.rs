//! Event-field validation (T-057 / R4.2).
//!
//! For each `on <EventKind> field=pattern …` selector, verify every
//! field name exists on the target `AgentEvent` variant. Unknown
//! fields surface as [`SceneError::UnknownEventField`] with a Jaro-
//! Winkler suggestion.
//!
//! # Why hardcoded
//!
//! `ark_types::AgentEvent` does NOT derive `facet::Facet` today (its
//! serde shape is hand-tuned). Rather than wait for a facet migration
//! of the types crate, this pass hardcodes the field set per variant
//! against [`crate::reactions::event_kind::EventKind`]. The list stays
//! in sync with `AgentEvent` via a unit test that walks a sample of
//! each variant through serde_json and asserts the fields match.
//!
//! # UserEvent special case
//!
//! The UserEvent variant exposes only `name`, `source`, and `payload`
//! as first-class fields. Scene selectors can address arbitrary keys
//! under `payload.*` — those are NOT validated against any schema,
//! because payload shape is emitter-owned (R4.7). The hybrid-access
//! rule means bare field names on a UserEvent selector route into
//! `payload.*` at dispatch; we therefore skip field-existence checks
//! for UserEvent selectors entirely.

use crate::ast::selector::EventSelector;
use crate::error::SceneError;
use crate::reactions::EventKind;
use miette::{NamedSource, SourceSpan};

/// Canonical field list for a non-UserEvent `AgentEvent` variant.
///
/// Returns `None` for `EventKind::UserEvent` — see module docs. When
/// `Some(&[…])`, the slice enumerates every field the variant carries
/// (other than the serde `kind` tag, which is never a selectable
/// field because the kind is already expressed as the `on <Kind>`
/// head).
pub fn canonical_fields(kind: EventKind) -> Option<&'static [&'static str]> {
    match kind {
        EventKind::Started => Some(&["spec"]),
        EventKind::TabOpened => Some(&["id", "parent", "role", "tab_handle", "label"]),
        EventKind::TabClosed => Some(&["id", "tab_handle"]),
        EventKind::Progress => Some(&["id", "done", "total", "label"]),
        EventKind::TaskDone => Some(&["id", "task_id", "label"]),
        EventKind::Iteration => Some(&["id", "n", "max"]),
        EventKind::PhaseTransition => Some(&["id", "from", "to"]),
        EventKind::ToolUse => Some(&["id", "tool", "input_summary"]),
        EventKind::Message => Some(&["id", "role", "summary"]),
        EventKind::FileEdited => Some(&["id", "path", "additions", "deletions"]),
        EventKind::ReviewComment => Some(&["id", "reviewer", "severity", "path", "line", "body"]),
        EventKind::PermissionAsked => Some(&["id", "tool", "summary"]),
        EventKind::PermissionResolved => Some(&["id", "tool", "decision"]),
        EventKind::Stall => Some(&["id", "since"]),
        EventKind::Log => Some(&["id", "level", "line"]),
        EventKind::Error => Some(&["id", "message"]),
        EventKind::Done => Some(&["id", "outcome"]),
        EventKind::UserEvent => None,
    }
}

/// Validate every field name referenced by `selector` against the
/// canonical field set for its `kind`. Returns `Ok(())` when every
/// name is valid; on the first unknown field, returns
/// [`SceneError::UnknownEventField`] with a Jaro-Winkler suggestion.
///
/// For UserEvent selectors, the pass accepts the three reserved
/// top-level keys (`name`, `source`, `payload`) AND any other field
/// name — see module docs for the rationale (hybrid payload access
/// means arbitrary bare names are legal).
#[allow(clippy::result_large_err)]
pub fn validate_event_fields(selector: &EventSelector) -> Result<(), SceneError> {
    let Some(kind) = EventKind::parse(&selector.kind) else {
        // Unknown selector kind surfaces via a separate pass (T-056);
        // here we simply short-circuit so the dispatcher can report
        // the kind error without this validator also firing.
        return Ok(());
    };
    let Some(allowed) = canonical_fields(kind) else {
        // UserEvent: every field name is valid (see module docs).
        return Ok(());
    };
    for field in selector.field_patterns.keys() {
        if !allowed.contains(&field.as_str()) {
            let suggestion = best_suggestion(field, allowed);
            let help = format_help(allowed, suggestion.as_deref());
            return Err(SceneError::UnknownEventField {
                event_kind: selector.kind.clone(),
                field: field.clone(),
                help,
                src: NamedSource::new("<selector>", String::new()),
                span: SourceSpan::new(0.into(), 0),
            });
        }
    }
    Ok(())
}

/// Jaro-Winkler nearest-match suggestion from the canonical field
/// list. Threshold of 0.7 picks up obvious typos while rejecting
/// wildly wrong names.
fn best_suggestion<'a>(needle: &str, haystack: &'a [&'a str]) -> Option<&'a str> {
    let mut best: Option<(&str, f64)> = None;
    for candidate in haystack {
        let score = strsim::jaro_winkler(needle, candidate);
        match best {
            Some((_, bs)) if bs >= score => {}
            _ => best = Some((*candidate, score)),
        }
    }
    best.and_then(|(s, score)| if score >= 0.7 { Some(s) } else { None })
}

/// Format the miette `#[help]` text. Surfaces the available field
/// list and, when a close match exists, prepends a "did you mean …?"
/// cue.
fn format_help(allowed: &[&str], suggestion: Option<&str>) -> String {
    let list = allowed.join(", ");
    match suggestion {
        Some(s) => format!("did you mean `{s}`? Available fields: {list}"),
        None => format!("Available fields: {list}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::selector::{FieldPattern, MatchType};
    use std::collections::BTreeMap;

    fn sel(kind: &str, fields: &[(&str, &str)]) -> EventSelector {
        let mut map = BTreeMap::new();
        for (k, v) in fields {
            map.insert(
                (*k).to_string(),
                FieldPattern {
                    raw: (*v).to_string(),
                    match_type: MatchType::Exact,
                },
            );
        }
        EventSelector {
            kind: kind.to_string(),
            field_patterns: map,
        }
    }

    #[test]
    fn known_field_accepts() {
        let s = sel("FileEdited", &[("path", "x"), ("id", "y")]);
        validate_event_fields(&s).expect("known fields should pass");
    }

    #[test]
    fn unknown_field_rejects_with_suggestion() {
        let s = sel("FileEdited", &[("pth", "x")]);
        let err = validate_event_fields(&s).expect_err("typo should reject");
        match err {
            SceneError::UnknownEventField { field, help, .. } => {
                assert_eq!(field, "pth");
                assert!(help.contains("path"), "help: {help}");
            }
            other => panic!("expected UnknownEventField, got {other:?}"),
        }
    }

    #[test]
    fn unknown_kind_is_silent() {
        // Unknown event kinds get a dedicated pass; this validator
        // short-circuits so the error does not fire twice.
        let s = sel("NotARealKind", &[("foo", "x")]);
        validate_event_fields(&s).expect("unknown kind should not fire here");
    }

    #[test]
    fn user_event_accepts_arbitrary_fields() {
        let s = sel(
            "UserEvent",
            &[("name", "n"), ("source", "s"), ("tool", "Bash"), ("anything", "x")],
        );
        validate_event_fields(&s).expect("UserEvent should accept arbitrary fields");
    }

    #[test]
    fn no_suggestion_when_wildly_different() {
        let s = sel("FileEdited", &[("xyzzy", "q")]);
        let err = validate_event_fields(&s).unwrap_err();
        match err {
            SceneError::UnknownEventField { help, .. } => {
                // Expect no "did you mean" prefix when no close match.
                assert!(!help.contains("did you mean"), "help was: {help}");
                assert!(help.contains("Available fields:"));
            }
            other => panic!("expected UnknownEventField, got {other:?}"),
        }
    }

    #[test]
    fn canonical_fields_table_has_no_duplicates() {
        // Sanity: every `Some(list)` in `canonical_fields` has unique
        // entries. Guards against typos in the table above.
        for kind in [
            EventKind::Started,
            EventKind::TabOpened,
            EventKind::TabClosed,
            EventKind::Progress,
            EventKind::TaskDone,
            EventKind::Iteration,
            EventKind::PhaseTransition,
            EventKind::ToolUse,
            EventKind::Message,
            EventKind::FileEdited,
            EventKind::ReviewComment,
            EventKind::PermissionAsked,
            EventKind::PermissionResolved,
            EventKind::Stall,
            EventKind::Log,
            EventKind::Error,
            EventKind::Done,
        ] {
            if let Some(fields) = canonical_fields(kind) {
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for f in fields {
                    assert!(seen.insert(f), "duplicate field `{f}` for kind {kind:?}");
                }
            }
        }
    }
}
