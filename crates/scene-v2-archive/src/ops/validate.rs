//! Op cross-reference validation (T-4.3).
//!
//! Ops that carry named references to tabs / plugins are validated at
//! scene-compile time against the declarations in the same scene:
//!
//! * `split_pane into="<X>"` — `<X>` must appear as
//!   `layout { tab name="<X>" ... }`.
//! * `pipe plugin="<Y>"` — `<Y>` must appear as a top-level
//!   `plugin "<Y>" { ... }` block.
//! * `mount_plugin name="<Z>"` / `unmount_plugin name="<Z>"` — same
//!   constraint as `pipe`.
//!
//! Type / presence checks come for free via facet-kdl (T-4.2 ops parse
//! into typed Args) — this pass handles the cross-file / cross-node
//! constraints that facet-kdl can't see.
//!
//! Unresolved refs surface as
//! [`SceneError::OpUnresolvedRef`][crate::error::SceneError::OpUnresolvedRef]
//! (code `op/unresolved-ref`) with a "did you mean …?" suggestion
//! computed via [`crate::suggest::suggest_similar`].
//!
//! # Walker choice: raw KDL, not the typed AST
//!
//! The typed AST ([`crate::ast`]) encodes ops as an opaque `OpNode`
//! bag today (see TODO(T-3.2) in `ast.rs`). So we walk the raw
//! `kdl::KdlDocument` directly — the same pattern `crates/scene/src/scope.rs`
//! already uses — extracting op node names + argument spans via
//! `KdlNode::span()` / `KdlEntry::span()` for precise diagnostics.
//!
//! When the AST grows a typed op enum (T-3.2 eventually), this file
//! switches to iterating that enum; the public surface (`validate_op_refs`
//! + `SceneError::OpUnresolvedRef`) stays stable.

use std::collections::BTreeSet;
use std::path::Path;

use kdl::{KdlDocument, KdlEntry, KdlNode};
use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;
use crate::suggest::suggest_similar;

/// Op name → `(attribute, kind)` list of `(attr, kind)` pairs to
/// cross-reference.
///
/// `attr` is the KDL property name we read (e.g. `"into"`); `kind` tells
/// us which declared-name set to look the value up in (`"tab"` / `"plugin"`).
///
/// Kept as a small const table so adding a new ref-carrying op is a
/// one-line edit. The entries mirror the T-4.2 op vocabulary; op names
/// are the ark-internal KDL verb, not the namespaced `ark.core.*` form
/// (the KDL source uses the short form).
const OP_REF_TABLE: &[(&str, &[(&str, &str)])] = &[
    ("split_pane", &[("into", "tab")]),
    ("pipe", &[("plugin", "plugin")]),
    ("mount_plugin", &[("name", "plugin")]),
    ("unmount_plugin", &[("name", "plugin")]),
];

/// Validate op cross-references in a scene source document.
///
/// Takes the raw file source + its parsed `KdlDocument` (callers
/// already have both — the compile pipeline parses once and shares the
/// document across passes). Returns the full set of unresolved-ref
/// diagnostics; the walk never short-circuits, so `ark scene check`
/// can render them all in one go.
///
/// # Invariants
///
/// * `source` must be the KDL text the document was parsed from;
///   `NamedSource` re-clones the bytes for each diagnostic so miette
///   can render the caret.
/// * `src_name` is the file path (or synthetic name) used in the
///   rendered diagnostic header.
pub fn validate_op_refs(
    source: &str,
    src_name: &Path,
    doc: &KdlDocument,
) -> Result<(), Vec<SceneError>> {
    let mut errors = Vec::new();
    let decls = collect_declarations(doc);
    let scene_body = scene_body(doc);
    if let Some(body) = scene_body {
        for node in body.nodes() {
            match node.name().value() {
                "on" | "keybind" => walk_op_body(
                    node.children(),
                    &decls,
                    source,
                    src_name,
                    &mut errors,
                ),
                _ => {}
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Declarations collected from the scene for the cross-reference check.
#[derive(Debug, Default)]
pub struct Declarations {
    /// Tab names declared in `layout { tab name="<X>" }` (including
    /// nested `tab` inside `pane`, since zellij allows it).
    pub tabs: BTreeSet<String>,
    /// Plugin names declared in `plugin "<Y>" { … }` at the scene root.
    pub plugins: BTreeSet<String>,
}

impl Declarations {
    /// Return the set matching the `kind` string from the [`OP_REF_TABLE`].
    fn get(&self, kind: &str) -> &BTreeSet<String> {
        match kind {
            "tab" => &self.tabs,
            "plugin" => &self.plugins,
            _ => panic!("unknown ref kind {kind:?}"),
        }
    }
}

/// Enter the `scene { … }` body if present. v1 scenes always have the
/// wrapper (R1), but R15 also accepts a bare `layout { }` at top level;
/// for that path we just look at the document root directly since the
/// op-carrying nodes (`on`, `keybind`) can't live there anyway —
/// returning `None` is correct and the walk becomes a no-op.
fn scene_body(doc: &KdlDocument) -> Option<&KdlDocument> {
    let scene = doc.nodes().iter().find(|n| n.name().value() == "scene")?;
    scene.children()
}

/// Collect tab + plugin declarations from a parsed scene.
fn collect_declarations(doc: &KdlDocument) -> Declarations {
    let mut out = Declarations::default();
    let Some(body) = scene_body(doc) else {
        return out;
    };
    for node in body.nodes() {
        match node.name().value() {
            "layout" => {
                if let Some(children) = node.children() {
                    collect_layout_tabs(children, &mut out.tabs);
                }
            }
            "plugin" => {
                // `plugin "<name>" { … }` — first positional argument.
                if let Some(first) = node.entries().iter().find(|e| e.name().is_none()) {
                    if let Some(s) = first.value().as_string() {
                        out.plugins.insert(s.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Recurse through `layout { }` collecting `tab name="<X>"` declarations,
/// including nested tabs under `pane` (zellij permits it).
fn collect_layout_tabs(layout: &KdlDocument, tabs: &mut BTreeSet<String>) {
    for node in layout.nodes() {
        match node.name().value() {
            "tab" => {
                // Prefer `name="<X>"` when present; else the first
                // positional argument per `TabNode::name` contract.
                if let Some(name) = tab_name(node) {
                    tabs.insert(name);
                }
                if let Some(children) = node.children() {
                    collect_layout_tabs(children, tabs);
                }
            }
            "pane" => {
                if let Some(children) = node.children() {
                    collect_layout_tabs(children, tabs);
                }
            }
            _ => {}
        }
    }
}

/// Extract a tab's name from either the `name="X"` property or the
/// first positional argument (facet-kdl accepts both per
/// `TabNode::name`).
fn tab_name(node: &KdlNode) -> Option<String> {
    if let Some(entry) = node.entries().iter().find(|e| {
        e.name()
            .map(|n| n.value() == "name")
            .unwrap_or(false)
    }) {
        return entry.value().as_string().map(|s| s.to_string());
    }
    // First positional.
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string().map(|s| s.to_string()))
}

/// Walk an op-body child list — the contents of an `on { … }` or
/// `keybind { … }` block — flagging every ref-carrying op whose
/// attribute points at an unknown declaration.
fn walk_op_body(
    body: Option<&KdlDocument>,
    decls: &Declarations,
    source: &str,
    src_name: &Path,
    errors: &mut Vec<SceneError>,
) {
    let Some(body) = body else {
        return;
    };
    for node in body.nodes() {
        let op_name = node.name().value();
        // Look up the op's ref table (short-verb form).
        let Some((_, attrs)) = OP_REF_TABLE.iter().find(|(n, _)| *n == op_name) else {
            continue;
        };
        for (attr, kind) in *attrs {
            if let Some(entry) = find_property(node, attr) {
                let Some(value) = entry.value().as_string() else {
                    continue;
                };
                let known = decls.get(kind);
                if known.contains(value) {
                    continue;
                }
                let available: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
                let suggestion = suggest_similar(value, &available).into_iter().next();
                let at = entry_value_span(entry).unwrap_or_else(|| SourceSpan::new(0.into(), 0));
                let named = NamedSource::new(
                    src_name.display().to_string(),
                    source.to_string(),
                );
                let full_op_name = format!("ark.core.{op_name}");
                errors.push(SceneError::op_unresolved_ref(
                    full_op_name,
                    *kind,
                    value.to_string(),
                    suggestion,
                    &available,
                    named,
                    at,
                ));
            }
        }
    }
}

/// Find a `name="value"` property entry on a KDL node, by property name.
fn find_property<'a>(node: &'a KdlNode, name: &str) -> Option<&'a KdlEntry> {
    node.entries().iter().find(|e| {
        e.name()
            .map(|n| n.value() == name)
            .unwrap_or(false)
    })
}

/// Span covering a property entry's value. KDL 2.0 exposes per-entry
/// spans via `KdlEntry::span()`; the value span isn't separately
/// addressable, so we use the full entry span. Good enough for the
/// caret to point at `attr="value"`.
fn entry_value_span(entry: &KdlEntry) -> Option<SourceSpan> {
    let span = entry.span();
    Some(SourceSpan::new(span.offset().into(), span.len()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use miette::Diagnostic;
    use std::path::PathBuf;

    fn parse(src: &str) -> KdlDocument {
        src.parse().expect("test fixture is valid KDL")
    }

    /// Helper: collect rendered `(code, help)` pairs for quick assertions.
    fn into_code(err: &SceneError) -> String {
        err.code().map(|c| c.to_string()).unwrap_or_default()
    }

    // -- declarations collection ----------------------------------------

    #[test]
    fn collect_layout_tabs_finds_direct_and_nested() {
        let src = r#"
scene "s" {
    layout {
        tab name="work" {
            pane
            tab name="nested"
        }
        tab name="logs"
    }
}
"#;
        let doc = parse(src);
        let d = collect_declarations(&doc);
        assert!(d.tabs.contains("work"));
        assert!(d.tabs.contains("nested"));
        assert!(d.tabs.contains("logs"));
    }

    #[test]
    fn collect_plugins_returns_root_names() {
        let src = r#"
scene "s" {
    plugin "picker" { source "shipped:picker" }
    plugin "status" { source "shipped:status" }
}
"#;
        let doc = parse(src);
        let d = collect_declarations(&doc);
        assert!(d.plugins.contains("picker"));
        assert!(d.plugins.contains("status"));
    }

    // -- happy path: all refs resolve ------------------------------------

    #[test]
    fn resolved_refs_pass() {
        let src = r#"
scene "s" {
    layout {
        tab name="work" { pane }
    }
    plugin "picker" { source "shipped:picker" }
    on "AgentReady" {
        split_pane into="work" side="right"
        pipe plugin="picker" { text "hi" }
        mount_plugin name="picker"
        unmount_plugin name="picker"
    }
}
"#;
        let doc = parse(src);
        validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc).expect("all refs resolve");
    }

    // -- fixture 1: split_pane → missing tab ----------------------------

    #[test]
    fn split_pane_into_missing_tab_reports_op_unresolved_ref() {
        let src = r#"
scene "s" {
    layout {
        tab name="work" { pane }
    }
    on "AgentReady" {
        split_pane into="wokr" side="right"
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("should flag missing tab");
        assert_eq!(errs.len(), 1);
        assert_eq!(into_code(&errs[0]), "op/unresolved-ref");
        assert_eq!(errs[0].code_enum(), ErrorCode::OpUnresolvedRef);
        // Typo `wokr` → suggest `work`.
        match &errs[0] {
            SceneError::OpUnresolvedRef {
                op,
                kind,
                name,
                suggestion,
                ..
            } => {
                assert_eq!(op, "ark.core.split_pane");
                assert_eq!(kind, "tab");
                assert_eq!(name, "wokr");
                assert_eq!(suggestion.as_deref(), Some("work"));
            }
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
        // Rendered help includes "did you mean `work`?" + the available list.
        let help = errs[0]
            .help()
            .map(|h| h.to_string())
            .unwrap_or_default();
        assert!(help.contains("did you mean `work`?"), "help: {help:?}");
        assert!(help.contains("Available tabs: work"), "help: {help:?}");
    }

    // -- fixture 2: pipe → missing plugin --------------------------------

    #[test]
    fn pipe_plugin_missing_reports_op_unresolved_ref() {
        let src = r#"
scene "s" {
    plugin "status" { source "shipped:status" }
    on "AgentReady" {
        pipe plugin="ststus" { text "hi" }
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("should flag missing plugin");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            SceneError::OpUnresolvedRef {
                op,
                kind,
                name,
                suggestion,
                ..
            } => {
                assert_eq!(op, "ark.core.pipe");
                assert_eq!(kind, "plugin");
                assert_eq!(name, "ststus");
                assert_eq!(suggestion.as_deref(), Some("status"));
            }
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
    }

    // -- fixture 3: mount_plugin → missing plugin -----------------------

    #[test]
    fn mount_plugin_missing_reports_op_unresolved_ref() {
        let src = r#"
scene "s" {
    plugin "picker" { source "shipped:picker" }
    on "AgentReady" {
        mount_plugin name="pickre"
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("should flag missing plugin");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            SceneError::OpUnresolvedRef {
                op,
                kind,
                name,
                suggestion,
                ..
            } => {
                assert_eq!(op, "ark.core.mount_plugin");
                assert_eq!(kind, "plugin");
                assert_eq!(name, "pickre");
                assert_eq!(suggestion.as_deref(), Some("picker"));
            }
            other => panic!("expected OpUnresolvedRef, got {other:?}"),
        }
    }

    // -- keybind body walked too ----------------------------------------

    #[test]
    fn keybind_body_is_walked() {
        let src = r#"
scene "s" {
    keybind "Alt p" {
        mount_plugin name="nonesuch"
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("keybind op must be checked");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::OpUnresolvedRef);
    }

    // -- no declarations at all: renders a 'no plugins declared' help line.

    #[test]
    fn empty_declarations_help_text_is_useful() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        mount_plugin name="x"
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("no plugins declared");
        let help = errs[0].help().map(|h| h.to_string()).unwrap_or_default();
        assert!(
            help.contains("No plugins are declared"),
            "help: {help:?}"
        );
    }

    // -- multiple errors accumulate -------------------------------------

    #[test]
    fn multiple_unresolved_refs_accumulate_in_one_pass() {
        let src = r#"
scene "s" {
    layout { tab name="work" { pane } }
    plugin "picker" { source "shipped:picker" }
    on "AgentReady" {
        split_pane into="missing1" side="right"
        pipe plugin="missing2" { text "hi" }
    }
}
"#;
        let doc = parse(src);
        let errs = validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("both are unresolved");
        assert_eq!(errs.len(), 2);
    }

    // -- non-ref-carrying ops don't get flagged --------------------------

    #[test]
    fn non_ref_ops_are_ignored() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        exec script="echo hi"
        emit "user.tick"
        set_status text="ok"
    }
}
"#;
        let doc = parse(src);
        validate_op_refs(src, &PathBuf::from("scene.kdl"), &doc)
            .expect("ref-less ops pass");
    }
}
