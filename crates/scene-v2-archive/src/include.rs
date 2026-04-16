//! `include "<path>"` splicing with cycle detection (T-9.2, R11).
//!
//! `include` is a textual splice directive: at the include's source
//! position, the contents of another KDL fragment are merged into the
//! current scene. Unlike `extends` (which goes through the
//! scene-search-path), `include` resolves the argument as a filesystem
//! path **relative to the current scene file**. Multiple `include`
//! entries are allowed per scene; they are applied in source order per
//! R11's "includes in source position within the current scene file"
//! clause.
//!
//! # Cycle detection
//!
//! The cycle check is unified with [`crate::extends::load_extends`] at
//! the composition-loader layer (see [`crate::merge::load_composition`]):
//! a single `HashSet<PathBuf>` of canonical visited paths spans both
//! graphs, so `child extends A; A includes B; B extends child` surfaces
//! as [`SceneError::IncludeCycle`] regardless of which edge closed the
//! loop. The surfaced diagnostic carries the full trail of hops from
//! the entry file through the closing edge.
//!
//! `include` paths are canonicalised via [`dunce::canonicalize`]
//! equivalent — we use `Path::canonicalize` directly, and when that
//! fails (the file does not exist) we surface [`SceneError::Grammar`]
//! keyed by the user-provided relative path. That mapping mirrors the
//! rest of the pipeline's I/O-error handling.

use std::path::{Path, PathBuf};

use miette::NamedSource;

use crate::ast::{IncludeNode, SceneDoc};
use crate::error::SceneError;
use crate::parse::parse_scene;

/// Resolve an `include` declaration against the current scene file's
/// directory.
///
/// Returns the canonical absolute path on success. Errors surface as
/// [`SceneError::Grammar`] (relative path doesn't resolve, or the
/// filesystem canonical call fails because the file doesn't exist).
///
/// Pure function: the only I/O is the `canonicalize` call, which
/// `std` does not let us fake. Tests exercise this via tempdirs.
#[allow(clippy::result_large_err)]
pub fn resolve_include(
    decl: &IncludeNode,
    current_path: &Path,
) -> Result<PathBuf, SceneError> {
    // "Relative to the current scene file" — base is the PARENT
    // directory of the current file. When the current path is a
    // built-in synthetic path (no parent), we root the lookup at the
    // process CWD as a last resort (rare path, mostly tests).
    let base = current_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let candidate = base.join(&decl.path);
    candidate.canonicalize().map_err(|e| SceneError::Grammar {
        message: format!(
            "include `{}` could not be resolved relative to `{}`: {e}",
            decl.path,
            current_path.display()
        ),
        src: NamedSource::new(current_path.display().to_string(), String::new()),
        at: (0, 0).into(),
    })
}

/// Load the fragment referenced by an `include` declaration.
///
/// Reads the file off disk, parses via [`parse_scene`], and returns the
/// resulting [`SceneDoc`] along with its canonical path. The caller
/// is responsible for merging the fragment's reactions / keybinds /
/// plugins into the parent scene per R11.
///
/// Errors as [`SceneError::Grammar`] on I/O or UTF-8 failure;
/// [`SceneError::Parse`] when the fragment exists but is malformed.
#[allow(clippy::result_large_err)]
pub fn load_include(
    decl: &IncludeNode,
    current_path: &Path,
) -> Result<(SceneDoc, PathBuf), SceneError> {
    let resolved = resolve_include(decl, current_path)?;
    let bytes = std::fs::read(&resolved).map_err(|e| SceneError::Grammar {
        message: format!("read include `{}`: {e}", resolved.display()),
        src: NamedSource::new(resolved.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    let src = String::from_utf8(bytes).map_err(|e| SceneError::Grammar {
        message: format!("include `{}` is not valid utf-8: {e}", resolved.display()),
        src: NamedSource::new(resolved.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    let parsed = parse_scene(&src, &resolved)?;
    Ok((parsed, resolved))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn include_node(path: &str) -> IncludeNode {
        IncludeNode {
            path: path.to_string(),
        }
    }

    /// Rung: `include "sibling.kdl"` resolves relative to the parent
    /// scene's directory.
    #[test]
    fn resolve_sibling_include() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let parent = root.join("scene.kdl");
        let sibling = root.join("sibling.kdl");
        fs::write(&parent, r#"scene "p""#).unwrap();
        fs::write(&sibling, r#"scene "s""#).unwrap();

        let resolved = resolve_include(&include_node("sibling.kdl"), &parent)
            .expect("resolves sibling");
        assert_eq!(resolved, sibling.canonicalize().unwrap());
    }

    /// Subdirectory includes work the same way.
    #[test]
    fn resolve_subdir_include() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let parent = root.join("scene.kdl");
        let nested = sub.join("frag.kdl");
        fs::write(&parent, r#"scene "p""#).unwrap();
        fs::write(&nested, r#"scene "f""#).unwrap();

        let resolved = resolve_include(&include_node("sub/frag.kdl"), &parent)
            .expect("resolves subdir");
        assert_eq!(resolved, nested.canonicalize().unwrap());
    }

    /// Missing target file → `SceneError::Grammar` (mirrors the rest
    /// of the compile pipeline's I/O-error mapping).
    #[test]
    fn missing_include_is_grammar_error() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let parent = root.join("scene.kdl");
        fs::write(&parent, r#"scene "p""#).unwrap();

        let err = resolve_include(&include_node("missing.kdl"), &parent)
            .expect_err("missing target must error");
        assert!(matches!(err, SceneError::Grammar { .. }));
    }

    /// `load_include` reads + parses the target file.
    #[test]
    fn load_include_parses_target() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let parent = root.join("scene.kdl");
        let child = root.join("frag.kdl");
        fs::write(&parent, r#"scene "p""#).unwrap();
        fs::write(&child, r#"scene "f""#).unwrap();

        let (loaded, loaded_path) =
            load_include(&include_node("frag.kdl"), &parent).expect("load frag");
        assert_eq!(loaded.scene.name, "f");
        assert_eq!(loaded_path, child.canonicalize().unwrap());
    }
}
