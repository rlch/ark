//! Pane view-child arity validation (T-032).
//!
//! R3 requires exactly one view child per pane. Zero view children means
//! the pane has nothing to render; two or more means the grammar is
//! ambiguous about which view fills the pane.
//!
//! This pass walks the raw `KdlDocument` retained in [`SceneIR::kdl_doc`]
//! rather than the typed AST. The typed [`PaneNode::view`] field is a
//! single `ViewRef` — it cannot represent "zero" or "multiple" in its
//! shape. The parse-time population of `ViewRef` from the pane's single
//! view child is T-026+'s responsibility, so at this layer the raw KDL
//! is the ground truth for source-level arity.
//!
//! ## Detection strategy
//!
//! 1. Recursively walk every `pane` node in the document (inside
//!    `layout` / `mode` / nested `row` / `col`).
//! 2. Count each pane's direct children. Every direct child of a pane is
//!    treated as a view candidate — R3 admits no other child shape inside
//!    a pane at the source level.
//! 3. `count == 0` → `pane has no view child` (misplaced-node error).
//! 4. `count >= 2` → `pane has multiple view children`.
//!
//! ## Why not walk `SceneIR::scene`?
//!
//! The typed `PaneNode` only carries a `ViewRef` populated by a later
//! pass; its "default" state (empty alias) is indistinguishable from a
//! legitimately empty source pane once view resolution lands. Running
//! this validator off the raw KDL keeps its output stable regardless of
//! whether the T-026+ ViewRef population has run.

use kdl::{KdlDocument, KdlNode};
use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;
use crate::parse::SceneIR;

/// Validate that every pane has exactly one view child.
///
/// Returns an empty `Vec` when every pane is well-formed. Returns one
/// [`SceneError::MisplacedNode`] per offending pane otherwise — the
/// `node` field carries `"view"` (for the missing or extra entry) and
/// the `parent` field carries `"pane"` for uniform diagnostic shape.
pub fn validate_pane_views(ir: &SceneIR) -> Vec<SceneError> {
    let mut errors = Vec::new();
    let path = ir.path.display().to_string();

    let Some(doc) = ir.kdl_doc.as_ref() else {
        // No raw document (upstream kdl crate rejected the input even
        // though facet-kdl accepted it). Other passes will have
        // surfaced parse errors; skip silently.
        return errors;
    };

    walk_document(doc, &ir.src, &path, &mut errors);
    errors
}

/// Recursively walk every node in a KDL document looking for `pane`
/// nodes to validate.
fn walk_document(doc: &KdlDocument, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    for node in doc.nodes() {
        walk_node(node, src, path, errors);
    }
}

/// Recurse into a single KDL node. When the node's name is `pane`,
/// validate its view-child arity. Either way, descend into any child
/// document so nested panes (inside `row` / `col` / `tab` / `layout` /
/// `mode`) are reached.
fn walk_node(node: &KdlNode, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    if node.name().value() == "pane" {
        validate_pane(node, src, path, errors);
    }

    if let Some(children) = node.children() {
        walk_document(children, src, path, errors);
    }
}

/// Validate a single `pane` node's view-child arity.
///
/// A pane with `count == 0` view children emits "pane has no view
/// child"; `count >= 2` emits "pane has multiple view children". The
/// error span points at the pane's own head so miette's caret lands on
/// the `pane @handle` site.
fn validate_pane(pane: &KdlNode, src: &str, path: &str, errors: &mut Vec<SceneError>) {
    let count = pane.children().map(|d| d.nodes().len()).unwrap_or(0);

    if count == 1 {
        return;
    }

    let message = if count == 0 {
        "pane has no view child"
    } else {
        "pane has multiple view children"
    };

    errors.push(SceneError::MisplacedNode {
        node: message.to_string(),
        parent: "pane".to_string(),
        src: NamedSource::new(path.to_string(), src.to_string()),
        span: pane_span(pane),
    });
}

/// Extract a usable `SourceSpan` from a pane node's raw KDL span.
fn pane_span(pane: &KdlNode) -> SourceSpan {
    let span = pane.span();
    SourceSpan::new(span.offset().into(), span.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn parse(src: &str) -> SceneIR {
        parse_scene(src, "test.kdl").expect("fixture should parse")
    }

    #[test]
    fn valid_pane_with_single_view_passes() {
        let src = r#"
scene "x" {
    layout {
        tab "@main" {
            pane "@editor" {
                command cmd="nvim"
            }
        }
    }
}
"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert!(
            errors.is_empty(),
            "single-view pane should pass; got {errors:?}"
        );
    }

    #[test]
    fn pane_with_no_view_rejected() {
        let src = r#"
scene "x" {
    layout {
        tab "@main" {
            pane "@editor"
        }
    }
}
"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::MisplacedNode { node, parent, .. } => {
                assert!(node.contains("no view"));
                assert_eq!(parent, "pane");
            }
            other => panic!("expected MisplacedNode, got {other:?}"),
        }
    }

    #[test]
    fn pane_with_multiple_views_rejected() {
        let src = r#"
scene "x" {
    layout {
        tab "@main" {
            pane "@editor" {
                command cmd="nvim"
                shell
            }
        }
    }
}
"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            SceneError::MisplacedNode { node, parent, .. } => {
                assert!(node.contains("multiple"));
                assert_eq!(parent, "pane");
            }
            other => panic!("expected MisplacedNode, got {other:?}"),
        }
    }

    #[test]
    fn nested_panes_under_row_walked() {
        // A pane two layers deep (tab > row > pane) is still validated.
        let src = r#"
scene "x" {
    layout {
        tab "@main" {
            row {
                pane "@editor"
                pane "@term" {
                    shell
                }
            }
        }
    }
}
"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert_eq!(errors.len(), 1, "one offending pane; got {errors:?}");
    }

    #[test]
    fn panes_in_mode_also_walked() {
        // Mode blocks carry panes too — the walker must descend into them.
        let src = r#"
scene "x" {
    mode "review" {
        tab "@main" {
            pane "@editor"
        }
    }
}
"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn no_panes_no_errors() {
        let src = r#"scene "empty" { }"#;
        let ir = parse(src);
        let errors = validate_pane_views(&ir);
        assert!(errors.is_empty());
    }
}
