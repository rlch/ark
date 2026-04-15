//! Lower a scene [`LayoutNode`] AST into a zellij-compatible KDL string.
//!
//! Implements `cavekit-scene.md` R3 (layout compile) and the spawn-time
//! slice of R15 (scene-vs-legacy layout file shape). The compiler builds
//! the KDL document via [`kdl::KdlDocument`] / [`kdl::KdlNode`] — the
//! upstream builder API guarantees correct escaping, quoting, and
//! indentation without hand-rolled string concatenation, and the
//! resulting document self-validates by round-tripping through
//! [`kdl::KdlDocument::parse`] before we return.
//!
//! # Pass-through vs strip
//!
//! Every zellij-owned attribute on `tab` / `pane` is passed through
//! unchanged — `name`, `command`, `args` (surface-equivalent via pane
//! body; see note on [`AST PaneNode`](crate::ast::PaneNode)), `size`,
//! `split_direction`, `focus`, `cwd`. Ark-owned attributes that mean
//! nothing to zellij — `when=` — are stripped during lowering. Pruning
//! of `when=false` branches is T-3.2 (next commit): this module
//! unconditionally emits every branch for now, with `when=` simply
//! omitted from the rendered output.
//!
//! # Scope
//!
//! The v1 AST enumerates only `tab` and `pane` nodes (see
//! [`crate::ast::LayoutNode`] module docs). Additional zellij-native
//! nodes (`swap_tiled_layout`, `swap_floating_layout`, pane templates,
//! tab templates, `floating_panes`) surface in later tiers when the
//! AST grows. Until then, anything the scene author writes that isn't
//! in the v1 AST is already rejected at parse time, so this compiler
//! never sees it.
//!
//! # Error surface
//!
//! The compiler returns [`SceneError::Grammar`] if the rendered output
//! fails to re-parse (belt-and-suspenders check — in practice the
//! builder API never produces invalid KDL, but the guard is cheap
//! and catches upstream bugs early).

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use miette::NamedSource;

use crate::ast::{LayoutNode, PaneNode, TabNode};
use crate::error::SceneError;

/// Static compile-time context for `when=` predicate evaluation.
///
/// T-3.1 does not yet evaluate predicates — every branch is emitted
/// unconditionally. T-3.2 adds CEL evaluation against the agent /
/// session fields below.
///
/// Fields mirror the documented spawn-time CEL surface:
/// - `agent.{id, name, orchestrator, engine, cwd, cmd, args}`
/// - `session.{name}`
///
/// No `phase`, no `event`, no `payload` — those do not exist at spawn
/// time. Dynamic predicates belong in reaction `if=`, not layout `when=`.
#[derive(Clone, Debug)]
pub struct CompileContext {
    /// Agent snapshot bound to `agent.*` during CEL evaluation.
    pub agent: crate::context::AgentSnapshot,
    /// Session snapshot bound to `session.*` during CEL evaluation.
    pub session: crate::context::SessionSnapshot,
}

impl CompileContext {
    /// Convenience: build a context from typed snapshots.
    pub fn new(
        agent: crate::context::AgentSnapshot,
        session: crate::context::SessionSnapshot,
    ) -> Self {
        Self { agent, session }
    }
}

/// Lower a scene layout AST to a zellij-compatible KDL string.
///
/// The returned string is the full zellij layout document (wrapped
/// in the top-level `layout { }` node zellij expects). Output is
/// guaranteed to re-parse via [`kdl::KdlDocument::parse`] before
/// return.
///
/// `_ctx` is currently unused (see module-level note on T-3.2 pruning)
/// but is part of the stable public signature so callers can wire it
/// through without churn when the pruning pass lands.
pub fn compile_layout(
    layout: &LayoutNode,
    _ctx: &CompileContext,
) -> Result<String, SceneError> {
    let mut doc = KdlDocument::new();

    // Build the single outer `layout { … }` node.
    let mut layout_node = KdlNode::new("layout");
    let mut inner = KdlDocument::new();
    for tab in &layout.tabs {
        inner.nodes_mut().push(lower_tab(tab));
    }
    for pane in &layout.panes {
        inner.nodes_mut().push(lower_pane(pane));
    }
    layout_node.set_children(inner);
    doc.nodes_mut().push(layout_node);

    // Autoformat applies consistent indentation + newlines.
    doc.autoformat();
    let rendered = doc.to_string();

    // Belt-and-suspenders: the builder API guarantees valid KDL, but
    // re-parse so any future AST bug surfaces at compile time rather
    // than bubbling through to zellij as a vague parse error.
    KdlDocument::parse(&rendered).map_err(|e| SceneError::Grammar {
        message: format!("rendered layout failed to re-parse: {e}"),
        src: NamedSource::new("<compiled-layout>", rendered.clone()),
        at: (0, rendered.len().min(1)).into(),
    })?;

    Ok(rendered)
}

/// Lower a `TabNode` to a `KdlNode`. Ark-only `when=` is stripped.
fn lower_tab(tab: &TabNode) -> KdlNode {
    let mut node = KdlNode::new("tab");

    if let Some(name) = &tab.name {
        node.entries_mut().push(KdlEntry::new(name.clone()));
    }
    if let Some(focus) = tab.focus {
        node.entries_mut()
            .push(KdlEntry::new_prop("focus", KdlValue::Bool(focus)));
    }

    if !tab.panes.is_empty() {
        let mut inner = KdlDocument::new();
        for pane in &tab.panes {
            inner.nodes_mut().push(lower_pane(pane));
        }
        node.set_children(inner);
    }

    node
}

/// Lower a `PaneNode` to a `KdlNode`. Ark-only `when=` is stripped.
/// Every zellij-owned attribute (`name`, `command`, `size`,
/// `split_direction`, `focus`, `cwd`) passes through unchanged.
fn lower_pane(pane: &PaneNode) -> KdlNode {
    let mut node = KdlNode::new("pane");

    // Properties — emit in a stable order so rendered output is
    // deterministic for tests.
    if let Some(name) = &pane.name {
        node.entries_mut()
            .push(KdlEntry::new_prop("name", name.clone()));
    }
    if let Some(cmd) = &pane.command {
        node.entries_mut()
            .push(KdlEntry::new_prop("command", cmd.clone()));
    }
    if let Some(size) = &pane.size {
        node.entries_mut()
            .push(KdlEntry::new_prop("size", size.clone()));
    }
    if let Some(sd) = &pane.split_direction {
        node.entries_mut()
            .push(KdlEntry::new_prop("split_direction", sd.clone()));
    }
    if let Some(focus) = pane.focus {
        node.entries_mut()
            .push(KdlEntry::new_prop("focus", KdlValue::Bool(focus)));
    }
    if let Some(cwd) = &pane.cwd {
        node.entries_mut()
            .push(KdlEntry::new_prop("cwd", cwd.clone()));
    }

    if !pane.panes.is_empty() {
        let mut inner = KdlDocument::new();
        for child in &pane.panes {
            inner.nodes_mut().push(lower_pane(child));
        }
        node.set_children(inner);
    }

    node
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SceneDoc;
    use crate::context::{AgentSnapshot, SessionSnapshot};

    fn ctx() -> CompileContext {
        CompileContext::new(
            AgentSnapshot {
                id: "cavekit-auth-01".into(),
                name: "auth".into(),
                orchestrator: "cavekit".into(),
                engine: "claude-code".into(),
                cwd: "/tmp/worktree".into(),
                cmd: "claude".into(),
                args: vec!["--resume".into()],
            },
            SessionSnapshot {
                name: "ark-cavekit-auth".into(),
            },
        )
    }

    fn parse_scene(input: &str) -> SceneDoc {
        facet_kdl::from_str(input).expect("scene parses")
    }

    /// Smoke: an empty layout still produces a parseable `layout { }` doc.
    #[test]
    fn empty_layout_renders_wrapped_document() {
        let doc = parse_scene(r#"scene "s" { layout { } }"#);
        let layout = doc.scene.layout.as_ref().unwrap();
        let rendered = compile_layout(layout, &ctx()).expect("compile");
        assert!(
            rendered.contains("layout"),
            "rendered output missing `layout` wrapper: {rendered}"
        );
        // Round-trips via the kdl parser (already exercised inside
        // compile_layout; keep the assertion for explicitness).
        let _ = KdlDocument::parse(&rendered).expect("re-parse");
    }

    /// Pass-through: `tab "<name>"` → `tab "<name>"` in the rendered KDL.
    #[test]
    fn tab_name_passes_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "builder"
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        let parsed = KdlDocument::parse(&rendered).unwrap();
        let layout_node = parsed.nodes().iter().find(|n| n.name().value() == "layout").unwrap();
        let inner = layout_node.children().unwrap();
        let tab = inner.nodes().iter().find(|n| n.name().value() == "tab").unwrap();
        assert_eq!(tab.entries()[0].value(), &KdlValue::String("builder".into()));
    }

    /// Pass-through: `pane name="..." command="..."` → same output.
    #[test]
    fn pane_name_command_pass_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane name="editor" command="claude"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(
            rendered.contains("name=\"editor\"") || rendered.contains("name=editor"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("command"), "rendered: {rendered}");
    }

    /// Pass-through: `size` is emitted as an attribute.
    #[test]
    fn pane_size_passes_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane size="60%"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("60%"), "rendered: {rendered}");
    }

    /// Pass-through: `split_direction` keeps its snake-cased name.
    #[test]
    fn pane_split_direction_passes_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane split_direction="vertical"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("split_direction"), "rendered: {rendered}");
        assert!(rendered.contains("vertical"), "rendered: {rendered}");
    }

    /// Pass-through: `focus=#true` stays as a bool.
    #[test]
    fn pane_focus_passes_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane focus=#true
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        let parsed = KdlDocument::parse(&rendered).unwrap();
        let layout_n = parsed
            .nodes()
            .iter()
            .find(|n| n.name().value() == "layout")
            .unwrap();
        let tab_n = layout_n
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "tab")
            .unwrap();
        let pane_n = tab_n
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "pane")
            .unwrap();
        let focus_entry = pane_n
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("focus"))
            .expect("focus entry present");
        assert_eq!(focus_entry.value(), &KdlValue::Bool(true));
    }

    /// Pass-through: `cwd` on a pane survives lowering.
    #[test]
    fn pane_cwd_passes_through() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane cwd="/tmp/wt"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("/tmp/wt"), "rendered: {rendered}");
    }

    /// Strip: `when="…"` on a pane never appears in the rendered output.
    #[test]
    fn strip_when_on_pane() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane name="editor" when="agent.engine == 'claude-code'"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(!rendered.contains("when="), "rendered: {rendered}");
        assert!(!rendered.contains("agent.engine"), "rendered: {rendered}");
    }

    /// Strip: `when="…"` on a tab never appears in the rendered output.
    #[test]
    fn strip_when_on_tab() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" when="session.name == 'x'" {
            pane name="e"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(!rendered.contains("when="), "rendered: {rendered}");
        assert!(!rendered.contains("session.name"), "rendered: {rendered}");
    }

    /// Structure preserved: nested `pane` inside `tab` renders with the
    /// correct hierarchy.
    #[test]
    fn nested_structure_preserved() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" {
            pane split_direction="horizontal" {
                pane name="a" size="60%"
                pane name="b"
            }
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        let parsed = KdlDocument::parse(&rendered).unwrap();
        let layout_n = parsed
            .nodes()
            .iter()
            .find(|n| n.name().value() == "layout")
            .unwrap();
        let tab_n = layout_n
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "tab")
            .unwrap();
        let outer_pane = tab_n
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "pane")
            .unwrap();
        let inner_panes = outer_pane.children().expect("nested panes present");
        let pane_count = inner_panes
            .nodes()
            .iter()
            .filter(|n| n.name().value() == "pane")
            .count();
        assert_eq!(pane_count, 2, "expected 2 inner panes, rendered: {rendered}");
    }

    /// The rendered output round-trips through the upstream KDL parser.
    /// Belt-and-suspenders check duplicated in the compiler; here we
    /// prove the claim from an integration angle.
    #[test]
    fn output_round_trips_through_kdl_parser() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" {
            pane name="a" command="claude" size="50%"
            pane name="b" split_direction="horizontal"
        }
        tab "logs"
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        let parsed = KdlDocument::parse(&rendered).expect("rendered must parse");
        assert!(
            parsed
                .nodes()
                .iter()
                .any(|n| n.name().value() == "layout"),
            "expected `layout` root node; rendered: {rendered}"
        );
    }

    /// Multiple tabs render in source order.
    #[test]
    fn multiple_tabs_order_preserved() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "first"
        tab "second"
        tab "third"
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        let i_first = rendered.find("first").expect("first");
        let i_second = rendered.find("second").expect("second");
        let i_third = rendered.find("third").expect("third");
        assert!(i_first < i_second && i_second < i_third, "{rendered}");
    }
}
