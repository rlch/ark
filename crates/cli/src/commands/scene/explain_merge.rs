//! `ark scene explain-merge` — trace scene composition per R11.
//!
//! T-12.11 (cavekit-scene R13). Prints which fragment each contribution
//! came from, and for plugins / keybinds, which fragment's value won
//! the merge. Designed to answer:
//!
//! * "Where does keybind `Alt p` come from and who last overrode it?"
//! * "Did my `plugin override=#true` actually replace the parent's
//!   declaration, or did the parent slip through unchanged?"
//! * "Which reactions does the final scene run, in what order, from
//!   which fragment?"
//!
//! The command re-runs the same composition + merge pipeline used by
//! [`ark_scene::compile::compile_scene_file_with_composition`], then
//! walks the per-fragment contributions to attribute each final entry
//! back to its originating fragment.
//!
//! Merge rules per R11 (reproduced here so the CLI help text matches
//! semantics):
//!
//! * Reactions: APPEND in load order — parents first, includes in
//!   source order, root last. Every reaction is retained.
//! * Keybinds: LAST-WINS per chord — the latest fragment declaring a
//!   given chord wins; earlier declarations are shadowed.
//! * Plugins: duplicate-by-name is an ERROR unless the later fragment
//!   uses `override=#true`. `disable-plugin "<name>"` drops prior
//!   contributions silently.
//! * Layout: first fragment declaring a layout seeds the base; later
//!   fragments append tabs / top-level panes. Duplicate tab names are
//!   fatal.

use std::path::{Path, PathBuf};

use clap::Args;

use ark_scene::extends::SceneSearchCtx;
use ark_scene::merge::{
    FragmentRole, LoadedFragment, load_composition, merge_fragments,
};
use ark_scene::parse::parse_scene;
use ark_scene::path::DEFAULT_APPNAME;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene explain-merge`.
#[derive(Debug, Args)]
pub struct ExplainMergeArgs {
    /// Path to a scene file. Explains composition of that scene.
    #[arg(value_name = "SCENE")]
    pub scene: PathBuf,
}

pub fn run(args: ExplainMergeArgs, _ctx: &Ctx) -> Result<(), CliError> {
    // ---- Step 1: load entry scene --------------------------------------
    let src = std::fs::read_to_string(&args.scene).map_err(|e| CliError::Generic {
        reason: format!("cannot read {}: {e}", args.scene.display()),
    })?;
    let doc = parse_scene(&src, &args.scene).map_err(|e| CliError::Generic {
        reason: format!("parse {}: {e}", args.scene.display()),
    })?;

    // ---- Step 2: walk the composition graph + merge --------------------
    let search_ctx = build_search_ctx(&args.scene);
    let fragments = load_composition(doc, args.scene.clone(), &search_ctx)
        .map_err(|e| CliError::Generic {
            reason: format!("resolve composition for {}: {e}", args.scene.display()),
        })?;

    // Running the full merge catches errors like DuplicatePlugin /
    // DuplicateTab. We still want the trace even when the merge
    // succeeds, so we clone relevant data from fragments first and
    // then merge to surface any fatal conflicts the user should know
    // about.
    let trace = build_trace(&fragments);
    // Re-run the merger on owned fragments (it consumes them); we
    // already have the trace computed from the borrowed view.
    let fragments_owned = load_composition(
        parse_scene(&src, &args.scene).map_err(|e| CliError::Generic {
            reason: format!("re-parse {}: {e}", args.scene.display()),
        })?,
        args.scene.clone(),
        &search_ctx,
    )
    .map_err(|e| CliError::Generic {
        reason: format!(
            "re-resolve composition for {}: {e}",
            args.scene.display()
        ),
    })?;
    let merge_result = merge_fragments(fragments_owned);

    // ---- Step 3: render -------------------------------------------------
    render_trace(&trace, &args.scene);
    match merge_result {
        Ok(_) => Ok(()),
        Err(err) => Err(CliError::Generic {
            reason: format!("merge conflict: {err}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Trace model
// ---------------------------------------------------------------------------

/// Flattened merge trace — one entry per contribution across every
/// fragment, plus the winner attribution for merge-resolved categories.
#[derive(Debug)]
struct MergeTrace {
    fragments: Vec<FragmentSummary>,
    /// Every reaction in load order — all are retained per R11.
    reactions: Vec<Contribution>,
    /// Every keybind declaration in load order, with the winning flag
    /// set on the *last* occurrence per chord (last-wins).
    keybinds: Vec<KeybindContribution>,
    /// Every plugin declaration in load order, with the winner flag
    /// on the surviving fragment (the last non-disabled, override-
    /// conflict-free declaration).
    plugins: Vec<PluginContribution>,
    /// Every `use "<name>"` activation in load order.
    uses: Vec<Contribution>,
    /// Every layout-tab contribution — first fragment seeds, later
    /// fragments append. Duplicate names are flagged (the merger
    /// returns DuplicateTab).
    layout_tabs: Vec<LayoutTabContribution>,
    /// Every `clear-reactions` / `clear-keybind` / `disable-plugin`
    /// directive from the root fragment. Parents / includes do NOT
    /// contribute these (per R11 the clear pass fires at the child
    /// boundary only).
    root_clears: Vec<RootClear>,
}

#[derive(Debug)]
struct FragmentSummary {
    role: &'static str,
    path: String,
    scene_name: String,
}

#[derive(Debug)]
struct Contribution {
    /// Display label (selector, use name, etc.).
    label: String,
    /// Fragment index (into `MergeTrace::fragments`).
    fragment_idx: usize,
}

#[derive(Debug)]
struct KeybindContribution {
    chord: String,
    intent: Option<String>,
    fragment_idx: usize,
    /// `true` when this is the last-wins survivor for this chord.
    winner: bool,
    /// `true` when a `clear-keybind "<chord>"` directive from the root
    /// dropped this contribution.
    cleared_by_root: bool,
}

#[derive(Debug)]
struct PluginContribution {
    name: String,
    fragment_idx: usize,
    /// `true` when the later fragment set `override=#true`.
    override_set: bool,
    /// `true` when this is the surviving declaration after merge.
    winner: bool,
    /// `true` when a `disable-plugin "<name>"` directive from the
    /// root dropped this contribution.
    disabled_by_root: bool,
}

#[derive(Debug)]
struct LayoutTabContribution {
    name: Option<String>,
    fragment_idx: usize,
}

#[derive(Debug)]
enum RootClear {
    ClearReactions { selector: String },
    ClearKeybind { chord: String },
    DisablePlugin { name: String },
}

// ---------------------------------------------------------------------------
// Trace building
// ---------------------------------------------------------------------------

fn build_trace(fragments: &[LoadedFragment]) -> MergeTrace {
    let fragment_summaries: Vec<FragmentSummary> = fragments
        .iter()
        .map(|f| FragmentSummary {
            role: role_label(f.role),
            path: f.path.display().to_string(),
            scene_name: f.doc.scene.name.clone(),
        })
        .collect();

    // Locate the root fragment so we can evaluate its clear-* / disable-*
    // directives against the accumulated contributions.
    let root_idx = fragments
        .iter()
        .position(|f| f.role == FragmentRole::Root)
        .unwrap_or(fragments.len().saturating_sub(1));

    // ---- Reactions: append in load order (all retained) ----
    let mut reactions: Vec<Contribution> = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        for on in &frag.doc.scene.ons {
            reactions.push(Contribution {
                label: on.selector.clone(),
                fragment_idx: idx,
            });
        }
    }

    // ---- Uses: recorded as they appear ----
    let mut uses: Vec<Contribution> = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        for u in &frag.doc.scene.uses {
            uses.push(Contribution {
                label: u.name.clone(),
                fragment_idx: idx,
            });
        }
    }

    // ---- Keybinds: last-wins per chord ----
    let mut keybinds: Vec<KeybindContribution> = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        for kb in &frag.doc.scene.keybinds {
            keybinds.push(KeybindContribution {
                chord: kb.chord.clone(),
                intent: kb.intent.clone(),
                fragment_idx: idx,
                winner: false,
                cleared_by_root: false,
            });
        }
    }
    // Mark winners: for each distinct chord, the LAST contribution wins.
    // Iterate right-to-left, flagging the first we see per chord.
    let mut seen_chord: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for kb in keybinds.iter_mut().rev() {
        if seen_chord.insert(kb.chord.clone()) {
            kb.winner = true;
        }
    }
    // Apply root's clear-keybind directives: a chord cleared by root
    // loses its winner flag entirely (no keybind survives).
    if let Some(root) = fragments.get(root_idx) {
        for clear in &root.doc.scene.clear_keybinds {
            for kb in keybinds.iter_mut() {
                if kb.chord == clear.chord && kb.fragment_idx < root_idx {
                    kb.cleared_by_root = true;
                    kb.winner = false;
                }
            }
        }
    }

    // ---- Plugins: duplicate-by-name resolution ----
    let mut plugins: Vec<PluginContribution> = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        for p in &frag.doc.scene.plugins {
            plugins.push(PluginContribution {
                name: p.name.clone(),
                fragment_idx: idx,
                override_set: p.override_.unwrap_or(false),
                winner: false,
                disabled_by_root: false,
            });
        }
    }
    // Walk left-to-right: track the current surviving index per plugin
    // name. When we see a duplicate, the later declaration's
    // `override=#true` flag determines whether it supersedes (winner
    // flag moves); otherwise the duplicate is a merge conflict —
    // the merger itself will error. We still record the winner for
    // the explain output so the user understands which fragment the
    // error attribution came from.
    let mut winner_idx: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (list_idx, contrib) in plugins.iter().enumerate() {
        match winner_idx.get(&contrib.name) {
            None => {
                winner_idx.insert(contrib.name.clone(), list_idx);
            }
            Some(&_prev_idx) if contrib.override_set => {
                winner_idx.insert(contrib.name.clone(), list_idx);
            }
            Some(_) => { /* duplicate without override — merger will error */ }
        }
    }
    for (list_idx, contrib) in plugins.iter_mut().enumerate() {
        if let Some(&winner) = winner_idx.get(&contrib.name) {
            contrib.winner = winner == list_idx;
        }
    }
    // Apply root's disable-plugin directives: matching contributions
    // are dropped.
    if let Some(root) = fragments.get(root_idx) {
        for disable in &root.doc.scene.disable_plugins {
            for p in plugins.iter_mut() {
                if p.name == disable.name && p.fragment_idx < root_idx {
                    p.disabled_by_root = true;
                    p.winner = false;
                }
            }
            // If ALL occurrences of this plugin were disabled, no
            // winner survives.
        }
    }

    // ---- Layout: base + append ----
    let mut layout_tabs: Vec<LayoutTabContribution> = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        if let Some(layout) = &frag.doc.scene.layout {
            for tab in &layout.tabs {
                layout_tabs.push(LayoutTabContribution {
                    name: tab.name.clone(),
                    fragment_idx: idx,
                });
            }
        }
    }

    // ---- Root clears ----
    let mut root_clears: Vec<RootClear> = Vec::new();
    if let Some(root) = fragments.get(root_idx) {
        for cr in &root.doc.scene.clear_reactions {
            root_clears.push(RootClear::ClearReactions {
                selector: cr.selector.clone(),
            });
        }
        for ck in &root.doc.scene.clear_keybinds {
            root_clears.push(RootClear::ClearKeybind {
                chord: ck.chord.clone(),
            });
        }
        for dp in &root.doc.scene.disable_plugins {
            root_clears.push(RootClear::DisablePlugin {
                name: dp.name.clone(),
            });
        }
    }

    MergeTrace {
        fragments: fragment_summaries,
        reactions,
        keybinds,
        plugins,
        uses,
        layout_tabs,
        root_clears,
    }
}

fn role_label(role: FragmentRole) -> &'static str {
    match role {
        FragmentRole::Extends => "extends",
        FragmentRole::Include => "include",
        FragmentRole::Root => "root",
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_trace(trace: &MergeTrace, scene: &Path) {
    println!("scene explain-merge: {}", scene.display());
    println!();
    println!("Fragments ({}):", trace.fragments.len());
    for (idx, f) in trace.fragments.iter().enumerate() {
        println!(
            "  [{idx}] {role:<8}  scene \"{name}\"  ({path})",
            idx = idx,
            role = f.role,
            name = f.scene_name,
            path = f.path
        );
    }
    println!();

    if !trace.uses.is_empty() {
        println!("Extensions (use, in load order):");
        for u in &trace.uses {
            println!("  use \"{}\"  from [{}]", u.label, u.fragment_idx);
        }
        println!();
    }

    println!("Reactions ({}; all retained in load order):", trace.reactions.len());
    for r in &trace.reactions {
        println!(
            "  on \"{sel}\"  from [{idx}]",
            sel = r.label,
            idx = r.fragment_idx
        );
    }
    println!();

    println!(
        "Keybinds ({}; last-wins per chord):",
        trace.keybinds.len()
    );
    for kb in &trace.keybinds {
        let winner_tag = if kb.winner {
            " [WINS]"
        } else if kb.cleared_by_root {
            " [cleared by root]"
        } else {
            " [shadowed]"
        };
        let intent_tag = kb
            .intent
            .as_deref()
            .map(|i| format!(" intent=\"{i}\""))
            .unwrap_or_default();
        println!(
            "  keybind \"{chord}\"{intent}  from [{idx}]{w}",
            chord = kb.chord,
            intent = intent_tag,
            idx = kb.fragment_idx,
            w = winner_tag
        );
    }
    println!();

    println!(
        "Plugins ({}; duplicate-by-name = error unless override=#true):",
        trace.plugins.len()
    );
    for p in &trace.plugins {
        let winner_tag = if p.winner {
            " [WINS]"
        } else if p.disabled_by_root {
            " [disabled by root]"
        } else if p.override_set {
            " [overrides prior]"
        } else {
            " [superseded or conflict]"
        };
        println!(
            "  plugin \"{name}\"  from [{idx}]{w}",
            name = p.name,
            idx = p.fragment_idx,
            w = winner_tag
        );
    }
    println!();

    if !trace.layout_tabs.is_empty() {
        println!(
            "Layout tabs ({}; first fragment seeds, later append):",
            trace.layout_tabs.len()
        );
        for t in &trace.layout_tabs {
            let name = t.name.as_deref().unwrap_or("<unnamed>");
            println!("  tab \"{name}\"  from [{idx}]", idx = t.fragment_idx);
        }
        println!();
    }

    if !trace.root_clears.is_empty() {
        println!("Root clear-* directives:");
        for c in &trace.root_clears {
            match c {
                RootClear::ClearReactions { selector } => {
                    println!("  clear-reactions selector=\"{selector}\"");
                }
                RootClear::ClearKeybind { chord } => {
                    println!("  clear-keybind \"{chord}\"");
                }
                RootClear::DisablePlugin { name } => {
                    println!("  disable-plugin \"{name}\"");
                }
            }
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Search ctx
// ---------------------------------------------------------------------------

fn build_search_ctx(entry_path: &Path) -> SceneSearchCtx {
    let cwd = entry_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config"))
        });
    let appname = std::env::var("ARK_APPNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_APPNAME.to_string());
    SceneSearchCtx {
        cwd,
        xdg_config_home: xdg,
        appname,
        builtins: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_scenes() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
    keybind "Alt p" intent="picker.show"
    on "Started" { }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let child_src = r#"scene "child" {
    extends "base"
    keybind "Alt p" intent="user.custom"
    on "Done" { }
}
"#;
        fs::write(&child_path, child_src).unwrap();
        (tmp, child_path)
    }

    #[test]
    fn trace_marks_last_wins_keybind_as_winner() {
        let (_tmp, child_path) = setup_scenes();
        let src = fs::read_to_string(&child_path).unwrap();
        let doc = parse_scene(&src, &child_path).unwrap();
        let ctx = SceneSearchCtx::new(child_path.parent().unwrap());
        let frags = load_composition(doc, child_path.clone(), &ctx).unwrap();
        let trace = build_trace(&frags);

        // Both base and child have keybind "Alt p" → last-wins = child.
        assert_eq!(trace.keybinds.len(), 2);
        // Child is the root (index 1 per R11 load order: parent first).
        let child_kb = trace
            .keybinds
            .iter()
            .find(|k| k.fragment_idx == 1)
            .expect("child keybind");
        assert!(child_kb.winner, "child's keybind should win");
        let base_kb = trace
            .keybinds
            .iter()
            .find(|k| k.fragment_idx == 0)
            .expect("base keybind");
        assert!(!base_kb.winner, "base keybind should be shadowed");
    }

    #[test]
    fn trace_keeps_every_reaction() {
        let (_tmp, child_path) = setup_scenes();
        let src = fs::read_to_string(&child_path).unwrap();
        let doc = parse_scene(&src, &child_path).unwrap();
        let ctx = SceneSearchCtx::new(child_path.parent().unwrap());
        let frags = load_composition(doc, child_path.clone(), &ctx).unwrap();
        let trace = build_trace(&frags);

        // Base has "Started"; child has "Done".
        assert_eq!(trace.reactions.len(), 2);
        assert!(trace
            .reactions
            .iter()
            .any(|r| r.label == "Started" && r.fragment_idx == 0));
        assert!(trace
            .reactions
            .iter()
            .any(|r| r.label == "Done" && r.fragment_idx == 1));
    }

    #[test]
    fn trace_surfaces_override_and_winner_for_plugins() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let child_src = r#"scene "child" {
    extends "base"
    plugin "status" override=#true {
        source "shipped:status"
        mount "floating"
    }
}
"#;
        fs::write(&child_path, child_src).unwrap();
        let doc = parse_scene(child_src, &child_path).unwrap();
        let ctx = SceneSearchCtx::new(&root);
        let frags = load_composition(doc, child_path.clone(), &ctx).unwrap();
        let trace = build_trace(&frags);

        assert_eq!(trace.plugins.len(), 2);
        let child_plugin = trace
            .plugins
            .iter()
            .find(|p| p.fragment_idx == 1)
            .expect("child plugin");
        assert!(child_plugin.override_set, "override=#true should be captured");
        assert!(child_plugin.winner, "child's override should win");
        let base_plugin = trace
            .plugins
            .iter()
            .find(|p| p.fragment_idx == 0)
            .expect("base plugin");
        assert!(!base_plugin.winner, "base plugin should be superseded");
    }

    #[test]
    fn trace_reports_root_clear_directives() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    keybind "Alt p" intent="picker.show"
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let child_src = r#"scene "child" {
    extends "base"
    clear-keybinds "Alt p"
}
"#;
        fs::write(&child_path, child_src).unwrap();
        let doc = parse_scene(child_src, &child_path).unwrap();
        let ctx = SceneSearchCtx::new(&root);
        let frags = load_composition(doc, child_path.clone(), &ctx).unwrap();
        let trace = build_trace(&frags);

        // Clear directive lands in root_clears.
        assert_eq!(trace.root_clears.len(), 1);
        assert!(matches!(
            trace.root_clears[0],
            RootClear::ClearKeybind { ref chord } if chord == "Alt p"
        ));
        // And the base's keybind is flagged cleared.
        let base_kb = trace
            .keybinds
            .iter()
            .find(|k| k.fragment_idx == 0)
            .expect("base kb");
        assert!(base_kb.cleared_by_root);
        assert!(!base_kb.winner);
    }
}
