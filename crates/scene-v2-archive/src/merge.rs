//! Scene composition loader + merger (R11).
//!
//! This module is the fold-together point for [`crate::extends`] +
//! [`crate::include`] + [`crate::clear`]. The pipeline is:
//!
//! 1. Start from the user's [`SceneDoc`] + its path.
//! 2. Recursively load every `extends` parent and every `include`
//!    fragment into a flat sequence of [`LoadedFragment`]s, preserving
//!    R11 load order (extensions handled elsewhere; parents first,
//!    then the child's own includes in source order, then the child's
//!    own root contributions).
//! 3. Track visited paths in a single `HashSet<PathBuf>` that spans
//!    both graphs, so an `extends → include → extends` loop surfaces
//!    as [`SceneError::IncludeCycle`] / [`SceneError::ExtendsCycle`]
//!    regardless of which edge closed it.
//! 4. Apply clears (see [`crate::clear`]) at the child boundary:
//!    parents and earlier-loaded includes contribute first; the
//!    child's `clear-reactions` / `clear-keybind` / `disable-plugin`
//!    directives drop matching items from the accumulated state; then
//!    the child's own additions land on top.
//! 5. Merge into a [`ComposedScene`] with R11 semantics:
//!    - **reactions** append in load order (all retained).
//!    - **keybinds** last-wins per chord.
//!    - **plugins** error on duplicate unless later block has
//!      `override=true`; `disable-plugin` silently drops prior.
//!    - **layout**: duplicate `tab name="X"` across merged layouts is
//!      an error; otherwise the first non-None layout wins and later
//!      fragments' tabs / panes are appended.
//!
//! The loader is **pure** in the sense of "no hidden env reads" — all
//! filesystem roots arrive through the [`crate::extends::SceneSearchCtx`]
//! parameter. The actual file reads are I/O, of course, but they are
//! the only side effect.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::{
    ClearKeybindNode, ClearReactionsNode, DisablePluginNode, KeybindNode, LayoutNode, OnNode,
    PluginNode, SceneDoc, TabNode,
};
use crate::clear::apply_clears;
use crate::error::SceneError;
use crate::extends::{SceneSearchCtx, load_extends};
use crate::include::load_include;

/// A single fragment produced by the composition loader.
///
/// Each fragment knows:
/// * the parsed [`SceneDoc`] it came from,
/// * the filesystem path it was read from (synthetic `<built-in:X>`
///   for baked-in scenes),
/// * the logical role — `Extends` parents first, then `Include`
///   splices in source order, then the `Root` child's own
///   contributions applied last.
///
/// The loader returns fragments in R11 load order. The merger then
/// walks them front-to-back, applying each one's contributions.
#[derive(Debug)]
pub struct LoadedFragment {
    /// Parsed scene document for this fragment.
    pub doc: SceneDoc,
    /// Source path for diagnostics + subsequent `include` resolution.
    pub path: PathBuf,
    /// Role of this fragment in the composition graph.
    pub role: FragmentRole,
}

/// How a fragment came to be loaded.
///
/// Matters for R11's "apply clears between parent merge and child's
/// own additions" rule: only the `Root` fragment's clear directives
/// trigger the clear pass; parents' and includes' clears are their
/// own merge responsibility (applied when they themselves are the
/// root of a sub-composition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentRole {
    /// Fragment contributed by an `extends` chain — parents merge
    /// before the child.
    Extends,
    /// Fragment contributed by an `include` splice — merged at the
    /// source position within the parent scene.
    Include,
    /// The user's own scene at the entry point of the composition.
    /// The root's `clear-*` directives are applied between the
    /// merged parents/includes and the root's own additions.
    Root,
}

/// Load the full composition graph rooted at `doc` (the user's scene)
/// into a sequenced list of [`LoadedFragment`]s.
///
/// Order of the returned vector:
/// 1. Grandparent → parent → ... (extends chain, deepest first).
/// 2. For each scene in the chain, its own `include` fragments in
///    source order, each appearing AFTER its owning scene's parents
///    but BEFORE its owning scene itself.
/// 3. The user's own scene (entry-point) last.
///
/// Cycle detection is unified across both graphs: a single
/// `HashSet<PathBuf>` tracks every path visited during the traversal.
/// Revisiting a canonical path → [`SceneError::ExtendsCycle`] (if the
/// closing edge was an `extends`) or [`SceneError::IncludeCycle`] (if
/// the closing edge was an `include`). The caller supplies the entry
/// path + context; all relative paths are canonicalised before
/// cycle-check insertion.
#[allow(clippy::result_large_err)]
pub fn load_composition(
    root_doc: SceneDoc,
    root_path: PathBuf,
    ctx: &SceneSearchCtx,
) -> Result<Vec<LoadedFragment>, SceneError> {
    let mut out: Vec<LoadedFragment> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    // Canonicalise the root path when possible; fall back to the
    // user-supplied path for built-ins / synthetic sources.
    let root_canonical = root_path.canonicalize().unwrap_or_else(|_| root_path.clone());
    visited.insert(root_canonical);

    load_recursive(root_doc, root_path, FragmentRole::Root, ctx, &mut visited, &mut out)?;
    Ok(out)
}

/// Inner recursion — walk the extends chain depth-first, then the
/// includes at this level, then append the scene itself.
#[allow(clippy::result_large_err)]
fn load_recursive(
    doc: SceneDoc,
    path: PathBuf,
    role: FragmentRole,
    ctx: &SceneSearchCtx,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<LoadedFragment>,
) -> Result<(), SceneError> {
    // 1. Parent chain first (depth-first recursion up the extends tree).
    if let Some((parent_doc, parent_path)) = load_extends(&doc, ctx)? {
        let parent_canonical = parent_path
            .canonicalize()
            .unwrap_or_else(|_| parent_path.clone());
        if !visited.insert(parent_canonical) {
            return Err(SceneError::ExtendsCycle {
                starting_scene: doc.scene.name.clone(),
                trail: vec![
                    doc.scene.name.clone(),
                    parent_doc.scene.name.clone(),
                    doc.scene.name.clone(),
                ],
            });
        }
        load_recursive(
            parent_doc,
            parent_path,
            FragmentRole::Extends,
            ctx,
            visited,
            out,
        )?;
    }

    // 2. Includes (in source order) for THIS scene.
    for inc in &doc.scene.includes {
        let (included_doc, included_path) = load_include(inc, &path)?;
        let canonical = included_path
            .canonicalize()
            .unwrap_or_else(|_| included_path.clone());
        if !visited.insert(canonical) {
            return Err(SceneError::IncludeCycle {
                starting_file: included_path.display().to_string(),
                src: miette::NamedSource::new(path.display().to_string(), String::new()),
                at: (0, 0).into(),
                trail: vec![],
            });
        }
        load_recursive(
            included_doc,
            included_path,
            FragmentRole::Include,
            ctx,
            visited,
            out,
        )?;
    }

    // 3. Self last — parents and includes contribute their state
    //    before the fragment itself lands on the accumulator.
    out.push(LoadedFragment {
        doc,
        path,
        role,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Merge stage: collapse a Vec<LoadedFragment> into a single ComposedScene
// ---------------------------------------------------------------------------

/// Result of composing a sequence of [`LoadedFragment`]s per R11
/// merge semantics.
///
/// Preserves every contribution needed by downstream compile passes
/// (layout, keybind, reaction registration). Fields mirror the
/// [`crate::ast::SceneNode`] body but in the post-merge shape.
#[derive(Debug, Default)]
pub struct ComposedScene {
    /// Name of the root scene — carried through unchanged so
    /// `ark scene graph` attribution still names the user's entry
    /// scene rather than a random parent.
    pub name: String,
    /// Merged `max-cascade-depth` property — root wins if set;
    /// otherwise parent's value is carried forward; otherwise
    /// unset (caller uses the R4 default).
    pub max_cascade_depth: Option<u32>,
    /// All `on { }` reactions in load order. Parents first, then
    /// includes in source order, then the root's own — exactly as
    /// the fragments arrive in the loader's output.
    pub reactions: Vec<OnNode>,
    /// Keybinds after last-wins-per-chord resolution. Iteration
    /// order matches the first-occurrence order of each surviving
    /// chord (stable: the earlier block's source position is kept
    /// but the value reflects the last-seen override).
    pub keybinds: Vec<KeybindNode>,
    /// Plugin blocks keyed by name. Duplicates across fragments are
    /// rejected unless the later block has `override=true`;
    /// `disable-plugin` drops prior entries silently.
    pub plugins: Vec<PluginNode>,
    /// Flattened layout — root or deepest ancestor contributes the
    /// base layout; includes append `tab`s / top-level `pane`s to
    /// that base. `tab` name collisions across fragments are
    /// rejected.
    pub layout: Option<LayoutNode>,
    /// Path of the ROOT fragment — useful for diagnostics and
    /// `scene_layout_path` derivation.
    pub root_path: PathBuf,
}

/// Merge a sequence of [`LoadedFragment`]s into a single
/// [`ComposedScene`] per R11.
///
/// Applies clears at the boundary between accumulated contributions
/// and the `Root`'s own additions: the root's `clear-reactions`,
/// `clear-keybind`, and `disable-plugin` directives drop matching
/// entries from the accumulator before the root's own reactions /
/// keybinds / plugins land on top.
#[allow(clippy::result_large_err)]
pub fn merge_fragments(fragments: Vec<LoadedFragment>) -> Result<ComposedScene, SceneError> {
    let mut acc = ComposedScene::default();
    let mut keybind_index: HashMap<String, usize> = HashMap::new();

    // Identify the root so we can pull its clear-* directives for the
    // "apply between parent-merge and root additions" step.
    let root_idx = fragments
        .iter()
        .position(|f| f.role == FragmentRole::Root)
        .unwrap_or(fragments.len().saturating_sub(1));

    for (idx, fragment) in fragments.iter().enumerate() {
        let is_root = idx == root_idx;

        // STEP A: If we are about to apply the root fragment, first
        //         run the root's clear-* directives against whatever
        //         the parents + includes have accumulated.
        if is_root {
            apply_clears(
                &mut acc,
                &fragment.doc.scene.clear_reactions,
                &fragment.doc.scene.clear_keybinds,
                &fragment.doc.scene.disable_plugins,
                &mut keybind_index,
            );
        }

        // STEP B: Apply this fragment's contributions.
        apply_fragment(
            &mut acc,
            &fragment.doc,
            &fragment.path,
            &mut keybind_index,
            is_root,
        )?;
    }

    // Remember which path we started at, for downstream attribution.
    if let Some(root) = fragments.iter().find(|f| f.role == FragmentRole::Root) {
        acc.name = root.doc.scene.name.clone();
        acc.root_path = root.path.clone();
        // Root-supplied max-cascade-depth overrides parents' (R11
        // child-wins); if root omits it, parent's carry value stays.
        if let Some(v) = root.doc.scene.max_cascade_depth {
            acc.max_cascade_depth = Some(v);
        }
    }

    Ok(acc)
}

/// Apply a single fragment's contributions to the accumulator.
#[allow(clippy::result_large_err)]
fn apply_fragment(
    acc: &mut ComposedScene,
    doc: &SceneDoc,
    _path: &Path,
    keybind_index: &mut HashMap<String, usize>,
    is_root: bool,
) -> Result<(), SceneError> {
    // Name + max_cascade_depth: root-wins semantics handled in the
    // wrapper; here we just carry forward for non-root fragments so
    // the root sees a sensible inherited default when it omits.
    if acc.name.is_empty() {
        acc.name = doc.scene.name.clone();
    }
    if !is_root && acc.max_cascade_depth.is_none() {
        acc.max_cascade_depth = doc.scene.max_cascade_depth;
    }

    // Reactions: append in load order. Clone OnNode is cheap-ish but
    // OnNode doesn't derive Clone; fall back to a manual shallow
    // copy via the public fields.
    for on in &doc.scene.ons {
        acc.reactions.push(clone_on_node(on));
    }

    // Keybinds: last-wins per chord. Maintain an index so repeat
    // chords replace the prior entry in place (stable ordering).
    for kb in &doc.scene.keybinds {
        let chord = kb.chord.clone();
        let cloned = clone_keybind_node(kb);
        if let Some(&idx) = keybind_index.get(&chord) {
            acc.keybinds[idx] = cloned;
        } else {
            keybind_index.insert(chord, acc.keybinds.len());
            acc.keybinds.push(cloned);
        }
    }

    // Plugins: duplicate-by-name is an error UNLESS the later block
    // has `override=true`. Ordering stable on first occurrence.
    for plugin in &doc.scene.plugins {
        if let Some(existing) = acc.plugins.iter_mut().find(|p| p.name == plugin.name) {
            if plugin.override_.unwrap_or(false) {
                *existing = clone_plugin_node(plugin);
            } else {
                return Err(SceneError::DuplicatePlugin {
                    name: plugin.name.clone(),
                });
            }
        } else {
            acc.plugins.push(clone_plugin_node(plugin));
        }
    }

    // Layout: merge tabs + top-level panes across fragments. First
    // fragment that declares a layout establishes the base; later
    // fragments append their tabs / panes. Tab-name collisions are
    // fatal per R11 (merge attribute deferred to v0.3+).
    if let Some(layout) = doc.scene.layout.as_ref() {
        match acc.layout.as_mut() {
            None => {
                acc.layout = Some(clone_layout_node(layout));
            }
            Some(existing) => {
                for tab in &layout.tabs {
                    if let Some(name) = tab.name.as_deref() {
                        if existing
                            .tabs
                            .iter()
                            .any(|t| t.name.as_deref() == Some(name))
                        {
                            return Err(SceneError::DuplicateTab {
                                name: name.to_string(),
                            });
                        }
                    }
                    existing.tabs.push(clone_tab_node(tab));
                }
                for pane in &layout.panes {
                    existing.panes.push(crate::clear::clone_pane_node(pane));
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Clone helpers (AST types don't derive Clone — explicit here)
// ---------------------------------------------------------------------------

pub(crate) fn clone_on_node(src: &OnNode) -> OnNode {
    OnNode {
        selector: src.selector.clone(),
        if_: src.if_.clone(),
        ops: src.ops.iter().map(clone_op_node).collect(),
    }
}

pub(crate) fn clone_keybind_node(src: &KeybindNode) -> KeybindNode {
    KeybindNode {
        chord: src.chord.clone(),
        intent: src.intent.clone(),
        ops: src.ops.iter().map(clone_op_node).collect(),
    }
}

pub(crate) fn clone_plugin_node(src: &PluginNode) -> PluginNode {
    PluginNode {
        name: src.name.clone(),
        override_: src.override_,
        source: src.source.as_ref().map(|s| crate::ast::SourceNode {
            uri: s.uri.clone(),
        }),
        mount: src.mount.as_ref().map(|m| crate::ast::MountNode {
            target: m.target.clone(),
            into: m.into.clone(),
            split: m.split.clone(),
            size: m.size.clone(),
            x: m.x.clone(),
            y: m.y.clone(),
            width: m.width.clone(),
            height: m.height.clone(),
        }),
        summon: src.summon.as_ref().map(|s| crate::ast::SummonNode {
            selector: s.selector.clone(),
        }),
        dismiss: src.dismiss.as_ref().map(|d| crate::ast::DismissNode {
            selector: d.selector.clone(),
        }),
        on: src
            .on
            .iter()
            .map(|o| crate::ast::PluginOnNode {
                selector: o.selector.clone(),
            })
            .collect(),
        subscribes: src
            .subscribes
            .iter()
            .map(|s| crate::ast::SubscribesNode {
                selector: s.selector.clone(),
            })
            .collect(),
        config: src.config.as_ref().map(|c| crate::ast::OpaqueBlock {
            args: c.args.clone(),
        }),
    }
}

pub(crate) fn clone_layout_node(src: &LayoutNode) -> LayoutNode {
    LayoutNode {
        tabs: src.tabs.iter().map(clone_tab_node).collect(),
        panes: src.panes.iter().map(crate::clear::clone_pane_node).collect(),
    }
}

pub(crate) fn clone_tab_node(src: &TabNode) -> TabNode {
    TabNode {
        name: src.name.clone(),
        when: src.when.clone(),
        focus: src.focus,
        panes: src.panes.iter().map(crate::clear::clone_pane_node).collect(),
    }
}

pub(crate) fn clone_op_node(src: &crate::ast::OpNode) -> crate::ast::OpNode {
    crate::ast::OpNode {
        args: src.args.clone(),
    }
}

// Unused-but-exported for crate::clear — silenced until clear wires up.
#[allow(dead_code)]
pub(crate) fn clone_clear_reactions(src: &ClearReactionsNode) -> ClearReactionsNode {
    ClearReactionsNode {
        selector: src.selector.clone(),
    }
}

#[allow(dead_code)]
pub(crate) fn clone_clear_keybind(src: &ClearKeybindNode) -> ClearKeybindNode {
    ClearKeybindNode {
        chord: src.chord.clone(),
    }
}

#[allow(dead_code)]
pub(crate) fn clone_disable_plugin(src: &DisablePluginNode) -> DisablePluginNode {
    DisablePluginNode {
        name: src.name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;
    use std::fs;
    use tempfile::TempDir;

    fn ctx_for(tmp: &Path) -> SceneSearchCtx {
        SceneSearchCtx::new(tmp)
    }

    /// Smoke test: a scene with no parents and no includes loads as
    /// a single `Root` fragment.
    #[test]
    fn single_scene_loads_as_root_only() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("scene.kdl");
        fs::write(&path, r#"scene "solo""#).unwrap();

        let doc = parse_scene(&std::fs::read_to_string(&path).unwrap(), &path).unwrap();
        let frags =
            load_composition(doc, path.clone(), &ctx_for(&root)).expect("load composition");
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].role, FragmentRole::Root);
        assert_eq!(frags[0].doc.scene.name, "solo");
    }

    /// `extends` populates the fragment list with the parent BEFORE
    /// the child (R11 load order).
    #[test]
    fn extends_parent_loads_before_child() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(scenes_dir.join("base.kdl"), r#"scene "base""#).unwrap();

        let child_path = root.join("child.kdl");
        let child_src = r#"
scene "child" {
    extends "base"
}
"#;
        fs::write(&child_path, child_src).unwrap();
        let child_doc = parse_scene(child_src, &child_path).unwrap();

        let frags = load_composition(child_doc, child_path, &ctx_for(&root))
            .expect("load composition");
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].role, FragmentRole::Extends);
        assert_eq!(frags[0].doc.scene.name, "base");
        assert_eq!(frags[1].role, FragmentRole::Root);
        assert_eq!(frags[1].doc.scene.name, "child");
    }

    /// `include` children load in source order, after parents.
    #[test]
    fn include_children_load_in_source_order() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let parent_path = root.join("parent.kdl");
        let a = root.join("a.kdl");
        let b = root.join("b.kdl");
        fs::write(&a, r#"scene "a""#).unwrap();
        fs::write(&b, r#"scene "b""#).unwrap();
        let src = r#"
scene "parent" {
    include "a.kdl"
    include "b.kdl"
}
"#;
        fs::write(&parent_path, src).unwrap();
        let doc = parse_scene(src, &parent_path).unwrap();

        let frags = load_composition(doc, parent_path, &ctx_for(&root)).expect("load");
        assert_eq!(frags.len(), 3);
        assert_eq!(frags[0].doc.scene.name, "a");
        assert_eq!(frags[0].role, FragmentRole::Include);
        assert_eq!(frags[1].doc.scene.name, "b");
        assert_eq!(frags[1].role, FragmentRole::Include);
        assert_eq!(frags[2].doc.scene.name, "parent");
        assert_eq!(frags[2].role, FragmentRole::Root);
    }

    /// Self-include is a cycle: `a.kdl` includes itself.
    #[test]
    fn self_include_detected_as_cycle() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("a.kdl");
        let src = r#"
scene "a" {
    include "a.kdl"
}
"#;
        fs::write(&path, src).unwrap();
        let doc = parse_scene(src, &path).unwrap();

        let err = load_composition(doc, path, &ctx_for(&root)).expect_err("cycle");
        assert!(matches!(err, SceneError::IncludeCycle { .. }));
    }

    /// Two-hop include cycle: `a → b → a`.
    #[test]
    fn two_hop_include_cycle_detected() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let a = root.join("a.kdl");
        let b = root.join("b.kdl");
        fs::write(
            &a,
            r#"scene "a" {
    include "b.kdl"
}"#,
        )
        .unwrap();
        fs::write(
            &b,
            r#"scene "b" {
    include "a.kdl"
}"#,
        )
        .unwrap();

        let doc = parse_scene(&std::fs::read_to_string(&a).unwrap(), &a).unwrap();
        let err = load_composition(doc, a, &ctx_for(&root)).expect_err("cycle");
        assert!(matches!(err, SceneError::IncludeCycle { .. }));
    }

    /// Cross-graph cycle: child extends parent, parent includes
    /// child. Fires as IncludeCycle (closing edge is an include).
    #[test]
    fn extends_include_cross_graph_cycle() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        let child_path = root.join("child.kdl");
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        let parent_path = scenes_dir.join("parent.kdl");

        fs::write(
            &child_path,
            r#"scene "child" {
    extends "parent"
}"#,
        )
        .unwrap();
        fs::write(
            &parent_path,
            format!(
                r#"scene "parent" {{
    include "{}"
}}"#,
                child_path.display()
            ),
        )
        .unwrap();

        let src = std::fs::read_to_string(&child_path).unwrap();
        let doc = parse_scene(&src, &child_path).unwrap();
        let err = load_composition(doc, child_path, &ctx_for(&root)).expect_err("cycle");
        assert!(matches!(err, SceneError::IncludeCycle { .. }));
    }

    /// Merge: two fragments' reactions append in load order.
    #[test]
    fn merge_appends_reactions_in_load_order() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    on "AgentReady" {
        exec
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    on "AgentReady" {
        emit
    }
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();

        let frags = load_composition(doc, child_path, &ctx_for(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.reactions.len(), 2);
    }

    /// Merge: keybinds resolve last-wins per chord.
    #[test]
    fn merge_keybinds_last_wins() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    keybind "Alt p" intent="picker.show"
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    keybind "Alt p" intent="user.custom"
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags = load_composition(doc, child_path, &ctx_for(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.keybinds.len(), 1);
        assert_eq!(merged.keybinds[0].intent.as_deref(), Some("user.custom"));
    }

    /// Duplicate plugin across fragments without `override=true`
    /// errors with `scene/duplicate-plugin`.
    #[test]
    fn merge_rejects_duplicate_plugin_without_override() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    plugin "status" {
        source "shipped:status"
        mount "floating"
    }
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags = load_composition(doc, child_path, &ctx_for(&root)).unwrap();
        let err = merge_fragments(frags).expect_err("duplicate plugin");
        assert!(matches!(err, SceneError::DuplicatePlugin { .. }));
    }

    /// `override=true` on the child's plugin block replaces the
    /// parent's declaration silently.
    #[test]
    fn merge_override_plugin_replaces_parent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    plugin "status" override=#true {
        source "shipped:status"
        mount "floating"
    }
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags = load_composition(doc, child_path, &ctx_for(&root)).unwrap();
        let merged = merge_fragments(frags).expect("override wins");
        assert_eq!(merged.plugins.len(), 1);
        assert_eq!(
            merged.plugins[0].mount.as_ref().map(|m| m.target.as_str()),
            Some("floating")
        );
    }

    /// Duplicate tab name across merged layouts is fatal.
    #[test]
    fn merge_rejects_duplicate_tab_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    layout {
        tab "work"
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    layout {
        tab "work"
    }
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags = load_composition(doc, child_path, &ctx_for(&root)).unwrap();
        let err = merge_fragments(frags).expect_err("duplicate tab");
        assert!(matches!(err, SceneError::DuplicateTab { .. }));
    }
}
