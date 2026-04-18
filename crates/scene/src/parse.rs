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

    // Pre-parse normalization: convert KDL boolean property values to
    // strings so facet-kdl (which cannot deserialize booleans into
    // `Option<String>` fields) sees them as strings. This covers
    // `focus=true`, `sticky=true`, and any future bool-valued property
    // on AST nodes typed as `Option<String>`.
    let normalized = normalize_bool_properties(&src);

    // Primary parse: facet-kdl -> typed AST.
    let doc = match facet_kdl::from_str::<SceneDoc>(&normalized) {
        Ok(doc) => doc,
        Err(err) => return Err(kdl_err_to_scene_error(err, &src, &path)),
    };

    // Secondary parse: raw KDL document for formatter round-trip.
    // Errors here are non-fatal — the typed parse succeeded, so the source
    // is valid scene KDL. The upstream `kdl` crate may reject edge cases
    // that facet-kdl accepts; we simply drop the document in that case.
    let kdl_doc = kdl::KdlDocument::parse(&src).ok();

    // F-0005: facet-kdl silently ignores extra top-level `scene` nodes
    // (the `#[facet(kdl::child)]` field on `SceneDoc` picks the first
    // and discards the rest). Catch this explicitly so the user sees a
    // clear diagnostic rather than a silent data loss.
    if let Some(ref raw) = kdl_doc {
        let scene_count = raw
            .nodes()
            .iter()
            .filter(|n| n.name().value() == "scene")
            .count();
        if scene_count > 1 {
            return Err(SceneError::Parse {
                message: format!(
                    "expected exactly one top-level `scene` node, found {scene_count}"
                ),
                src: NamedSource::new(path.display().to_string(), src.clone()),
                span: SourceSpan::new(0.into(), src.len().min(1)),
            });
        }
    }

    Ok(SceneIR {
        scene: doc.scene,
        path,
        src,
        id,
        kdl_doc,
    })
}

/// Walk a KDL document and coerce every boolean property value to its
/// string equivalent (`true` → `"true"`, `false` → `"false"`). Returns
/// the round-tripped source when any coercion was applied, or the
/// original source when parsing fails or nothing changes.
fn normalize_bool_properties(src: &str) -> String {
    let Ok(mut doc) = kdl::KdlDocument::parse(src) else {
        return src.to_string();
    };
    let changed = normalize_doc_bools(&mut doc);
    if changed {
        doc.to_string()
    } else {
        src.to_string()
    }
}

/// Recursively walk a `KdlDocument`, coercing boolean entries to strings.
/// Returns `true` if at least one entry was modified.
fn normalize_doc_bools(doc: &mut kdl::KdlDocument) -> bool {
    let mut changed = false;
    for node in doc.nodes_mut() {
        changed |= normalize_node_bools(node);
    }
    changed
}

/// Coerce boolean entries on a single node + recurse into its children.
fn normalize_node_bools(node: &mut kdl::KdlNode) -> bool {
    let mut changed = false;
    for entry in node.entries_mut() {
        if let kdl::KdlValue::Bool(b) = entry.value() {
            let s = if *b { "true" } else { "false" };
            entry.set_value(kdl::KdlValue::String(s.to_string()));
            // Clear cached formatting so `to_string()` re-renders
            // the value from the new `KdlValue::String` rather than
            // the stale `value_repr` that still says `#true`.
            entry.clear_format();
            changed = true;
        }
    }
    if let Some(children) = node.children_mut() {
        changed |= normalize_doc_bools(children);
    }
    changed
}

/// Convert a `facet_kdl::KdlDeserializeError` into `SceneError::Parse`.
fn kdl_err_to_scene_error(
    err: facet_kdl::KdlDeserializeError,
    src: &str,
    path: &std::path::Path,
) -> SceneError {
    let message = err.to_string();
    let span =
        first_label_span(&err).unwrap_or_else(|| SourceSpan::new(0.into(), src.len().min(1)));

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
        let ir = parse_scene(r#"scene "x" { }"#, "test.kdl").expect("minimal scene should parse");
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

    /// F-0004: `focus=#true` (KDL v2 boolean literal) must parse into the
    /// `Option<String>` field via the boolean-to-string normalization pass.
    /// KDL v2 uses `#true` / `#false` — bare `true` is a plain identifier,
    /// not a boolean.
    #[test]
    fn focus_bool_literal_parses_to_string() {
        let src = r#"
scene "dev" {
    layout {
        tab "@main" focus=#true {
            pane "@editor"
        }
    }
}
"#;
        let ir =
            parse_scene(src, "focus_bool.kdl").expect("focus=#true (KDL v2 boolean) should parse");
        // Extract the tab and verify `focus` is populated.
        let layout = ir
            .scene
            .body
            .iter()
            .find_map(|n| {
                if let crate::ast::SceneBodyNode::Layout(l) = n {
                    Some(l)
                } else {
                    None
                }
            })
            .expect("layout present");
        let tab = &layout.tabs[0];
        assert_eq!(
            tab.focus.as_deref(),
            Some("true"),
            "focus=#true should normalize to Some(\"true\")"
        );
    }

    /// F-0004: `focus=#false` must also parse and normalize.
    #[test]
    fn focus_false_bool_literal_parses() {
        let src = r#"
scene "dev" {
    layout {
        tab "@main" focus=#false {
            pane "@editor"
        }
    }
}
"#;
        let ir = parse_scene(src, "focus_false.kdl").expect("focus=#false should parse");
        let layout = ir
            .scene
            .body
            .iter()
            .find_map(|n| {
                if let crate::ast::SceneBodyNode::Layout(l) = n {
                    Some(l)
                } else {
                    None
                }
            })
            .expect("layout present");
        let tab = &layout.tabs[0];
        assert_eq!(
            tab.focus.as_deref(),
            Some("false"),
            "focus=#false should normalize to Some(\"false\")"
        );
    }

    /// KDL v2 uses `#true` / `#false` for boolean literals (not bare
    /// `true` / `false`). Verify kdl 6.5 accepts `focus=#true`.
    #[test]
    fn kdl_crate_parses_bool_property() {
        let src = r#"tab "@main" focus=#true { }"#;
        let doc = kdl::KdlDocument::parse(src);
        assert!(doc.is_ok(), "kdl 6.5 should parse #true property: {doc:?}");
    }

    /// F-0005: a KDL file with two `scene` nodes must produce a parse error.
    /// `SceneDoc` uses `#[facet(kdl::child)]` (singular) — facet-kdl should
    /// reject duplicate `scene` nodes at the top level.
    #[test]
    fn rejects_multiple_scene_nodes() {
        let src = r#"
scene "first" { }
scene "second" { }
"#;
        let result = parse_scene(src, "multi.kdl");
        assert!(
            result.is_err(),
            "two top-level `scene` nodes should produce an error"
        );
    }

    /// Verify the normalization pass converts `#true` to `"true"` string.
    #[test]
    fn normalize_bool_properties_converts_bools() {
        let src = r#"scene "dev" { layout { tab "@main" focus=#true { } } }"#;
        // First verify kdl can parse the full document.
        let parse_result = kdl::KdlDocument::parse(src);
        assert!(
            parse_result.is_ok(),
            "kdl should parse full scene with #true: {parse_result:?}"
        );
        let normalized = normalize_bool_properties(src);
        assert!(
            !normalized.contains("#true"),
            "normalized should not contain #true: {normalized}"
        );
        assert!(
            normalized.contains("\"true\""),
            "normalized should contain string \"true\": {normalized}"
        );
    }

    #[test]
    fn scene_id_reflects_content() {
        let ir = parse_scene(r#"scene "a" { }"#, "a.kdl").unwrap();
        let id2 = SceneId::new("a.kdl", br#"scene "a" { }"#);
        assert_eq!(ir.id, id2);
    }
}
