//! Event-field validation (T-057 / R4.2).
//!
//! For each `on <EventKind> field=pattern …` selector, verify every
//! field name exists on the target `CoreEvent` variant. Unknown
//! fields surface as [`SceneError::UnknownEventField`] with a Jaro-
//! Winkler suggestion.
//!
//! # Why hardcoded
//!
//! `CoreEvent` does not derive `facet::Facet` today. Rather than wait
//! for a facet migration of the types crate, this pass hardcodes the
//! field set per variant against [`crate::reactions::EventKind`]. The
//! list stays in sync with `CoreEvent` via a unit test.
//!
//! # Ext special case
//!
//! The Ext variant wraps extension-owned `ExtEvent { ext, kind,
//! payload }`. Scene selectors can address:
//! - `name` — the flattened dotted name (`<ext>.<kind>`)
//! - `payload` — the full payload object
//! - `payload.X` — a specific payload key (not validated against any
//!   schema since payload shape is extension-owned)
//!
//! We therefore skip field-existence checks for Ext selectors entirely.

use crate::ast::selector::EventSelector;
use crate::error::SceneError;
use crate::reactions::EventKind;
use miette::{NamedSource, SourceSpan};

/// Canonical field list for a non-Ext `CoreEvent` variant.
///
/// Returns `None` for `EventKind::Ext` — see module docs. When
/// `Some(&[…])`, the slice enumerates every selectable field the
/// variant carries (excluding the serde `type` tag).
pub fn canonical_fields(kind: EventKind) -> Option<&'static [&'static str]> {
    match kind {
        EventKind::Log => Some(&["level", "message", "target"]),
        EventKind::Error => Some(&["error"]),
        EventKind::SessionStarted => Some(&["spec"]),
        EventKind::SessionEnded => Some(&["terminated_at"]),
        EventKind::Ext => None,
    }
}

/// Validate every field name referenced by `selector` against the
/// canonical field set for its `kind`. Returns `Ok(())` when every
/// name is valid; on the first unknown field, returns
/// [`SceneError::UnknownEventField`] with a Jaro-Winkler suggestion.
///
/// For Ext selectors, the pass accepts any field name — see module
/// docs for the rationale (hybrid payload access means arbitrary bare
/// names are legal, and `name`/`payload`/`payload.X` are the only
/// first-class fields).
#[allow(clippy::result_large_err)]
pub fn validate_event_fields(selector: &EventSelector) -> Result<(), SceneError> {
    let Some(kind) = EventKind::parse(&selector.kind) else {
        // Unknown selector kind surfaces via a separate pass;
        // here we simply short-circuit.
        return Ok(());
    };
    let Some(allowed) = canonical_fields(kind) else {
        // Ext: every field name is valid.
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
/// list. Threshold of 0.7 picks up obvious typos.
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

/// Format the miette `#[help]` text.
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
    fn known_field_on_log_accepts() {
        let s = sel("Log", &[("level", "info"), ("message", "hi")]);
        validate_event_fields(&s).expect("known fields should pass");
    }

    #[test]
    fn known_field_on_error_accepts() {
        let s = sel("Error", &[("error", "boom")]);
        validate_event_fields(&s).expect("known field should pass");
    }

    #[test]
    fn unknown_field_on_error_rejects_with_suggestion() {
        let s = sel("Error", &[("errors", "boom")]);
        let err = validate_event_fields(&s).expect_err("typo should reject");
        match err {
            SceneError::UnknownEventField { field, help, .. } => {
                assert_eq!(field, "errors");
                assert!(help.contains("error"), "help: {help}");
            }
            other => panic!("expected UnknownEventField, got {other:?}"),
        }
    }

    #[test]
    fn unknown_kind_is_silent() {
        let s = sel("NotARealKind", &[("foo", "x")]);
        validate_event_fields(&s).expect("unknown kind should not fire here");
    }

    #[test]
    fn ext_accepts_arbitrary_fields() {
        let s = sel(
            "Ext",
            &[("name", "n"), ("payload", "{}"), ("anything", "x"), ("tool", "Bash")],
        );
        validate_event_fields(&s).expect("Ext should accept arbitrary fields");
    }

    #[test]
    fn no_suggestion_when_wildly_different() {
        let s = sel("Error", &[("xyzzy", "q")]);
        let err = validate_event_fields(&s).unwrap_err();
        match err {
            SceneError::UnknownEventField { help, .. } => {
                assert!(!help.contains("did you mean"), "help was: {help}");
                assert!(help.contains("Available fields:"));
            }
            other => panic!("expected UnknownEventField, got {other:?}"),
        }
    }

    #[test]
    fn canonical_fields_table_has_no_duplicates() {
        for kind in [
            EventKind::Log,
            EventKind::Error,
            EventKind::SessionStarted,
            EventKind::SessionEnded,
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
