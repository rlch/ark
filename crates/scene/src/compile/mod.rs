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

pub mod layout;
pub mod writer;

pub use layout::{CompileContext, compile_layout};
pub use writer::{scene_layout_path, write_scene_layout};

use std::path::{Path, PathBuf};

use crate::ast::SceneDoc;
use crate::error::SceneError;
use crate::id::SceneId;
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

    let src = std::str::from_utf8(&bytes).map_err(|e| SceneError::Grammar {
        message: format!("scene `{}` is not valid utf-8: {e}", scene_file.display()),
        src: NamedSource::new(scene_file.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;

    let doc: SceneDoc = facet_kdl::from_str(src).map_err(|e| SceneError::Parse {
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
        message: e.to_string(),
    })?;

    let layout = doc.scene.layout.as_ref().ok_or_else(|| SceneError::Grammar {
        message: format!(
            "scene `{}` does not declare a `layout {{ }}` block",
            scene_file.display()
        ),
        src: NamedSource::new(scene_file.display().to_string(), src.to_string()),
        at: (0, src.len().min(1)).into(),
    })?;

    let kdl = compile_layout(layout, ctx)?;
    let rendered_path = write_scene_layout(runtime_dir_root, &scene_id, &kdl)?;
    Ok((rendered_path, scene_id))
}
