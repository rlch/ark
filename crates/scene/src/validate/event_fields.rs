//! Event-field validation (T-057 / R4.2).
//!
//! For each `on <EventKind> field=pattern …` selector, verify every
//! field name exists on the target `CoreEvent` variant. Unknown
//! fields surface as [`SceneError::UnknownEventField`] with a Jaro-
//! Winkler suggestion.
//!
//! # Reflection (T-057, scene-v3 S-D)
//!
//! The canonical field list per variant is derived at runtime via
//! `<CoreEvent as facet::Facet>::SHAPE`: we walk the enum's variants,
//! match on variant name, and enumerate `variant.data.fields`. The
//! scene crate used to carry a hardcoded `match kind { … }` table that
//! drifted from `CoreEvent` whenever a new field landed; shape
//! traversal eliminates that sync bug by making `CoreEvent` itself the
//! single source of truth.
//!
//! # Projection-only fields
//!
//! [`FlatEvent::from(&CoreEvent::SessionEnded{..})`] splits `exit:
//! ExitReason` into two flat payload keys — `exit` (discriminant) and
//! `exit_message` (Error's payload). The `exit_message` name is therefore
//! valid in selectors even though it isn't on the `SessionEnded` struct
//! itself. [`flat_projection_extras`] lists these projection-only
//! extras per variant; [`canonical_fields`] overlays them onto the
//! shape-derived base list before the contains-check.
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

use std::sync::OnceLock;

use crate::ast::selector::EventSelector;
use crate::error::SceneError;
use crate::reactions::EventKind;
use ark_types::CoreEvent;
use facet::{Facet, Type, UserType};
use miette::{NamedSource, SourceSpan};

/// Selectable fields that exist in [`ark_types::FlatEvent`]'s payload
/// projection but NOT on the corresponding `CoreEvent` struct variant.
///
/// Today the only projection-only name is `SessionEnded.exit_message`:
/// [`FlatEvent::from(&CoreEvent::SessionEnded{..})`] splits the
/// `ExitReason::Error(msg)` payload into a top-level `exit_message`
/// key. Scene selectors like `on SessionEnded exit_message="..."` are
/// legal, so the validator adds these names after the shape-derived
/// base list.
fn flat_projection_extras(variant_name: &str) -> &'static [&'static str] {
    match variant_name {
        "SessionEnded" => &["exit_message"],
        _ => &[],
    }
}

/// Map an [`EventKind`] to the enum-variant identifier written in
/// [`CoreEvent`]. The scene-side snake_case naming (`session_started`,
/// `session_ended`, …) diverges from the Rust variant name
/// (`SessionStarted`, `SessionEnded`) because [`CoreEvent`] carries
/// `#[serde(rename_all = "snake_case")]` — but facet reflection uses
/// the Rust name, so we translate here rather than layering snake_case
/// logic onto the reflection walk.
fn variant_rust_name(kind: EventKind) -> Option<&'static str> {
    match kind {
        EventKind::Log => Some("Log"),
        EventKind::Error => Some("Error"),
        EventKind::SessionStarted => Some("SessionStarted"),
        EventKind::SessionEnded => Some("SessionEnded"),
        EventKind::Ext => None,
    }
}

/// Lazily-reflected field set for every non-Ext `CoreEvent` variant.
///
/// Built once on first call from `<CoreEvent as Facet>::SHAPE` +
/// [`flat_projection_extras`]. Subsequent calls hit a `OnceLock` and
/// cost one atomic load.
fn fields_for_variant(variant_name: &str) -> Option<&'static [&'static str]> {
    static CACHE: OnceLock<Vec<(&'static str, Vec<&'static str>)>> = OnceLock::new();
    let table = CACHE.get_or_init(|| {
        let mut out: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
        let shape = <CoreEvent as Facet>::SHAPE;
        let en = match &shape.ty {
            Type::User(UserType::Enum(e)) => e,
            // A non-enum CoreEvent would be a grammar break — fall back
            // to an empty table so the validator silently accepts every
            // name; the roundtrip test below would catch the regression.
            _ => return out,
        };
        for variant in en.variants {
            let mut fields: Vec<&'static str> =
                variant.data.fields.iter().map(|f| f.name).collect();
            for extra in flat_projection_extras(variant.name) {
                if !fields.contains(extra) {
                    fields.push(*extra);
                }
            }
            out.push((variant.name, fields));
        }
        out
    });
    table.iter().find_map(|(name, fields)| {
        if *name == variant_name {
            // Safe to return a slice of the cached Vec: the `OnceLock`
            // owns the Vec for program lifetime, so the slice is
            // effectively `'static`.
            Some(fields.as_slice())
        } else {
            None
        }
    })
}

/// Canonical field list for a non-Ext `CoreEvent` variant.
///
/// Returns `None` for `EventKind::Ext` (extension selectors accept
/// arbitrary names — see module docs). Otherwise returns the union of
/// the SHAPE-derived struct fields and [`flat_projection_extras`] for
/// that variant.
pub fn canonical_fields(kind: EventKind) -> Option<&'static [&'static str]> {
    let name = variant_rust_name(kind)?;
    fields_for_variant(name)
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
            &[
                ("name", "n"),
                ("payload", "{}"),
                ("anything", "x"),
                ("tool", "Bash"),
            ],
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

    // T-057: the canonical list is derived from `<CoreEvent as
    // Facet>::SHAPE`, not from a hardcoded table. The next two tests
    // lock in that contract so a `CoreEvent` field rename stays in
    // sync with the validator automatically.

    #[test]
    fn canonical_fields_from_shape_contains_log_fields() {
        let fields = canonical_fields(EventKind::Log).expect("Log has fields");
        for expected in ["level", "message", "target"] {
            assert!(
                fields.contains(&expected),
                "Log should expose `{expected}` via SHAPE reflection, got {fields:?}"
            );
        }
    }

    #[test]
    fn canonical_fields_session_ended_includes_flat_projection_extras() {
        // `exit_message` lives only in the FlatEvent projection, not on
        // the `SessionEnded` struct — the validator overlays it on top
        // of the shape-derived base list via `flat_projection_extras`.
        let fields = canonical_fields(EventKind::SessionEnded).expect("SessionEnded has fields");
        assert!(fields.contains(&"terminated_at"), "{fields:?}");
        assert!(fields.contains(&"exit"), "{fields:?}");
        assert!(
            fields.contains(&"exit_message"),
            "projection extra missing: {fields:?}"
        );
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
