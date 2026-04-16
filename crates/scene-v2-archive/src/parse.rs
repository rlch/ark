//! Scene parsing: KDL text → typed `SceneIR`.
//!
//! [`parse_scene`] is the single entry point for turning a scene file's
//! source text into the typed AST defined in [`crate::ast`]. Parsing is
//! done by `facet_kdl::from_str::<SceneNode>`, which surfaces KDL 2.0
//! tokenizer + structural errors as `miette::Diagnostic`s with labeled
//! spans. This module translates those span-bearing errors into the
//! crate's canonical [`SceneError::Parse`] variant so the rest of the
//! compile pipeline sees a single error type.
//!
//! Note on IR-vs-AST: for the current tier, `SceneIR` is a type alias for
//! [`crate::ast::SceneNode`]. Later tiers introduce a lowering step
//! (`SceneAst → SceneIR`) that resolves `extends` / `include` / `use`,
//! materialises default plugins, and merges reactions in textual order
//! (cavekit-scene.md R11). At that point `SceneIR` becomes a distinct
//! type carrying merge-provenance. For now the alias lets call sites
//! write `SceneIR` and keep compiling across the transition. See
//! `context/plans/build-site-scene.md` T-2.x / T-4.x for the lowering
//! work.
//!
//! Span fidelity: `facet_kdl::KdlDeserializeError` implements
//! `miette::Diagnostic::labels()`, so we consume the first labeled span
//! it yields. When the underlying failure is deep inside facet's
//! reflection layer and no span is attached, we fall back to
//! `(0, src.len().min(1))` and leave a TODO for a later tier to improve
//! fidelity upstream.
//!
//! Formatter: [`format_scene`] is a round-trip pretty-printer built on
//! the upstream `kdl` crate's `KdlDocument::autoformat`. It is NOT a
//! validation pass — scene-schema validation is exclusively via
//! facet types in [`parse_scene`]. Formatter rejects only inputs that
//! fail the raw KDL 2.0 tokenizer; anything else round-trips.

use std::path::Path;

use miette::{Diagnostic, NamedSource, SourceSpan};

use crate::ast::SceneDoc;
use crate::compat::preprocess_file_shape;
use crate::error::SceneError;

/// Scene intermediate representation.
///
/// For the current tier this is a type alias for [`crate::ast::SceneDoc`]
/// — the AST root produced directly by `facet_kdl`. A later tier
/// (post-lowering) will promote this to a distinct struct carrying
/// merge-provenance and pre-resolved op bodies. See the module docs.
pub type SceneIR = SceneDoc;

/// Parse a scene file's source text into a typed [`SceneIR`].
///
/// On success returns the AST root. On failure returns a
/// [`SceneError::Parse`] whose `src` is a `NamedSource` keyed by `path`
/// (so miette's renderer prints `<path>:<line>:<col>`), whose `at` is
/// the first labeled span produced by facet-kdl, and whose `message`
/// carries facet-kdl's raw error text.
///
/// ## Span fidelity
///
/// If facet-kdl's error reports at least one `LabeledSpan` via its
/// `miette::Diagnostic::labels()` impl, the first label's offset + len
/// are used. Otherwise we fall back to a placeholder span at offset 0
/// with length `src.len().min(1)` (i.e. the first byte of the file, or
/// zero-length for empty input). This placeholder is a deliberate
/// compromise documented in `build-site-scene.md` T-1.1 — tighter spans
/// require facet-kdl to expose span info for non-parse reflection
/// failures (`MissingField`, `UnknownField`, etc.), which is
/// upstream-work.
// TODO(T-1.x): upstream richer span surface to facet-kdl so non-parse
// errors (MissingField, UnknownField, ExpectedScalarGotStruct) always
// carry a SourceSpan. Today their span is `None` when reflection trips
// mid-event, and we fall back to (0, src.len().min(1)).
#[allow(clippy::result_large_err)] // SceneError carries full NamedSource; matches facet_kdl::from_str.
pub fn parse_scene(src: &str, path: &Path) -> Result<SceneIR, SceneError> {
    // T-14.1: R15 file-shape detection — auto-wrap legacy layout-only files.
    let shape = preprocess_file_shape(src, path)?;
    let effective_src = shape.as_str();

    match facet_kdl::from_str::<SceneDoc>(effective_src) {
        Ok(doc) => Ok(doc),
        Err(err) => Err(kdl_err_to_scene_error(err, effective_src, path)),
    }
}

/// Convert a `facet_kdl::KdlDeserializeError` into `SceneError::Parse`.
///
/// The translation preserves the raw facet-kdl error text via
/// `Display` and harvests the first `LabeledSpan` from the diagnostic
/// as the primary `at` span. See module docs for the fallback rules
/// when no span is available.
fn kdl_err_to_scene_error(
    err: facet_kdl::KdlDeserializeError,
    src: &str,
    path: &Path,
) -> SceneError {
    let message = err.to_string();
    let at = first_label_span(&err).unwrap_or_else(|| {
        // Fallback: point at the start of the file. Non-zero length
        // keeps miette's caret renderer happy even on empty input
        // (len=0 is legal but renders as a zero-width caret).
        SourceSpan::new(0.into(), src.len().min(1))
    });

    SceneError::Parse {
        src: NamedSource::new(path.display().to_string(), src.to_string()),
        at,
        message,
    }
}

/// Pull the first `LabeledSpan` from a `miette::Diagnostic` and convert
/// to `SourceSpan`. Returns `None` when the diagnostic has no labels
/// (e.g. some reflection-layer errors) — callers should fall back.
fn first_label_span(err: &dyn Diagnostic) -> Option<SourceSpan> {
    let mut labels = err.labels()?;
    let label = labels.next()?;
    Some(SourceSpan::new(label.offset().into(), label.len()))
}

/// Round-trip a scene file through the upstream `kdl` crate's
/// formatter, returning a canonicalised string.
///
/// This is NOT a validation pass. The input must be syntactically
/// valid KDL 2.0, but need not conform to the scene grammar — the
/// formatter preserves unknown nodes and attributes unchanged (modulo
/// whitespace). It exists solely so `ark scene fmt` can canonicalise
/// files the user is editing without having to fully type-check them
/// first.
///
/// Errors surface as [`SceneError::Parse`] with facet-kdl-style
/// diagnostics so the formatter's error shape matches the full-parse
/// shape.
#[allow(clippy::result_large_err)] // SceneError carries full NamedSource; matches parse_scene.
pub fn format_scene(src: &str, path: &Path) -> Result<String, SceneError> {
    match kdl::KdlDocument::parse(src) {
        Ok(mut doc) => {
            doc.autoformat();
            Ok(doc.to_string())
        }
        Err(err) => {
            // `kdl::KdlError` implements miette::Diagnostic and exposes
            // its own labels() (through its `.diagnostics` vec). Re-use
            // the same span extraction logic as `parse_scene`.
            let message = err.to_string();
            let at = first_label_span(&err).unwrap_or_else(|| {
                SourceSpan::new(0.into(), src.len().min(1))
            });
            Err(SceneError::Parse {
                src: NamedSource::new(path.display().to_string(), src.to_string()),
                at,
                message,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.kdl")
    }

    /// Minimal valid scene parses: scene wrapper + a layout with nested
    /// tab + pane round-trips through `facet_kdl::from_str` cleanly.
    #[test]
    fn parse_minimal_scene_round_trips() {
        let input = r#"
scene "hello" {
    layout {
        tab "work" {
            pane
        }
    }
}
"#;
        let ir = parse_scene(input, &p()).expect("parse valid scene");
        assert_eq!(ir.scene.name, "hello");
        let layout = ir.scene.layout.as_ref().expect("layout present");
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.tabs[0].name.as_deref(), Some("work"));
        assert_eq!(layout.tabs[0].panes.len(), 1);
    }

    /// The simplest possible scene (no body) also parses.
    #[test]
    fn parse_bareword_scene() {
        let input = r#"scene "bare""#;
        let ir = parse_scene(input, &p()).expect("parse bare scene");
        assert_eq!(ir.scene.name, "bare");
        assert!(ir.scene.layout.is_none());
    }

    /// Malformed input produces `SceneError::Parse` with the right code
    /// and a non-zero-length span. Uses an unterminated string to
    /// guarantee the KDL tokenizer throws a tokenisation-level error
    /// (which always carries a labeled span via the `kdl` crate's
    /// diagnostics vec).
    #[test]
    fn parse_failure_has_parse_code_and_span() {
        // Unterminated string literal — KDL 2.0 tokenizer flags this
        // with a precise span.
        let input = r#"scene "unterminated"#;
        let err = parse_scene(input, &p()).expect_err("malformed scene must error");

        assert_eq!(err.code_enum(), ErrorCode::Parse);

        match err {
            SceneError::Parse { at, message, .. } => {
                assert!(at.len() > 0, "span length must be non-zero, got {at:?}");
                assert!(
                    !message.is_empty(),
                    "facet-kdl error message must be non-empty"
                );
            }
            other => panic!("expected SceneError::Parse, got {other:?}"),
        }
    }

    /// A missing required argument on `scene` (it needs a name string)
    /// produces a `SceneError::Parse` rather than a panic or raw facet
    /// error. Grammar refinement (remapping to `scene/grammar` with a
    /// better message) is later-tier work; at this tier we only
    /// guarantee the `scene/parse` catch-all wraps every failure.
    #[test]
    fn parse_scene_without_name_argument_is_parse_error() {
        // `scene` with no name arg — facet-kdl will reject this
        // because `SceneNode::name` is a non-Option `kdl::argument`.
        let input = r#"scene { }"#;
        let err = parse_scene(input, &p()).expect_err("scene without name must error");
        assert_eq!(err.code_enum(), ErrorCode::Parse);
    }

    /// Formatter produces an equivalent document that itself round-trips
    /// through `parse_scene`. We verify semantic equivalence by parsing
    /// both the original and the formatted output and comparing the
    /// resulting IR shapes on load-bearing fields.
    #[test]
    fn format_round_trips_semantically() {
        let input = r#"
scene    "demo"    {
    layout    {
        tab   "work"  { pane   name="editor" }
    }
}
"#;
        let formatted = format_scene(input, &p()).expect("format valid input");

        // The formatted output must re-parse as a scene.
        let original = parse_scene(input, &p()).expect("original parses");
        let reparsed = parse_scene(&formatted, &p()).expect("formatted re-parses");

        assert_eq!(original.scene.name, reparsed.scene.name);
        let l0 = original.scene.layout.as_ref().expect("layout");
        let l1 = reparsed.scene.layout.as_ref().expect("layout");
        assert_eq!(l0.tabs.len(), l1.tabs.len());
        assert_eq!(l0.tabs[0].name, l1.tabs[0].name);
        assert_eq!(l0.tabs[0].panes.len(), l1.tabs[0].panes.len());
        assert_eq!(l0.tabs[0].panes[0].name, l1.tabs[0].panes[0].name);
    }

    /// Formatter is NOT validation: it only needs the input to be
    /// valid KDL 2.0, not valid scene grammar. A file that's legal KDL
    /// but not legal scene grammar (e.g. bare unknown node at root)
    /// still formats cleanly.
    #[test]
    fn format_accepts_non_scene_kdl() {
        let input = r#"not-a-scene-node "hello""#;
        let out = format_scene(input, &p()).expect("format non-scene KDL");
        assert!(out.contains("not-a-scene-node"));
    }

    /// Formatter surfaces tokenizer errors as `SceneError::Parse`, same
    /// shape as `parse_scene` failures.
    #[test]
    fn format_failure_is_parse_error() {
        let input = r#"scene "unterminated"#;
        let err = format_scene(input, &p()).expect_err("unterminated string must error");
        assert_eq!(err.code_enum(), ErrorCode::Parse);
        if let SceneError::Parse { at, .. } = err {
            assert!(at.len() > 0);
        } else {
            panic!("expected SceneError::Parse");
        }
    }
}
