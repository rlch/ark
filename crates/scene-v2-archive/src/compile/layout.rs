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
//! nothing to zellij — `when=` — are stripped during lowering.
//!
//! # `when=` branch pruning (T-3.2)
//!
//! When a `tab` / `pane` carries a `when="<CEL>"` predicate, the
//! compiler evaluates the expression against the **static
//! compile-time context** ([`CompileContext`]) before lowering. The
//! CEL surface at spawn time is deliberately narrower than at
//! reaction time:
//!
//! - `agent.{id, name, orchestrator, engine, cwd, cmd, args}` — from
//!   the `AgentSpec` that drove this spawn.
//! - `session.{name}` — the resolved zellij session name.
//!
//! There is no `phase`, no `event`, no `payload` because none of
//! those exist yet at spawn. Dynamic predicates belong in a reaction
//! `if=` guard, not a layout `when=`. Authors who reach for
//! `event.*` in a layout `when=` get a CEL-evaluate error surfaced
//! through [`SceneError::CelEvaluate`] — consistent with the rest
//! of the scene pipeline.
//!
//! Branches that evaluate to `false` are **pruned** — the node and
//! its entire subtree are dropped before they reach the rendered
//! KDL. Branches that evaluate to `true`, or carry no `when=` at
//! all, are lowered unchanged (sans `when=` attribute). The
//! compiler emits a single `tracing::debug!(target = "scene::compile",
//! retained, pruned)` record summarising the pass so operators can
//! follow pruning decisions via `RUST_LOG=scene::compile=debug`.
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
//! Invalid `when=` predicates surface as [`SceneError::CelParse`]
//! (compile-time) or [`SceneError::CelEvaluate`] (runtime type
//! mismatch, undefined reference, etc.). The compiler returns
//! [`SceneError::Grammar`] if the rendered output fails to re-parse
//! (belt-and-suspenders check — in practice the builder API never
//! produces invalid KDL, but the guard is cheap and catches upstream
//! bugs early).

use cel_interpreter::Context;
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use miette::NamedSource;
use serde_json::Value as JsonValue;

use crate::ast::{LayoutNode, PaneNode, TabNode};
use crate::cel;
use crate::error::SceneError;

/// Static compile-time context for `when=` predicate evaluation.
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

    /// Build a `cel_interpreter::Context` bound with **only** the
    /// spawn-time surface (no `event`, no `payload`).
    ///
    /// Used internally by the pruning pass. Kept crate-public so
    /// downstream compile passes (extension merge, keybind lowering)
    /// can reuse the exact same binding set.
    pub(crate) fn to_cel_context(&self) -> Result<Context<'_>, SceneError> {
        let mut ctx = Context::default();
        cel::register_custom_functions(&mut ctx);

        let agent_json =
            serde_json::to_value(&self.agent).unwrap_or(JsonValue::Null);
        ctx.add_variable("agent", agent_json)
            .map_err(|e| SceneError::CelEvaluate {
                message: format!("failed to bind `agent`: {e}"),
            })?;

        let session_json =
            serde_json::to_value(&self.session).unwrap_or(JsonValue::Null);
        ctx.add_variable("session", session_json)
            .map_err(|e| SceneError::CelEvaluate {
                message: format!("failed to bind `session`: {e}"),
            })?;

        Ok(ctx)
    }
}

/// Counters returned from the compile pass, for debug logging.
///
/// The pair is not part of the public `compile_layout` signature — it
/// rides along in the `tracing::debug!` record. Exposed as a private
/// struct so the recursion below can mutate it uniformly.
#[derive(Default)]
struct PruneStats {
    retained: u32,
    pruned: u32,
}

/// Lower a scene layout AST to a zellij-compatible KDL string.
///
/// The returned string is the full zellij layout document (wrapped
/// in the top-level `layout { }` node zellij expects). Output is
/// guaranteed to re-parse via [`kdl::KdlDocument::parse`] before
/// return.
///
/// Branches whose `when=` predicate evaluates to `false` against
/// [`CompileContext`] are pruned before rendering.
pub fn compile_layout(
    layout: &LayoutNode,
    ctx: &CompileContext,
) -> Result<String, SceneError> {
    let cel_ctx = ctx.to_cel_context()?;
    let mut stats = PruneStats::default();

    let mut doc = KdlDocument::new();

    // Build the single outer `layout { … }` node.
    let mut layout_node = KdlNode::new("layout");
    let mut inner = KdlDocument::new();
    for tab in &layout.tabs {
        if let Some(node) = lower_tab(tab, &cel_ctx, &mut stats)? {
            inner.nodes_mut().push(node);
        }
    }
    for pane in &layout.panes {
        if let Some(node) = lower_pane(pane, &cel_ctx, &mut stats)? {
            inner.nodes_mut().push(node);
        }
    }
    layout_node.set_children(inner);
    doc.nodes_mut().push(layout_node);

    // Autoformat applies consistent indentation + newlines.
    doc.autoformat();
    let rendered = doc.to_string();

    tracing::debug!(
        target: "scene::compile",
        retained = stats.retained,
        pruned = stats.pruned,
        "layout when= pass"
    );

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

/// Evaluate a `when=` predicate against the compile-time context.
///
/// Returns `Ok(true)` when the predicate is absent (unconditional
/// retention). Returns `Ok(false)` when the predicate evaluates to
/// `false`. Any parse / evaluation / non-bool-result error surfaces
/// as a [`SceneError`].
fn retain_branch(
    when: Option<&str>,
    cel_ctx: &Context<'_>,
) -> Result<bool, SceneError> {
    let Some(expr) = when else {
        return Ok(true);
    };
    let prog = cel::compile(expr, "<when>", 0)?;
    cel::eval_bool(&prog, cel_ctx)
}

/// Lower a `TabNode` to a `KdlNode`, or `None` if its `when=`
/// predicate evaluates to `false`. Ark-only `when=` is stripped.
fn lower_tab(
    tab: &TabNode,
    cel_ctx: &Context<'_>,
    stats: &mut PruneStats,
) -> Result<Option<KdlNode>, SceneError> {
    if !retain_branch(tab.when.as_deref(), cel_ctx)? {
        stats.pruned += 1;
        return Ok(None);
    }
    stats.retained += 1;

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
            if let Some(child) = lower_pane(pane, cel_ctx, stats)? {
                inner.nodes_mut().push(child);
            }
        }
        // Only attach an empty children doc if at least one pane
        // survived pruning; otherwise keep the node childless so zellij
        // doesn't see an empty body.
        if !inner.nodes().is_empty() {
            node.set_children(inner);
        }
    }

    Ok(Some(node))
}

/// Lower a `PaneNode` to a `KdlNode`, or `None` if its `when=`
/// predicate evaluates to `false`. Every zellij-owned attribute
/// (`name`, `command`, `size`, `split_direction`, `focus`, `cwd`)
/// passes through unchanged.
fn lower_pane(
    pane: &PaneNode,
    cel_ctx: &Context<'_>,
    stats: &mut PruneStats,
) -> Result<Option<KdlNode>, SceneError> {
    if !retain_branch(pane.when.as_deref(), cel_ctx)? {
        stats.pruned += 1;
        return Ok(None);
    }
    stats.retained += 1;

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
            if let Some(lowered) = lower_pane(child, cel_ctx, stats)? {
                inner.nodes_mut().push(lowered);
            }
        }
        if !inner.nodes().is_empty() {
            node.set_children(inner);
        }
    }

    Ok(Some(node))
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

    /// Strip: `when="…"` on a retained pane never appears in the
    /// rendered output (the branch is kept because the predicate is
    /// `true` against the default test context).
    #[test]
    fn strip_when_on_retained_pane() {
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
        // The pane was retained (predicate true) so its `name=` survived.
        assert!(rendered.contains("editor"), "rendered: {rendered}");
    }

    /// Strip: `when="…"` on a retained tab never appears in the
    /// rendered output.
    #[test]
    fn strip_when_on_retained_tab() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" when="session.name == 'ark-cavekit-auth'" {
            pane name="e"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(!rendered.contains("when="), "rendered: {rendered}");
        assert!(!rendered.contains("session.name"), "rendered: {rendered}");
        // Tab retained.
        assert!(rendered.contains("work"), "rendered: {rendered}");
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

    // ---------------------------------------------------------------
    // T-3.2 pruning tests
    // ---------------------------------------------------------------

    /// `when="agent.engine == 'claude'"` against `agent.engine =
    /// "claude"` retains the branch (predicate evaluates to `true`).
    #[test]
    fn when_true_retains_branch() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" {
            pane name="editor" when="agent.engine == 'claude-code'"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("editor"), "retained: {rendered}");
    }

    /// `when="agent.engine == 'codex'"` against `agent.engine =
    /// "claude-code"` prunes the branch.
    #[test]
    fn when_false_prunes_branch() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" {
            pane name="editor" when="agent.engine == 'codex'"
            pane name="kept"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(!rendered.contains("editor"), "should be pruned: {rendered}");
        assert!(rendered.contains("kept"), "should survive: {rendered}");
    }

    /// Pruning a tab drops its entire subtree.
    #[test]
    fn when_false_on_tab_prunes_subtree() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "pruned" when="agent.engine == 'codex'" {
            pane name="child_should_die"
        }
        tab "kept" {
            pane name="kept_pane"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(!rendered.contains("pruned"), "tab pruned: {rendered}");
        assert!(
            !rendered.contains("child_should_die"),
            "subtree pruned: {rendered}"
        );
        assert!(rendered.contains("kept_pane"), "sibling kept: {rendered}");
    }

    /// Nested panes under a retained parent still evaluate their own
    /// `when=` independently.
    #[test]
    fn nested_pane_pruning_is_independent() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane split_direction="horizontal" {
                pane name="visible"
                pane name="invisible" when="agent.engine == 'codex'"
            }
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("visible"), "{rendered}");
        assert!(!rendered.contains("invisible"), "{rendered}");
    }

    /// `session.*` bindings are readable from layout `when=`.
    #[test]
    fn session_bindings_available_in_when() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane name="logs" when="starts_with(session.name, 'ark-')"
        }
    }
}
"#,
        );
        let rendered = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx()).unwrap();
        assert!(rendered.contains("logs"), "{rendered}");
    }

    /// Reaching for an unavailable binding (`event.*`, `payload.*`,
    /// `phase`) in a layout `when=` is a CEL evaluate error — dynamic
    /// predicates must use reaction `if=`, not layout `when=`.
    #[test]
    fn event_binding_unavailable_in_layout_when() {
        use crate::error::ErrorCode;
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane when="event.kind == 'started'"
        }
    }
}
"#,
        );
        let err = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx())
            .expect_err("event.* should be unbound at spawn-time");
        assert_eq!(err.code_enum(), ErrorCode::CelEvaluate);
    }

    /// A malformed CEL expression surfaces as a `cel/parse` error.
    #[test]
    fn malformed_when_surfaces_cel_parse() {
        use crate::error::ErrorCode;
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane when="agent.engine =="
        }
    }
}
"#,
        );
        let err = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx())
            .expect_err("malformed CEL should fail");
        assert_eq!(err.code_enum(), ErrorCode::CelParse);
    }

    /// A non-bool `when=` result (e.g. arithmetic) surfaces as a
    /// `cel/evaluate` error — `when=` must evaluate to a bool.
    #[test]
    fn non_bool_when_surfaces_cel_evaluate() {
        use crate::error::ErrorCode;
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "t" {
            pane when="1 + 1"
        }
    }
}
"#,
        );
        let err = compile_layout(doc.scene.layout.as_ref().unwrap(), &ctx())
            .expect_err("non-bool when= should fail");
        assert_eq!(err.code_enum(), ErrorCode::CelEvaluate);
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
