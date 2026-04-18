//! File-shape detection and normalization (T-112 / R15).
//!
//! Scene files can appear in two valid shapes:
//!
//! (a) **Wrapped** — `scene "name" { … }` at top level. Used directly.
//! (b) **Bare layout** — top-level `layout { … }` without a scene wrapper.
//!     Auto-wrapped as `scene "default" { … }` with a debug log.
//!
//! Two error shapes:
//!
//! (c) **Empty / unknown** — neither `scene` nor `layout` at top level.
//! (d) **Ambiguous** — both a `scene` wrapper AND a bare `layout` at top level.
//!
//! [`detect_and_normalize`] runs *before* the typed `parse_scene` pass so that
//! callers always feed a well-shaped `scene` document into the facet-kdl parser.

use std::path::Path;

use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;

/// Detect the file shape and normalize the source to a `scene`-wrapped document.
///
/// Returns the (possibly rewritten) source text on success. Callers should feed
/// the returned string into [`crate::parse::parse_scene`].
#[allow(clippy::result_large_err)]
pub fn detect_and_normalize(src: &str, path: &Path) -> Result<String, SceneError> {
    let doc = kdl::KdlDocument::parse(src).map_err(|e| SceneError::Parse {
        message: e.to_string(),
        src: NamedSource::new(path.display().to_string(), src.to_string()),
        span: SourceSpan::new(0.into(), src.len().min(1)),
    })?;

    let nodes = doc.nodes();

    // Locate span info for the first `scene` and first `layout` node.
    let scene_node = nodes.iter().find(|n| n.name().value() == "scene");
    let layout_node = nodes.iter().find(|n| n.name().value() == "layout");

    let has_scene = scene_node.is_some();
    let has_layout = layout_node.is_some();

    match (has_scene, has_layout) {
        // (a) Normal wrapped scene — pass through unchanged.
        (true, false) => Ok(src.to_string()),

        // (b) Bare layout without scene wrapper — auto-wrap.
        (false, true) => {
            tracing::debug!(
                path = %path.display(),
                "auto-wrapping bare layout as scene \"default\""
            );
            Ok(format!("scene \"default\" {{\n{src}\n}}"))
        }

        // (d) Both scene AND bare layout at top level — ambiguous.
        (true, true) => {
            let scene_span = node_name_span(src, scene_node.unwrap());
            let layout_span = node_name_span(src, layout_node.unwrap());
            Err(SceneError::AmbiguousFileShape {
                src: NamedSource::new(path.display().to_string(), src.to_string()),
                scene_span,
                layout_span,
            })
        }

        // (c) Neither scene nor layout — empty or unrecognised.
        (false, false) => {
            let span = if let Some(first) = nodes.first() {
                node_name_span(src, first)
            } else {
                SourceSpan::new(0.into(), 0)
            };
            Err(SceneError::EmptyOrUnknown {
                src: NamedSource::new(path.display().to_string(), src.to_string()),
                span,
            })
        }
    }
}

/// Compute a `SourceSpan` covering a node's name in the source text.
///
/// Uses the parser-provided span from the `kdl` crate rather than a
/// text search, so the span is always accurate even when the node name
/// appears earlier in a comment or string literal.
fn node_name_span(_src: &str, node: &kdl::KdlNode) -> SourceSpan {
    let span = node.name().span();
    SourceSpan::new(span.offset().into(), span.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    // ── Case (a): scene wrapper present ─────────────────────────────

    #[test]
    fn case_a_scene_wrapper_passes_through() {
        let src = r#"scene "dev" { layout { } }"#;
        let out = detect_and_normalize(src, &p("a.kdl")).unwrap();
        assert_eq!(out, src, "wrapped scene should pass through unchanged");
    }

    #[test]
    fn case_a_scene_with_reactions_and_binds() {
        let src = r#"
scene "dev" {
    layout { }
    on "FileEdited" { }
    bind "Alt q" { }
}
"#;
        let out = detect_and_normalize(src, &p("a2.kdl")).unwrap();
        assert_eq!(out, src);
    }

    // ── Case (b): bare layout, auto-wrap ────────────────────────────

    #[test]
    fn case_b_bare_layout_auto_wraps() {
        let src = "layout {\n    tab \"@main\" { }\n}";
        let out = detect_and_normalize(src, &p("b.kdl")).unwrap();
        assert!(
            out.starts_with("scene \"default\" {"),
            "should start with scene wrapper: {out}"
        );
        assert!(
            out.contains("layout {"),
            "original layout should be inside: {out}"
        );
    }

    #[test]
    fn case_b_bare_layout_with_extra_nodes() {
        // A bare layout file that also has `on` and `bind` — all should
        // be wrapped together.
        let src = "layout { }\non \"Ready\" { }\nbind \"Alt q\" { }";
        let out = detect_and_normalize(src, &p("b2.kdl")).unwrap();
        assert!(out.starts_with("scene \"default\" {"));
        assert!(out.contains("on \"Ready\""));
        assert!(out.contains("bind \"Alt q\""));
    }

    // ── Case (c): neither scene nor layout ──────────────────────────

    #[test]
    fn case_c_empty_file() {
        let src = "";
        let err = detect_and_normalize(src, &p("c.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::EmptyOrUnknown { .. }),
            "empty file should be EmptyOrUnknown, got: {err:?}"
        );
    }

    #[test]
    fn case_c_only_comments() {
        let src = "// just a comment\n// nothing else";
        let err = detect_and_normalize(src, &p("c2.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::EmptyOrUnknown { .. }),
            "comments-only file should be EmptyOrUnknown, got: {err:?}"
        );
    }

    #[test]
    fn case_c_bare_on_without_layout_or_scene() {
        let src = "on \"Ready\" { }\nbind \"Alt q\" { }";
        let err = detect_and_normalize(src, &p("c3.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::EmptyOrUnknown { .. }),
            "bare on/bind without layout should be EmptyOrUnknown, got: {err:?}"
        );
    }

    #[test]
    fn case_c_bare_bind_only() {
        let src = "bind \"Ctrl s\" { }";
        let err = detect_and_normalize(src, &p("c4.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::EmptyOrUnknown { .. }),
            "bare bind-only should be EmptyOrUnknown, got: {err:?}"
        );
    }

    // ── Case (d): both scene AND bare layout ────────────────────────

    #[test]
    fn case_d_both_scene_and_layout() {
        let src = "scene \"x\" { }\nlayout { }";
        let err = detect_and_normalize(src, &p("d.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::AmbiguousFileShape { .. }),
            "both scene + layout should be AmbiguousFileShape, got: {err:?}"
        );
    }

    #[test]
    fn case_d_layout_before_scene() {
        let src = "layout { }\nscene \"x\" { }";
        let err = detect_and_normalize(src, &p("d2.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::AmbiguousFileShape { .. }),
            "layout-then-scene should be AmbiguousFileShape, got: {err:?}"
        );
    }

    // ── Edge: invalid KDL ───────────────────────────────────────────

    #[test]
    fn invalid_kdl_returns_parse_error() {
        let src = "not valid kdl {{{";
        let err = detect_and_normalize(src, &p("bad.kdl")).unwrap_err();
        assert!(
            matches!(err, SceneError::Parse { .. }),
            "invalid KDL should be Parse error, got: {err:?}"
        );
    }
}
