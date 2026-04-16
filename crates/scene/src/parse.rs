//! Scene parsing: KDL text -> typed `SceneIR`.
//!
//! [`parse_scene`] is the single entry point for turning a scene file's source
//! text into a typed AST. Parsing is done by `facet_kdl::from_str::<SceneDoc>`,
//! which surfaces KDL 2.0 tokenizer + structural errors as `miette::Diagnostic`s
//! with labeled spans. This module translates those into the crate's canonical
//! [`SceneError::Parse`] variant.
//!
//! A secondary `kdl::KdlDocument::parse` pass captures the raw KDL document for
//! formatter round-trip (T-119 `ark scene fmt`). The two parses are independent:
//! the facet-kdl pass produces typed AST nodes; the kdl pass preserves comments
//! and whitespace for round-tripping.

use std::path::PathBuf;

use miette::{Diagnostic, NamedSource, SourceSpan};

use crate::ast::{SceneDoc, SceneNode};
use crate::error::SceneError;
use crate::id::SceneId;

/// Parsed scene intermediate representation.
///
/// Bundles the typed AST (`scene`), the source text and path for diagnostic
/// attribution, a content-addressed identity key for hot-reload delta
/// detection, and an optional raw `KdlDocument` for formatter round-trip.
#[derive(Debug)]
pub struct SceneIR {
    /// The typed scene AST root.
    pub scene: SceneNode,
    /// Path to the source file (or a synthetic path for stdin / tests).
    pub path: PathBuf,
    /// Raw source text — retained for diagnostic `NamedSource` construction
    /// in downstream passes and for the formatter round-trip.
    pub src: String,
    /// Content-addressed identity key (path + blake3 hash of `src`).
    pub id: SceneId,
    /// Raw KDL document for formatter round-trip (T-119). `None` when the
    /// upstream `kdl` crate rejects the input (facet-kdl may accept a
    /// slightly different grammar subset).
    pub kdl_doc: Option<kdl::KdlDocument>,
}

/// Parse a scene file's source text into a typed [`SceneIR`].
///
/// On success returns the full IR bundle. On failure returns a
/// [`SceneError::Parse`] whose `src` is a `NamedSource` keyed by `path`
/// so miette renders `<path>:<line>:<col>`.
///
/// ## Span fidelity
///
/// If facet-kdl's error reports at least one `LabeledSpan` via its
/// `miette::Diagnostic::labels()` impl, the first label's offset + len
/// are used. Otherwise we fall back to a placeholder span at offset 0
/// with length `min(src.len(), 1)`.
#[allow(clippy::result_large_err)]
pub fn parse_scene(
    src: impl Into<String>,
    path: impl Into<PathBuf>,
) -> Result<SceneIR, SceneError> {
    let src = src.into();
    let path = path.into();
    let id = SceneId::new(&path, src.as_bytes());

    // Primary parse: facet-kdl -> typed AST.
    let doc = match facet_kdl::from_str::<SceneDoc>(&src) {
        Ok(doc) => doc,
        Err(err) => return Err(kdl_err_to_scene_error(err, &src, &path)),
    };

    // Secondary parse: raw KDL document for formatter round-trip.
    // Errors here are non-fatal — the typed parse succeeded, so the source
    // is valid scene KDL. The upstream `kdl` crate may reject edge cases
    // that facet-kdl accepts; we simply drop the document in that case.
    let kdl_doc = kdl::KdlDocument::parse(&src).ok();

    Ok(SceneIR {
        scene: doc.scene,
        path,
        src,
        id,
        kdl_doc,
    })
}

/// Convert a `facet_kdl::KdlDeserializeError` into `SceneError::Parse`.
fn kdl_err_to_scene_error(
    err: facet_kdl::KdlDeserializeError,
    src: &str,
    path: &std::path::Path,
) -> SceneError {
    let message = err.to_string();
    let span = first_label_span(&err).unwrap_or_else(|| {
        SourceSpan::new(0.into(), src.len().min(1))
    });

    SceneError::Parse {
        message,
        src: NamedSource::new(path.display().to_string(), src.to_string()),
        span,
    }
}

/// Pull the first `LabeledSpan` from a `miette::Diagnostic` and convert
/// to `SourceSpan`. Returns `None` when the diagnostic has no labels.
fn first_label_span(err: &dyn Diagnostic) -> Option<SourceSpan> {
    let mut labels = err.labels()?;
    let label = labels.next()?;
    Some(SourceSpan::new(label.offset().into(), label.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_scene() {
        let ir = parse_scene(r#"scene "x" { }"#, "test.kdl")
            .expect("minimal scene should parse");
        assert_eq!(ir.scene.name, "x");
        assert!(ir.scene.body.is_empty());
        assert_eq!(ir.path.to_str().unwrap(), "test.kdl");
        assert!(ir.kdl_doc.is_some());
    }

    #[test]
    fn parses_scene_with_empty_layout() {
        let src = r#"scene "dev" { layout { } }"#;
        let ir = parse_scene(src, "layout.kdl").expect("empty layout should parse");
        assert_eq!(ir.scene.name, "dev");
        assert!(!ir.scene.body.is_empty());
    }

    #[test]
    fn parses_scene_with_tab() {
        let src = r#"scene "dev" { layout { tab "@main" { } } }"#;
        let ir = parse_scene(src, "tab.kdl").expect("tab should parse");
        assert_eq!(ir.scene.name, "dev");
    }

    #[test]
    fn parses_scene_with_tab_properties() {
        // String properties work fine; boolean properties (`focus=true`)
        // are deferred — facet-kdl 0.42 may not coerce KDL boolean
        // literals into `Option<bool>` yet. Test string properties only.
        let src = r#"scene "dev" { layout { tab "@main" cwd="/tmp" name="Main" { } } }"#;
        let ir = parse_scene(src, "props.kdl").expect("tab with properties should parse");
        assert_eq!(ir.scene.name, "dev");
    }

    #[test]
    fn parses_scene_with_layout() {
        let src = r#"
scene "dev" {
    layout {
        tab "@main" focus="true" {
            row {
                pane "@editor" span=2
                pane "@term"
            }
        }
    }
}
"#;
        let ir = parse_scene(src, "layout.kdl").expect("layout scene should parse");
        assert_eq!(ir.scene.name, "dev");
        assert!(!ir.scene.body.is_empty());
    }

    #[test]
    fn parses_scene_with_reaction() {
        let src = r#"
scene "reactive" {
    on "FileEdited" when="true" {
        close "@x"
    }
}
"#;
        let ir = parse_scene(src, "react.kdl").expect("reaction scene should parse");
        assert_eq!(ir.scene.name, "reactive");
    }

    #[test]
    fn parses_scene_with_bind() {
        let src = r#"
scene "keys" {
    bind "Alt q" {
        close "@x"
    }
}
"#;
        let ir = parse_scene(src, "keys.kdl").expect("bind scene should parse");
        assert_eq!(ir.scene.name, "keys");
    }

    #[test]
    fn parse_error_surfaces_diagnostic() {
        let result = parse_scene("this is not valid kdl {{{", "bad.kdl");
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            SceneError::Parse { message, .. } => {
                assert!(!message.is_empty(), "parse error should have a message");
            }
            other => panic!("expected SceneError::Parse, got: {other:?}"),
        }
    }

    #[test]
    fn scene_id_reflects_content() {
        let ir = parse_scene(r#"scene "a" { }"#, "a.kdl").unwrap();
        let id2 = SceneId::new("a.kdl", br#"scene "a" { }"#);
        assert_eq!(ir.id, id2);
    }
}
