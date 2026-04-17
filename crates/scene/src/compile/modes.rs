//! Mode layout pre-rendering (T-045).
//!
//! Scene modes are named alternate whole-tab layouts declared as
//! `mode "<name>" { tab @handle { … } }` (R9.11). Each mode is
//! pre-rendered to a standalone zellij KDL artifact at
//! `${XDG_RUNTIME_DIR}/ark/layouts/<id-hash>-mode-<name>.kdl`. The
//! reconciler swaps between the base layout and modes via
//! `zellij action override-layout --apply-only-to-active-tab` (T-046).
//!
//! # Handle preservation (R9.12)
//!
//! Modes share the scene's flat handle namespace with the base layout,
//! so handles that appear in both resolve to the *same* subprocess at
//! runtime — zellij's override-layout matching is by `(command, args)`
//! tuple, and the `ARK_HANDLE=@<handle>` env wrapper guarantees uniqueness
//! per handle. Callers don't need to do anything special; they just author
//! `pane @review { shell }` in both the base layout and a `review` mode
//! and the same shell survives the swap.

// See compile/layout.rs — the scene error enum is intentionally heavy.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::KdlDocument;

use crate::ast::{LayoutNode, ModeNode, SceneBodyNode};
use crate::compile::layout::{compile_layout_kdl, id_slug, layouts_dir};
use crate::error::SceneError;
use crate::id::SceneId;
use crate::parse::SceneIR;
use crate::view::ViewRegistry;

/// Pre-render every `mode "<name>" { … }` block in the scene to its own
/// zellij-compatible [`KdlDocument`].
///
/// Returned map keys are mode names (e.g. `"review"`, `"debug"`). The
/// keys are sorted lexicographically (the underlying `BTreeMap`) so
/// the artifact enumeration is stable across runs.
///
/// Modes with duplicate names in the scene source are resolved
/// last-wins — the parser preserves textual order and the `BTreeMap`
/// assignment here simply overwrites prior entries. If strict
/// duplicate-name rejection is desired, callers should run a pre-pass
/// before invoking this compiler.
#[allow(clippy::result_large_err)]
pub fn compile_modes(
    ir: &SceneIR,
    registry: &ViewRegistry,
) -> Result<BTreeMap<String, KdlDocument>, SceneError> {
    let mut out = BTreeMap::new();
    for node in &ir.scene.body {
        if let SceneBodyNode::Mode(mode) = node {
            let doc = compile_mode(mode, registry)?;
            out.insert(mode.name.clone(), doc);
        }
    }
    Ok(out)
}

/// Lower a single mode (one or more `tab @handle { … }` blocks) into a
/// zellij-compatible [`KdlDocument`]. Wraps the tabs in a synthetic
/// `LayoutNode` and hands off to [`compile_layout_kdl`].
#[allow(clippy::result_large_err)]
fn compile_mode(mode: &ModeNode, registry: &ViewRegistry) -> Result<KdlDocument, SceneError> {
    let synthetic = LayoutNode {
        tabs: mode.tabs.clone(),
    };
    compile_layout_kdl(&synthetic, registry)
}

/// Write every mode in `modes` to its own artifact file under
/// `${XDG_RUNTIME_DIR}/ark/layouts/<id-hash>-mode-<name>.kdl`.
///
/// - File mode `0600`, parent dir `0700`.
/// - Re-parses the serialised KDL before returning (round-trip guard
///   mirrors [`crate::compile::write_layout_artifact`]).
pub fn write_mode_artifacts(
    modes: &BTreeMap<String, KdlDocument>,
    scene_id: &SceneId,
) -> Result<BTreeMap<String, PathBuf>, std::io::Error> {
    write_mode_artifacts_in(modes, scene_id, &layouts_dir())
}

/// [`write_mode_artifacts`] with a caller-provided directory. Used by
/// tests for isolation against `XDG_RUNTIME_DIR`.
pub fn write_mode_artifacts_in(
    modes: &BTreeMap<String, KdlDocument>,
    scene_id: &SceneId,
    dir: &std::path::Path,
) -> Result<BTreeMap<String, PathBuf>, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    set_mode(dir, 0o700)?;

    let mut out = BTreeMap::new();
    for (name, doc) in modes {
        let filename = format!("{}-mode-{}.kdl", id_slug(scene_id), sanitise_mode_name(name));
        let path = dir.join(filename);
        let text = doc.to_string();
        if let Err(e) = KdlDocument::parse_v2(&text) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("mode `{name}` KDL does not re-parse: {e}"),
            ));
        }
        std::fs::write(&path, &text)?;
        set_mode(&path, 0o600)?;
        out.insert(name.clone(), path);
    }
    Ok(out)
}

fn sanitise_mode_name(name: &str) -> String {
    // Keep the filename filesystem-safe without needing shell quoting.
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::layout::{LayoutChild, PaneNode, TabNode, ViewRef};
    use crate::ast::{ModeNode, SceneNode};
    use crate::id::SceneId;
    use crate::parse::SceneIR;

    fn ir_with_mode(name: &str) -> SceneIR {
        let mode = ModeNode {
            name: name.to_string(),
            tabs: vec![TabNode {
                handle: "@main".to_string(),
                cwd: None,
                name: None,
                focus: None,
                when: None,
                body: vec![LayoutChild::Pane(PaneNode {
                    handle: "@p".to_string(),
                    span: None,
                    cells: None,
                    min: None,
                    max: None,
                    when: None,
                    overlay: None,
                    view: ViewRef {
                        alias: "shell".to_string(),
                        config_block: None,
                    },
                })],
            }],
        };

        let scene = SceneNode {
            name: "x".to_string(),
            max_cascade_depth: None,
            body: vec![SceneBodyNode::Mode(mode)],
        };

        SceneIR {
            scene,
            path: PathBuf::from("test.kdl"),
            src: String::new(),
            id: SceneId::new("test.kdl", b"body"),
            kdl_doc: None,
        }
    }

    #[test]
    fn compile_modes_picks_up_every_mode_block() {
        let ir = ir_with_mode("review");
        let modes = compile_modes(&ir, &ViewRegistry::with_primitives()).unwrap();
        assert!(modes.contains_key("review"));
        assert_eq!(modes.len(), 1);
    }

    #[test]
    fn mode_layout_contains_ark_handle_wrapper() {
        let ir = ir_with_mode("debug");
        let modes = compile_modes(&ir, &ViewRegistry::with_primitives()).unwrap();
        let text = modes.get("debug").unwrap().to_string();
        assert!(text.contains("ARK_HANDLE"));
    }

    #[test]
    fn write_mode_artifacts_writes_one_file_per_mode() {
        let ir = ir_with_mode("review");
        let modes = compile_modes(&ir, &ViewRegistry::with_primitives()).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded in test scope; env mutation OK.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
        }

        let paths = write_mode_artifacts(&modes, &ir.id).expect("write modes");
        assert_eq!(paths.len(), 1);
        let p = &paths["review"];
        assert!(p.exists());
        let text = std::fs::read_to_string(p).unwrap();
        KdlDocument::parse_v2(&text).expect("mode file must re-parse");
        assert!(p.file_name().unwrap().to_string_lossy().contains("mode-review"));
    }
}
