//! Shared grammar types used by both `on` reactions (R4) and `bind` blocks
//! (R5) to describe *how* an event selector's field patterns are interpreted
//! at runtime.
//!
//! The KDL-level selector kind (the `<EventKind>` in `on <EventKind> … { }`)
//! is modelled on [`crate::ast::OnNode`] (T-003). This module owns the
//! *field-pattern* half of R4.1 / R4.3: the typed RHS of every
//! `field=pattern` property on an `on`/`clear-reactions` node.
//!
//! Explicit type annotations per R4.3:
//!
//! - `(glob)<pattern>` — glob match.
//! - `(exact)<pattern>` — literal equality.
//! - `(regex)<pattern>` — regex match.
//!
//! With no annotation, default inference applies (per R4.3): `Glob` when the
//! field name is `path` or ends with `_path`, otherwise `Exact`. Compilation
//! of glob/regex patterns is deferred to the runtime matcher (T-056+); this
//! module classifies the pattern and stores the raw string only.

use std::collections::BTreeMap;

use facet::Facet;
use thiserror::Error;

/// Parsed event selector: the `on <Kind> field=pat field=pat` RHS lifted out
/// of KDL into AST form. Also the shape used by `clear-reactions
/// event="<selector>"` (R11) when matching reactions for removal.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct EventSelector {
    /// Event kind — the bare identifier after `on` (e.g. `"Error"`,
    /// `"Ext"`, `"myext.something"`). Validated against the `CoreEvent`
    /// variant set by a later compile pass (T-057); this struct carries it
    /// verbatim as authored.
    pub kind: String,

    /// Map of field name → typed pattern. `BTreeMap` so iteration order is
    /// deterministic across reparse (important for diagnostic snapshots and
    /// `ark scene fmt` idempotence).
    pub field_patterns: BTreeMap<String, FieldPattern>,
}

/// One field pattern (`field=value` on an `on` node), classified against the
/// annotation rules of R4.3.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
pub struct FieldPattern {
    /// Pattern string *as the author wrote it*, minus any leading
    /// `(glob)` / `(exact)` / `(regex)` type annotation. The annotation is
    /// consumed during parse; downstream matchers only see the payload.
    pub raw: String,

    /// Resolved match discipline for `raw`. Either picked from an explicit
    /// annotation or inferred from the field name (see
    /// [`FieldPattern::parse`]).
    pub match_type: MatchType,
}

/// How to interpret a [`FieldPattern::raw`] against an incoming event field
/// value. Three disciplines are supported per R4.3; there is no open-ended
/// set. Compilation of `Glob` / `Regex` patterns happens inside the runtime
/// matcher (T-056+), not here.
#[derive(Facet, Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum MatchType {
    /// Shell-style glob (`*`, `**`, `?`, `[abc]`). Default for path-like
    /// fields: the field name is `path` or ends with `_path`.
    Glob,

    /// Literal string equality. Default for any field that isn't path-like.
    Exact,

    /// Regular expression. Only selected via an explicit `(regex)`
    /// annotation — there is no default that produces `Regex`.
    Regex,
}

/// Errors surfaced by [`parse_selector`] / [`FieldPattern::parse`].
///
/// These live *separately* from `SceneError` (T-006): integration into the
/// top-level diagnostic tree happens via a later `From<SelectorParseError>
/// for SceneError` impl in Tier 1 (T-013+). That lets this module stay a
/// leaf of the dependency graph and keeps the selector grammar unit-
/// testable without dragging the full `SceneError` surface into scope.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SelectorParseError {
    /// The pattern body was empty — e.g. `path=""` or `path="(glob)"`. An
    /// empty pattern is never meaningful (a glob of `""` matches nothing,
    /// an exact match of `""` is almost always a bug).
    #[error("empty field pattern")]
    EmptyPattern,
}

impl FieldPattern {
    /// Parse one `field=value` RHS into a classified [`FieldPattern`].
    ///
    /// Returns a [`Result`] rather than an infallible value because an
    /// explicit `(…)` annotation can be malformed or name an unknown
    /// discipline (see [`SelectorParseError`]). On the no-annotation path
    /// parsing never fails — the default-inference rule (`Glob` for
    /// path-like field names, `Exact` otherwise) always produces a valid
    /// [`FieldPattern`].
    ///
    /// `field_name` is the LHS of the property (e.g. `"path"`, `"tool"`)
    /// and is consulted only for the default-inference branch; the explicit
    /// annotation always overrides it. `raw_value` is the property value
    /// exactly as the KDL parser handed it over, including any leading
    /// annotation.
    pub fn parse(field_name: &str, raw_value: &str) -> Result<Self, SelectorParseError> {
        if raw_value.is_empty() {
            return Err(SelectorParseError::EmptyPattern);
        }

        // Only treat the value as annotated when it begins with EXACTLY one
        // of `(glob)`, `(exact)`, `(regex)`. Any other `(`-prefixed value
        // (e.g. `"(foo"`, `"(glob)"` with empty body) falls through to the
        // default-inference branch so legitimate literals starting with `(`
        // stay expressible.
        for (prefix, match_type) in [
            ("(glob)", MatchType::Glob),
            ("(exact)", MatchType::Exact),
            ("(regex)", MatchType::Regex),
        ] {
            if let Some(body) = raw_value.strip_prefix(prefix) {
                if body.is_empty() {
                    return Err(SelectorParseError::EmptyPattern);
                }
                return Ok(FieldPattern {
                    raw: body.to_string(),
                    match_type,
                });
            }
        }

        let match_type = if is_path_like_field(field_name) {
            MatchType::Glob
        } else {
            MatchType::Exact
        };
        Ok(FieldPattern {
            raw: raw_value.to_string(),
            match_type,
        })
    }
}

/// Parse the RHS of a single `field=pattern` selector property.
///
/// Thin module-level alias for [`FieldPattern::parse`] — exposed as a
/// free function so the reaction compiler (T-013) and the runtime matcher
/// (T-056) can call it without naming the type. Intentionally pub; see
/// module docs for why this lives here and not in `error.rs`.
pub fn parse_selector(
    field_name: &str,
    raw_value: &str,
) -> Result<FieldPattern, SelectorParseError> {
    FieldPattern::parse(field_name, raw_value)
}

/// Inference helper: a field is "path-like" — and therefore defaults to
/// [`MatchType::Glob`] — when its name is exactly `path` or ends with
/// `_path` (e.g. `src_path`, `dest_path`). Per R4.3.
fn is_path_like_field(field_name: &str) -> bool {
    field_name == "path" || field_name.ends_with("_path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_defaults_to_glob() {
        let fp = FieldPattern::parse("path", "**/*.md").unwrap();
        assert_eq!(fp.match_type, MatchType::Glob);
        assert_eq!(fp.raw, "**/*.md");
    }

    #[test]
    fn suffix_path_defaults_to_glob() {
        let fp = FieldPattern::parse("src_path", "x").unwrap();
        assert_eq!(fp.match_type, MatchType::Glob);
        assert_eq!(fp.raw, "x");
    }

    #[test]
    fn non_path_field_defaults_to_exact() {
        let fp = FieldPattern::parse("tool", "Bash").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "Bash");
    }

    #[test]
    fn explicit_regex_annotation_strips_prefix() {
        let fp = FieldPattern::parse("path", "(regex)^foo.*bar$").unwrap();
        assert_eq!(fp.match_type, MatchType::Regex);
        assert_eq!(fp.raw, "^foo.*bar$");
    }

    #[test]
    fn explicit_glob_overrides_exact_default() {
        let fp = FieldPattern::parse("tool", "(glob)Bash*").unwrap();
        assert_eq!(fp.match_type, MatchType::Glob);
        assert_eq!(fp.raw, "Bash*");
    }

    #[test]
    fn explicit_exact_annotation_on_non_path_field() {
        let fp = FieldPattern::parse("tool", "(exact)literal").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "literal");
    }

    #[test]
    fn explicit_exact_overrides_glob_default_on_path_field() {
        let fp = FieldPattern::parse("path", "(exact)literal/path").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "literal/path");
    }

    #[test]
    fn non_annotation_parenthesised_value_is_literal() {
        // F-0003 fix — only exact `(glob)`, `(exact)`, `(regex)` prefixes
        // count as type annotations. Everything else is a regular value
        // and falls through to default-inference.
        let fp = FieldPattern::parse("x", "(unknown)foo").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "(unknown)foo");

        let fp = FieldPattern::parse("x", "(globfoo").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "(globfoo");

        let fp = FieldPattern::parse("tool", "(foo").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
        assert_eq!(fp.raw, "(foo");
    }

    #[test]
    fn empty_raw_value_is_error() {
        let err = FieldPattern::parse("path", "").unwrap_err();
        assert_eq!(err, SelectorParseError::EmptyPattern);
    }

    #[test]
    fn empty_body_after_annotation_is_error() {
        let err = FieldPattern::parse("path", "(glob)").unwrap_err();
        assert_eq!(err, SelectorParseError::EmptyPattern);
    }

    #[test]
    fn parse_selector_alias_matches_method() {
        let via_alias = parse_selector("path", "**/*.md").unwrap();
        let via_method = FieldPattern::parse("path", "**/*.md").unwrap();
        assert_eq!(via_alias, via_method);
    }

    #[test]
    fn field_name_without_underscore_path_suffix_is_exact() {
        let fp = FieldPattern::parse("filepath", "x").unwrap();
        assert_eq!(fp.match_type, MatchType::Exact);
    }

    #[test]
    fn event_selector_uses_btreemap_deterministic_order() {
        let mut patterns = BTreeMap::new();
        patterns.insert(
            "path".to_string(),
            FieldPattern::parse("path", "**/*.md").unwrap(),
        );
        patterns.insert(
            "tool".to_string(),
            FieldPattern::parse("tool", "Bash").unwrap(),
        );
        let sel = EventSelector {
            kind: "FileEdited".to_string(),
            field_patterns: patterns,
        };
        let keys: Vec<&String> = sel.field_patterns.keys().collect();
        assert_eq!(keys, vec!["path", "tool"]);
    }
}
