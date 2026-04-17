//! Scene resolution + compile → layout artifact helper.
//!
//! Pulled out of `run()` so that `run_with()` can be driven by the
//! test multiplexer/spawner without duplicating the compile pipeline.
//! Still pure: reads the scene file, parses, composes, compiles, and
//! writes the lowered zellij KDL artifact to disk. Returns the
//! artifact path + the resolved scene file path (propagated into the
//! `AgentSpec` so the supervisor can hot-reload from the same source).

use std::path::{Path, PathBuf};

use ark_scene::ast::SceneBodyNode;
use ark_scene::compile::{compile_layout_kdl, compile_scene, write_layout_artifact};
use ark_scene::compose::compose_scene;
use ark_scene::parse::parse_scene;
use ark_scene::rhai::Engine;
use ark_scene::shape::detect_and_normalize;
use ark_scene::view::{RenderMode, ViewMeta, ViewRegistry, ViewSource};

use crate::error::CliError;

/// Resolved scene file + compiled layout artifact, ready for use by
/// the multiplexer and the supervisor spec.
pub(super) struct CompiledLayout {
    /// Absolute path to the written zellij KDL layout artifact.
    /// `None` when no scene file was resolved at any rung (bare-ark
    /// fallthrough — zellij's own default layout is used).
    pub layout_path: Option<PathBuf>,
}

/// Compile the scene at `scene_file` into a zellij layout artifact.
///
/// Returns `Ok(CompiledLayout { layout_path: None })` when
/// `scene_file` is `None` — the caller relies on zellij's built-in
/// default layout.
pub(super) fn compile_scene_to_layout(
    scene_file: Option<&Path>,
) -> Result<CompiledLayout, CliError> {
    let Some(path) = scene_file else {
        return Ok(CompiledLayout { layout_path: None });
    };

    if !path.exists() {
        return Err(CliError::NotFound {
            what: format!("scene file `{}`", path.display()),
        });
    }

    let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
        reason: format!("read scene `{}`: {e}", path.display()),
    })?;
    let normalized = detect_and_normalize(&src, path).map_err(|e| CliError::Generic {
        reason: format!("scene shape `{}`: {e}", path.display()),
    })?;
    let ir = parse_scene(&normalized, path).map_err(|e| CliError::Generic {
        reason: format!("parse scene `{}`: {e}", path.display()),
    })?;
    let ir = compose_scene(ir).map_err(|e| CliError::Generic {
        reason: format!("compose scene `{}`: {e}", path.display()),
    })?;
    let engine = Engine::new();
    let compiled = compile_scene(&engine, ir).map_err(|e| CliError::Generic {
        reason: format!("compile scene `{}`: {e}", path.display()),
    })?;

    let layout = compiled
        .ir
        .scene
        .body
        .iter()
        .find_map(|node| {
            if let SceneBodyNode::Layout(l) = node {
                Some(l)
            } else {
                None
            }
        })
        .ok_or_else(|| CliError::Generic {
            reason: format!("scene `{}` has no layout block", path.display()),
        })?;

    // Populate the registry with primitives + shipped views.
    // Shipped views (status, picker) are ark-bundled and always
    // available; user/project extensions land in a future tier once
    // the extension loader is wired to the CLI context.
    let mut registry = ViewRegistry::with_primitives();
    registry.register(ViewMeta {
        name: "status".to_string(),
        source: ViewSource::Shipped,
        render_mode: RenderMode::ZellijView,
        config_schema: None,
    });
    registry.register(ViewMeta {
        name: "picker".to_string(),
        source: ViewSource::Shipped,
        render_mode: RenderMode::ZellijView,
        config_schema: None,
    });
    let kdl_doc = compile_layout_kdl(layout, &registry).map_err(|e| CliError::Generic {
        reason: format!("layout compile `{}`: {e}", path.display()),
    })?;
    let artifact_path =
        write_layout_artifact(&kdl_doc, &compiled.ir.id).map_err(|e| CliError::Generic {
            reason: format!("write layout artifact: {e}"),
        })?;

    Ok(CompiledLayout {
        layout_path: Some(artifact_path),
    })
}
