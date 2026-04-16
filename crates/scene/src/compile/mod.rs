//! Compile pipeline — scene AST → emitted artefacts.
//!
//! The compile stage consumes a validated [`crate::ast::SceneNode`] and
//! renders the derivable artefacts a supervisor needs at spawn time.
//! v1 focuses on the layout pipeline (R3 of `cavekit-scene.md`):
//!
//! * [`layout`]  — lower `LayoutNode` → zellij-compatible KDL string via
//!   the `kdl::KdlDocument` builder API, pruning branches whose `when=`
//!   CEL predicate evaluates to false against the static compile-time
//!   context (R3 + R8).
//!
//! Later tiers wire the reaction / plugin / keybind compile steps here
//! once the op registry (T-4.x) and extension merge pass (T-6.x) land.
//! The on-disk writer (`${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl`)
//! arrives alongside `writer` in T-3.4.

pub mod inject_bus;
pub mod keybinds;
pub mod layout;
pub mod writer;

pub use inject_bus::{
    ARK_BUS_EVENT_PREFIX, ARK_BUS_MOUNT_TARGET, ARK_BUS_PLUGIN_NAME, ARK_BUS_SOURCE,
    maybe_inject_ark_bus,
};
pub use keybinds::{
    DEFAULT_MODE as KEYBIND_DEFAULT_MODE, PIPE_MESSAGE_NAME as KEYBIND_PIPE_MESSAGE_NAME,
    TARGET_PLUGIN as KEYBIND_TARGET_PLUGIN, compile_keybinds,
};
pub use layout::{CompileContext, compile_layout};
pub use writer::{scene_layout_path, write_scene_layout};

use std::path::{Path, PathBuf};

use crate::ast::{LayoutNode, SceneDoc, SceneNode};
use crate::compat::preprocess_file_shape;
use crate::error::SceneError;
use crate::extends::{SceneSearchCtx, ensure_single_extends};
use crate::id::SceneId;
use crate::merge::{ComposedScene, load_composition, merge_fragments};
use miette::NamedSource;

/// Convenience: read a scene file from disk, parse it, compile the
/// enclosed `layout { … }` block, and write the rendered zellij KDL
/// to `${runtime_dir_root}/layouts/{scene-short-hash}-scene.kdl`.
///
/// Returns the rendered layout path alongside the source scene's
/// [`SceneId`] so the caller can thread it into `AgentSpec.scene_path`.
///
/// I/O failure, UTF-8 decode, facet-kdl parse, CEL compile/eval, and
/// write failures all surface through [`SceneError`] variants.
pub fn compile_scene_file(
    scene_file: &Path,
    runtime_dir_root: &Path,
    ctx: &CompileContext,
) -> Result<(PathBuf, SceneId), SceneError> {
    let bytes = std::fs::read(scene_file).map_err(|e| SceneError::Grammar {
        message: format!("read scene `{}`: {e}", scene_file.display()),
        src: NamedSource::new(scene_file.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    let scene_id = SceneId::from_bytes(scene_file.to_path_buf(), &bytes);

    let raw_src = std::str::from_utf8(&bytes).map_err(|e| SceneError::Grammar {
        message: format!("scene `{}` is not valid utf-8: {e}", scene_file.display()),
        src: NamedSource::new(scene_file.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;

    // T-14.1: R15 file-shape detection — auto-wrap legacy layout-only files.
    let shape = preprocess_file_shape(raw_src, scene_file)?;
    let src = shape.as_str();

    let mut doc: SceneDoc = facet_kdl::from_str(src).map_err(|e| SceneError::Parse {
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
        message: e.to_string(),
    })?;

    // T-6.7: auto-inject `plugin "ark-bus" { … }` when the scene
    // declares any keybind, any zellij-side `on` selector, or any
    // plugin's `subscribes` selector targets a zellij-side UserEvent.
    // Skip when the scene already declares ark-bus explicitly. This
    // mutation runs **after parse** + **before compile** so downstream
    // passes see the same plugin set whether the author declared
    // ark-bus or relied on the injection.
    let _injected = maybe_inject_ark_bus(&mut doc.scene);

    let layout = doc.scene.layout.as_ref().ok_or_else(|| SceneError::Grammar {
        message: format!(
            "scene `{}` does not declare a `layout {{ }}` block",
            scene_file.display()
        ),
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
    })?;

    let layout_kdl = compile_layout(layout, ctx)?;
    // T-6.5: when the scene declares any `keybind` nodes, render a
    // sibling `keybinds { … }` block at the TOP of the layout file.
    // Zellij merges keybinds additively with the user's own config when
    // the block is at the layout-file root, so this is the
    // "no-clear-defaults" path called out in cavekit-scene.md R5.
    let keybinds_node = compile_keybinds(&doc.scene.keybinds)?;
    let combined = compose_layout_with_keybinds(&layout_kdl, keybinds_node)?;
    let rendered_path = write_scene_layout(runtime_dir_root, &scene_id, &combined)?;
    Ok((rendered_path, scene_id))
}

/// Composition-aware compile: load the full `extends`/`include`
/// graph, merge per R11, apply clears, then render the merged layout
/// + keybinds to disk.
///
/// Mirrors [`compile_scene_file`] but runs the T-9 composition
/// pipeline first. When the scene has no `extends` and no `include`
/// directives, the merged result is semantically equivalent to the
/// single-file path.
///
/// Returns the rendered layout path, the source scene's [`SceneId`]
/// (keyed off the entry file's bytes, consistent with the
/// pre-composition pipeline so downstream runtime-dir attribution
/// stays stable), and the merged [`ComposedScene`] so downstream
/// compile passes (reactions + plugin lifecycle registration) can
/// iterate every merged contribution.
#[allow(clippy::result_large_err)]
pub fn compile_scene_file_with_composition(
    scene_file: &Path,
    runtime_dir_root: &Path,
    compile_ctx: &CompileContext,
    scene_search: &SceneSearchCtx,
) -> Result<(PathBuf, SceneId, ComposedScene), SceneError> {
    // Step 1: read the entry file + parse.
    let bytes = std::fs::read(scene_file).map_err(|e| SceneError::Grammar {
        message: format!("read scene `{}`: {e}", scene_file.display()),
        src: NamedSource::new(scene_file.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    let scene_id = SceneId::from_bytes(scene_file.to_path_buf(), &bytes);
    let raw_src = std::str::from_utf8(&bytes).map_err(|e| SceneError::Grammar {
        message: format!("scene `{}` is not valid utf-8: {e}", scene_file.display()),
        src: NamedSource::new(scene_file.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;

    // T-14.1: R15 file-shape detection — auto-wrap legacy layout-only files.
    let shape = preprocess_file_shape(raw_src, scene_file)?;
    let src = shape.as_str();

    // One-extends-per-scene check at the raw-KDL layer so duplicate
    // `extends` clauses surface as `scene/multiple-extends` rather
    // than silently getting collapsed by facet-kdl's single-slot
    // child field.
    ensure_single_extends(src, scene_file)?;

    let mut doc: SceneDoc = facet_kdl::from_str(src).map_err(|e| SceneError::Parse {
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
        message: e.to_string(),
    })?;
    let _injected = maybe_inject_ark_bus(&mut doc.scene);

    // Step 2: load full composition graph + merge.
    let fragments = load_composition(doc, scene_file.to_path_buf(), scene_search)?;
    let merged = merge_fragments(fragments)?;

    // Step 3: project the merged state onto a synthetic SceneNode so
    // the existing layout + keybind compile passes stay unchanged.
    // The rendered file uses the merged layout and merged keybinds.
    let synthesized = composed_as_scene_node(&merged);
    let layout = synthesized.layout.as_ref().ok_or_else(|| SceneError::Grammar {
        message: format!(
            "composed scene `{}` does not declare a `layout {{ }}` block",
            scene_file.display()
        ),
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
    })?;

    let layout_kdl = compile_layout(layout, compile_ctx)?;
    let keybinds_node = compile_keybinds(&synthesized.keybinds)?;
    let combined = compose_layout_with_keybinds(&layout_kdl, keybinds_node)?;
    let rendered_path = write_scene_layout(runtime_dir_root, &scene_id, &combined)?;
    Ok((rendered_path, scene_id, merged))
}

/// Project a [`ComposedScene`] onto a minimal [`SceneNode`] carrying
/// only the fields the layout + keybind compile passes read.
///
/// The projection lets us reuse the existing pass code without
/// rewriting it against `ComposedScene` directly — once the full
/// composed-scene surface is consumed by every compile pass (later
/// tier), this helper goes away.
fn composed_as_scene_node(merged: &ComposedScene) -> SceneNode {
    SceneNode {
        name: merged.name.clone(),
        max_cascade_depth: merged.max_cascade_depth,
        extends: None,
        includes: Vec::new(),
        uses: Vec::new(),
        layout: merged.layout.as_ref().map(clone_layout_ref),
        plugins: merged
            .plugins
            .iter()
            .map(crate::merge::clone_plugin_node)
            .collect(),
        ons: merged.reactions.iter().map(crate::merge::clone_on_node).collect(),
        keybinds: merged
            .keybinds
            .iter()
            .map(crate::merge::clone_keybind_node)
            .collect(),
        engine: None,
        clear_reactions: Vec::new(),
        clear_keybinds: Vec::new(),
        disable_plugins: Vec::new(),
    }
}

/// Clone a borrowed [`LayoutNode`] — thin wrapper around the crate-
/// private helper in [`crate::merge`] so this module doesn't have to
/// reach into internals.
fn clone_layout_ref(layout: &LayoutNode) -> LayoutNode {
    crate::merge::clone_layout_node(layout)
}

/// Splice an optional `keybinds { … }` node ABOVE the rendered
/// `layout { }` document. Returns the combined KDL string ready for
/// `write_scene_layout`.
///
/// When `keybinds_node` is `None`, the input layout is returned
/// unchanged (the typical pre-T-6.5 shape). When `Some`, the keybinds
/// node is prepended as a sibling so it sits at the file's top
/// level — zellij requires the `keybinds { }` block to live outside
/// `layout { }` for additive-merge semantics with the user's config.
fn compose_layout_with_keybinds(
    layout_kdl: &str,
    keybinds_node: Option<kdl::KdlNode>,
) -> Result<String, SceneError> {
    let Some(keybinds_node) = keybinds_node else {
        return Ok(layout_kdl.to_string());
    };

    // Re-parse the layout output so we can splice into a real document
    // tree. `compile_layout` already guarantees the output is parseable
    // — the upstream check is duplicated here for paranoia and to
    // surface a clean error if the parser ever changes shape.
    let mut combined = kdl::KdlDocument::parse(layout_kdl).map_err(|e| {
        SceneError::Grammar {
            message: format!("compose_layout_with_keybinds: layout failed to re-parse: {e}"),
            src: NamedSource::new("<compiled-layout>", layout_kdl.to_string()),
            at: (0, layout_kdl.len().min(1)).into(),
        }
    })?;
    // Prepend by inserting at index 0 so the rendered file leads with
    // `keybinds { … }` followed by `layout { … }`. Authors reading the
    // rendered output see "what the bindings are first, then the
    // layout" — matches the cavekit-scene.md R5 example block.
    combined.nodes_mut().insert(0, keybinds_node);
    combined.autoformat();
    Ok(combined.to_string())
}
