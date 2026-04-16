//! Legacy-layout auto-wrap for scene migration (T-14.1 / R15).
//!
//! Before passing KDL text to the facet-kdl deserialization pipeline, this
//! module probes the file's top-level node structure and classifies it into
//! one of four shapes:
//!
//! | Rule | Shape | Action |
//! |------|-------|--------|
//! | (a) | Has `scene "…" { }` top-level node | Pass through unchanged |
//! | (b) | Has top-level `layout { }` but no `scene` | Auto-wrap as `scene "default" { … }` |
//! | (c) | Has neither `scene` nor `layout` | Error `scene/empty-or-unknown` |
//! | (d) | Has both `scene` AND top-level `layout` | Error `scene/ambiguous-file-shape` |
//!
//! Rule (b) emits a `tracing::debug!` log so migration tooling can detect
//! files that still use the legacy layout-only shape.
//!
//! The preprocessing step runs on the raw `kdl::KdlDocument` parse tree,
//! BEFORE facet-kdl's typed deserialization. This is intentional: the KDL
//! crate can parse any valid KDL 2.0 regardless of scene grammar, whereas
//! facet-kdl rejects unknown root nodes. By intercepting here we can
//! synthesise the `scene` wrapper without touching the source file.

use std::path::Path;

use kdl::KdlDocument;
use miette::{Diagnostic, NamedSource, SourceSpan};

use crate::error::SceneError;

/// Result of the file-shape probe.
///
/// `Ok(Cow::Borrowed(src))` when the input is already scene-wrapped (rule a);
/// `Ok(Cow::Owned(rewritten))` when a synthetic scene wrapper was injected
/// (rule b). Errors for rules (c) and (d).
#[derive(Debug)]
pub enum FileShape<'a> {
    /// Rule (a): input already has a `scene` wrapper — use as-is.
    PassThrough(&'a str),
    /// Rule (b): input had a bare `layout { }` — return the rewritten KDL
    /// with a synthetic `scene "default" { … }` wrapper.
    AutoWrapped(String),
}

impl<'a> FileShape<'a> {
    /// Borrow the KDL text regardless of variant.
    pub fn as_str(&self) -> &str {
        match self {
            FileShape::PassThrough(s) => s,
            FileShape::AutoWrapped(s) => s.as_str(),
        }
    }
}

/// Inspect a scene file's top-level KDL nodes and classify the file shape
/// per R15 migration rules.
///
/// The caller should pass the result's `.as_str()` to the facet-kdl
/// deserialization pipeline.
///
/// # Errors
///
/// * [`SceneError::Parse`] — input is not valid KDL 2.0.
/// * [`SceneError::AmbiguousFileShape`] — rule (d): both `scene` and
///   top-level `layout` present.
/// * [`SceneError::EmptyOrUnknown`] — rule (c): neither `scene` nor
///   `layout` found.
#[allow(clippy::result_large_err)]
pub fn preprocess_file_shape<'a>(
    src: &'a str,
    path: &Path,
) -> Result<FileShape<'a>, SceneError> {
    // Step 1: parse raw KDL. Any tokenizer failure becomes SceneError::Parse
    // immediately — the facet-kdl pipeline would fail the same way, but we
    // surface it here so the caller gets a uniform error type.
    let doc = KdlDocument::parse(src).map_err(|err| {
        let diag: &dyn Diagnostic = &err;
        let at = diag
            .labels()
            .and_then(|mut l| l.next())
            .map(|l| SourceSpan::new(l.offset().into(), l.len()))
            .unwrap_or_else(|| SourceSpan::new(0.into(), src.len().min(1)));
        SceneError::Parse {
            src: NamedSource::new(path.display().to_string(), src.to_string()),
            at,
            message: err.to_string(),
        }
    })?;

    // Step 2: scan top-level nodes for `scene` and `layout`.
    let mut scene_node: Option<&kdl::KdlNode> = None;
    let mut layout_node: Option<&kdl::KdlNode> = None;

    for node in doc.nodes() {
        match node.name().value() {
            "scene" => {
                scene_node = Some(node);
            }
            "layout" => {
                layout_node = Some(node);
            }
            _ => {}
        }
    }

    let named_src =
        || NamedSource::new(path.display().to_string(), src.to_string());

    match (scene_node, layout_node) {
        // Rule (d): both present — ambiguous.
        (Some(scene), Some(layout)) => Err(SceneError::AmbiguousFileShape {
            src: named_src(),
            scene_at: scene.name().span(),
            layout_at: layout.name().span(),
        }),

        // Rule (a): scene present, no stray layout — pass through.
        (Some(_scene), None) => Ok(FileShape::PassThrough(src)),

        // Rule (b): layout only — auto-wrap.
        (None, Some(_layout)) => {
            tracing::debug!(
                path = %path.display(),
                "legacy layout-only file detected; auto-wrapping in `scene \"default\" {{ }}`"
            );

            // Build the synthetic wrapper. We preserve the original text
            // verbatim as the body of a `scene "default" { … }` node so
            // span offsets inside the body remain valid relative to the
            // rewritten string (they shift by the prefix length, but
            // facet-kdl re-parses from scratch anyway).
            let wrapped = format!("scene \"default\" {{\n{src}\n}}");
            Ok(FileShape::AutoWrapped(wrapped))
        }

        // Rule (c): neither present — error.
        (None, None) => {
            let at = doc
                .nodes()
                .first()
                .map(|n| n.name().span())
                .unwrap_or_else(|| SourceSpan::new(0.into(), 0));
            Err(SceneError::EmptyOrUnknown {
                src: named_src(),
                at,
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
    use insta::assert_snapshot;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.kdl")
    }

    // -- Rule (a): scene pass-through ----------------------------------------

    #[test]
    fn rule_a_scene_wrapper_passes_through() {
        let input = r#"scene "hello" {
    layout {
        tab "work" {
            pane
        }
    }
}"#;
        let result = preprocess_file_shape(input, &p()).expect("should pass through");
        assert!(
            matches!(result, FileShape::PassThrough(_)),
            "expected PassThrough, got {result:?}"
        );
        assert_eq!(result.as_str(), input);
    }

    #[test]
    fn rule_a_bare_scene_no_body() {
        let input = r#"scene "bare""#;
        let result = preprocess_file_shape(input, &p()).expect("should pass through");
        assert!(matches!(result, FileShape::PassThrough(_)));
    }

    // -- Rule (b): layout auto-wrap ------------------------------------------

    #[test]
    fn rule_b_layout_only_auto_wraps() {
        let input = r#"layout {
    tab "work" {
        pane
    }
}"#;
        let result = preprocess_file_shape(input, &p()).expect("should auto-wrap");
        assert!(
            matches!(result, FileShape::AutoWrapped(_)),
            "expected AutoWrapped, got {result:?}"
        );

        let wrapped = result.as_str();
        assert!(wrapped.contains("scene \"default\""));
        assert!(wrapped.contains("layout {"));
    }

    #[test]
    fn rule_b_auto_wrapped_parses_as_scene() {
        let input = r#"layout {
    tab "work" {
        pane
    }
}"#;
        let result = preprocess_file_shape(input, &p()).expect("should auto-wrap");
        let wrapped = result.as_str();

        // The rewritten KDL must parse through the facet-kdl pipeline.
        let doc: crate::ast::SceneDoc =
            facet_kdl::from_str(wrapped).expect("auto-wrapped KDL must parse as SceneDoc");
        assert_eq!(doc.scene.name, "default");
        let layout = doc.scene.layout.as_ref().expect("layout present");
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.tabs[0].name.as_deref(), Some("work"));
    }

    #[test]
    fn rule_b_snapshot_wrapped_output() {
        let input = r#"layout {
    tab "work" {
        pane
    }
}"#;
        let result = preprocess_file_shape(input, &p()).expect("should auto-wrap");
        assert_snapshot!("auto_wrapped_layout", result.as_str());
    }

    // -- Rule (c): empty / unknown -------------------------------------------

    #[test]
    fn rule_c_empty_file_errors() {
        let input = "";
        let err = preprocess_file_shape(input, &p()).expect_err("empty file must error");
        assert_eq!(err.code_enum(), ErrorCode::EmptyOrUnknown);
    }

    #[test]
    fn rule_c_unknown_nodes_error() {
        let input = r#"plugin "foo" { }"#;
        let err =
            preprocess_file_shape(input, &p()).expect_err("unknown root shape must error");
        assert_eq!(err.code_enum(), ErrorCode::EmptyOrUnknown);
    }

    #[test]
    fn rule_c_snapshot_error_display() {
        let input = r#"plugin "foo" { }"#;
        let err = preprocess_file_shape(input, &p()).expect_err("must error");
        assert_snapshot!("empty_or_unknown_error", err.to_string());
    }

    // -- Rule (d): ambiguous file shape --------------------------------------

    #[test]
    fn rule_d_scene_and_layout_errors() {
        let input = r#"scene "x" { }
layout { }"#;
        let err = preprocess_file_shape(input, &p())
            .expect_err("scene + layout must error as ambiguous");
        assert_eq!(err.code_enum(), ErrorCode::AmbiguousFileShape);
    }

    #[test]
    fn rule_d_snapshot_error_display() {
        let input = r#"scene "x" { }
layout { }"#;
        let err = preprocess_file_shape(input, &p()).expect_err("must error");
        assert_snapshot!("ambiguous_file_shape_error", err.to_string());
    }

    // -- KDL parse failure ---------------------------------------------------

    #[test]
    fn invalid_kdl_surfaces_parse_error() {
        let input = r#"scene "unterminated"#;
        let err = preprocess_file_shape(input, &p()).expect_err("bad KDL must error");
        assert_eq!(err.code_enum(), ErrorCode::Parse);
    }

    // -- Integration: preprocess then parse_scene ----------------------------

    #[test]
    fn full_pipeline_scene_file() {
        let input = r#"scene "hello" {
    layout {
        tab "work" {
            pane
        }
    }
}"#;
        let shape = preprocess_file_shape(input, &p()).expect("preprocess");
        let ir = crate::parse::parse_scene(shape.as_str(), &p()).expect("parse");
        assert_eq!(ir.scene.name, "hello");
    }

    #[test]
    fn full_pipeline_legacy_layout() {
        let input = r#"layout {
    tab "logs" {
        pane name="tail"
    }
}"#;
        let shape = preprocess_file_shape(input, &p()).expect("preprocess");
        let ir = crate::parse::parse_scene(shape.as_str(), &p()).expect("parse");
        assert_eq!(ir.scene.name, "default");
        let layout = ir.scene.layout.as_ref().expect("layout");
        assert_eq!(layout.tabs[0].name.as_deref(), Some("logs"));
    }
}
