//! Rendered-KDL writer for `${XDG_RUNTIME_DIR}/ark/layouts/{id}-{tab}.kdl`.
//!
//! Implements cavekit-mux-zellij.md R5 / cavekit-layouts.md R3 (T-031):
//!
//! - **`.kdl` extension is mandatory** (zellij issue #4994 silently
//!   ignores other extensions when invoked with `--layout`).
//! - **Strict permissions:** parent dir 0700, file 0600. Layouts may
//!   carry secrets via env vars or interpolated paths; lock them down.
//! - **`cleanup_rendered` is idempotent** — best-effort, no error on
//!   missing files. Tab close is allowed to fire it twice.
//!
//! The caller supplies the runtime-dir root (typically resolved via
//! `ark_types::EnvPaths`), so this module stays free of process-env
//! dependencies and is fully testable with `tempfile`.

use ark_types::SessionId;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LayoutWriteError {
    #[error("layout output path must end in .kdl (zellij issue #4994): {0:?}")]
    InvalidExtension(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Per-session per-tab rendered-KDL cache dir: `${runtime_dir}/ark/layouts/`.
/// Caller supplies the runtime_dir root (typically from ark_types::EnvPaths).
pub fn rendered_layouts_dir(runtime_dir_root: &Path) -> PathBuf {
    runtime_dir_root.join("layouts")
}

/// Compute `{runtime}/ark/layouts/{id}-{tab}.kdl` where `{id}` is the
/// session's path-leaf form (`<name>-<ulid>`).
pub fn rendered_layout_path(runtime_dir_root: &Path, id: &SessionId, tab: &str) -> PathBuf {
    rendered_layouts_dir(runtime_dir_root).join(format!("{}-{}.kdl", id.as_path_leaf(), tab))
}

/// Write a rendered layout with strict `.kdl` extension enforcement + 0600 mode.
/// Parent dir is created with 0700 if missing.
pub fn write_rendered(path: &Path, contents: &str) -> Result<(), LayoutWriteError> {
    let ext_ok = path.extension().and_then(|e| e.to_str()) == Some("kdl");
    if !ext_ok {
        return Err(LayoutWriteError::InvalidExtension(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(path, contents)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Cleanup on tab close — best-effort, idempotent.
pub fn cleanup_rendered(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => tracing::debug!(?path, "rendered layout removed"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(?path, %err, "rendered layout cleanup failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn dummy_id() -> SessionId {
        SessionId::parse("cavekit_auth-01jx7z8k6x9y2zt4abcdef0123").expect("parse")
    }

    #[test]
    fn rendered_layout_path_composes_expected_filename() {
        let root = Path::new("/run/ark");
        let id = dummy_id();
        let p = rendered_layout_path(root, &id, "builder");
        assert_eq!(
            p,
            PathBuf::from("/run/ark/layouts/cavekit_auth-01jx7z8k6x9y2zt4abcdef0123-builder.kdl")
        );
    }

    #[test]
    fn rendered_layouts_dir_appends_layouts() {
        assert_eq!(
            rendered_layouts_dir(Path::new("/run/ark")),
            PathBuf::from("/run/ark/layouts")
        );
    }

    #[test]
    fn write_rendered_rejects_non_kdl_extension() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("layouts").join("oops.txt");
        let err = write_rendered(&bad, "layout {}").unwrap_err();
        assert!(
            matches!(err, LayoutWriteError::InvalidExtension(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn write_rendered_creates_parent_and_sets_0600() {
        let dir = tempdir().unwrap();
        let id = dummy_id();
        let p = rendered_layout_path(dir.path(), &id, "builder");
        write_rendered(&p, "layout { tab { pane } }").unwrap();

        assert!(p.is_file());
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let parent_mode = std::fs::metadata(p.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);

        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("layout"));
    }

    #[test]
    fn cleanup_rendered_removes_existing_file() {
        let dir = tempdir().unwrap();
        let id = dummy_id();
        let p = rendered_layout_path(dir.path(), &id, "review");
        write_rendered(&p, "layout {}").unwrap();
        assert!(p.is_file());
        cleanup_rendered(&p);
        assert!(!p.exists());
    }

    #[test]
    fn cleanup_rendered_idempotent_on_missing_path() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("layouts").join("nope.kdl");
        // Must not panic / not error.
        cleanup_rendered(&p);
        cleanup_rendered(&p);
    }

    // ------- T-122: additional .kdl validation edge cases -------

    /// Path with no extension at all must be rejected. Zellij issue
    /// #4994 silently ignores such layouts, so the writer MUST bounce
    /// them at the Rust boundary.
    #[test]
    fn write_rendered_rejects_path_with_no_extension() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("layouts").join("no_ext_here");
        let err = write_rendered(&bad, "layout {}").unwrap_err();
        assert!(
            matches!(err, LayoutWriteError::InvalidExtension(_)),
            "got: {err:?}"
        );
        assert!(
            !bad.exists(),
            "no file should be created for a rejected path"
        );
    }

    /// Only the FINAL extension is inspected — `layout.kdl.bak` is
    /// `.bak`, not `.kdl`, and must be rejected. Guards against a
    /// common backup-file naming convention slipping through.
    #[test]
    fn write_rendered_rejects_double_extension_where_final_is_not_kdl() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("layouts").join("layout.kdl.bak");
        let err = write_rendered(&bad, "layout {}").unwrap_err();
        assert!(
            matches!(err, LayoutWriteError::InvalidExtension(_)),
            "got: {err:?}"
        );
    }
}
