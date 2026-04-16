//! Write a compiled scene layout to disk at
//! `${XDG_RUNTIME_DIR}/ark/layouts/{scene-id}-scene.kdl`.
//!
//! Implements `cavekit-scene.md` R3 (layout emission) and the
//! on-disk half of R15 (scene-vs-legacy file shape). The writer
//! mirrors the policy enforced by
//! [`ark_mux_zellij::layout_writer::write_rendered`] — `.kdl`
//! extension mandatory (zellij issue #4994 silently ignores other
//! extensions), parent dir 0700, file 0600 because rendered layouts
//! may inline per-agent secrets through env vars or substituted
//! paths.
//!
//! # Why not depend on `ark_mux_zellij::layout_writer` directly?
//!
//! Scene is intentionally a **leaf** in the workspace dep graph —
//! `mux`, `core`, and `supervisor` depend on it, not the other way
//! round (see `crates/scene/src/intent.rs` module docs). Reusing
//! `ark_mux_zellij`'s writer would invert that topology. The policy
//! surface is small enough that duplicating the three-rule invariant
//! here is cheaper than refactoring a shared writer crate, and the
//! two implementations share a testable axiom: file ends in `.kdl`,
//! mode 0600, parent 0700.
//!
//! # Path shape
//!
//! ```text
//! {runtime_dir_root}/layouts/{scene-short-hash}-scene.kdl
//! ```
//!
//! Where `scene-short-hash` is [`SceneId::short_hash`] — the first
//! 8 hex chars of the blake3 content hash that already identifies the
//! source scene file. Using the hash rather than the path basename
//! guarantees:
//!
//! * Different scene contents at the same path produce different
//!   rendered files — the compile cache (keyed on `SceneId`) stays
//!   in sync with the filesystem.
//! * The filename is always a filesystem-safe, bounded-length token,
//!   independent of the author's choice of scene file name or path.
//! * The `-scene` suffix distinguishes scene-compiled layouts from
//!   the legacy `{agent-id}-{tab}.kdl` rendered files that
//!   `ark_mux_zellij` writes for non-scene spawns.
//!
//! # Validation
//!
//! Before writing, the KDL is re-parsed via
//! [`kdl::KdlDocument::parse`]. The compile layer already validates
//! via the same check, but centralising it here means callers that
//! bypass `compile_layout` (e.g. future `ark scene render` CLI) also
//! get a definitive parse guard.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use kdl::KdlDocument;
use miette::NamedSource;

use crate::error::SceneError;
use crate::id::SceneId;

/// Runtime-dir subdirectory that holds all rendered scene layouts.
const LAYOUTS_SUBDIR: &str = "layouts";

/// Compute the canonical on-disk path for a compiled scene layout.
///
/// `runtime_dir_root` is the resolved `${XDG_RUNTIME_DIR}/ark-{uid}`
/// directory (supplied by [`ark_types::EnvPaths::runtime_dir`]).
/// Callers that need to look up the path without triggering a write
/// — e.g. cleanup, hot-reload cache invalidation — use this helper
/// directly.
pub fn scene_layout_path(runtime_dir_root: &Path, scene_id: &SceneId) -> PathBuf {
    runtime_dir_root
        .join(LAYOUTS_SUBDIR)
        .join(format!("{}-scene.kdl", scene_id.short_hash()))
}

/// Write a compiled scene layout to `${runtime_dir_root}/layouts/
/// {scene-short-hash}-scene.kdl` with 0600 perms, and return the
/// absolute path.
///
/// The contents are re-parsed with [`kdl::KdlDocument::parse`]
/// before any filesystem I/O so malformed input never lands on
/// disk.
///
/// The parent directory is created with 0700 if it does not exist;
/// existing directories have their perms reset to 0700 so the
/// invariant holds across successive writes.
pub fn write_scene_layout(
    runtime_dir_root: &Path,
    scene_id: &SceneId,
    kdl: &str,
) -> Result<PathBuf, SceneError> {
    // Parse guard — upstream compilers already do this, but
    // centralising here covers any future code path that bypasses
    // `compile_layout`.
    KdlDocument::parse(kdl).map_err(|e| SceneError::Parse {
        src: NamedSource::new("<compiled-layout>", kdl.to_string()),
        at: (0, kdl.len().min(1)).into(),
        message: e.to_string(),
    })?;

    let path = scene_layout_path(runtime_dir_root, scene_id);
    write_with_strict_perms(&path, kdl).map_err(|e| SceneError::Grammar {
        message: format!("failed to write compiled scene layout: {e}"),
        src: NamedSource::new(path.display().to_string(), kdl.to_string()),
        at: (0, 0).into(),
    })?;
    Ok(path)
}

/// Write `contents` to `path` with the invariants documented at the
/// module level: `.kdl` extension enforced, parent dir 0700, file
/// 0600. Mirrors
/// [`ark_mux_zellij::layout_writer::write_rendered`].
fn write_with_strict_perms(path: &Path, contents: &str) -> io::Result<()> {
    let ext_ok = path.extension().and_then(|e| e.to_str()) == Some("kdl");
    if !ext_ok {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "layout output path must end in .kdl (zellij issue #4994): {}",
                path.display()
            ),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_scene_id() -> SceneId {
        // Deterministic: fixed path + fixed bytes → fixed hash.
        SceneId::from_bytes(PathBuf::from("/fake/scene.kdl"), b"hello world")
    }

    /// Happy path: write a minimal valid KDL layout, re-parse, check
    /// perms + path shape.
    #[test]
    fn write_scene_layout_creates_file_with_0600_perms() {
        let dir = tempfile::tempdir().unwrap();
        let id = fixture_scene_id();
        let kdl = "layout { tab name=\"work\" { pane } }";

        let path = write_scene_layout(dir.path(), &id, kdl).expect("write");
        assert!(path.is_file(), "file not created: {path:?}");

        // Filename format.
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()).unwrap(),
            format!("{}-scene.kdl", id.short_hash())
        );

        // Mode 0600.
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");

        // Parent dir 0700.
        let parent_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "expected 0700, got {parent_mode:o}");

        // Contents on disk re-parse as KDL.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        let _ = KdlDocument::parse(&on_disk).expect("round-trip parse");
    }

    /// Path helper composes the expected shape without I/O.
    #[test]
    fn scene_layout_path_composes_expected_filename() {
        let id = fixture_scene_id();
        let p = scene_layout_path(Path::new("/run/ark"), &id);
        assert_eq!(
            p,
            PathBuf::from("/run/ark/layouts").join(format!("{}-scene.kdl", id.short_hash()))
        );
    }

    /// Malformed KDL is rejected before anything hits the disk.
    #[test]
    fn malformed_kdl_never_touches_disk() {
        use crate::error::ErrorCode;
        let dir = tempfile::tempdir().unwrap();
        let id = fixture_scene_id();
        // Unbalanced braces — definitely not valid KDL.
        let bad = "layout { tab {";

        let err = write_scene_layout(dir.path(), &id, bad).expect_err("should reject");
        assert_eq!(err.code_enum(), ErrorCode::Parse);

        // No file was written.
        let path = scene_layout_path(dir.path(), &id);
        assert!(
            !path.exists(),
            "invalid KDL should not leave any artefact on disk: {path:?}"
        );
    }

    /// Two different scene contents produce distinct on-disk files —
    /// the SceneId's hash drives the filename, not the source path.
    #[test]
    fn distinct_scene_ids_write_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        let id_a = SceneId::from_bytes(PathBuf::from("/x.kdl"), b"a");
        let id_b = SceneId::from_bytes(PathBuf::from("/x.kdl"), b"b");
        let kdl = "layout { }";

        let a = write_scene_layout(dir.path(), &id_a, kdl).unwrap();
        let b = write_scene_layout(dir.path(), &id_b, kdl).unwrap();
        assert_ne!(a, b, "different scene bytes → different output paths");
    }

    /// Re-writing the same scene is idempotent — second call overwrites
    /// cleanly and leaves identical content.
    #[test]
    fn rewriting_same_scene_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let id = fixture_scene_id();
        let kdl = "layout { }";

        let first = write_scene_layout(dir.path(), &id, kdl).unwrap();
        let second = write_scene_layout(dir.path(), &id, kdl).unwrap();
        assert_eq!(first, second);
        let contents = std::fs::read_to_string(&second).unwrap();
        assert!(contents.contains("layout"));
    }
}
