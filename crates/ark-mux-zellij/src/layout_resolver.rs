//! Layout stem-or-path resolver for zellij KDL layouts.
//!
//! Resolution order (cavekit-mux-zellij R5, cavekit-layouts R1):
//!   1. If the input looks like an explicit path (contains `/` or ends in
//!      `.kdl`), treat it as a direct path. It MUST end in `.kdl`
//!      (zellij issue #4994 silently ignores other extensions when passed
//!      to `--layout`) and MUST exist.
//!   2. Otherwise it's a stem:
//!      a. User override at `{user_root}/{stem}.kdl`.
//!      b. Embedded shipped layout (`SHIPPED_LAYOUTS`).
//!      c. Error with the list of available stems.

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Layouts shipped embedded in the binary. Keys match stem names used by
/// the `--layout` CLI flag and orchestrator defaults.
pub const SHIPPED_LAYOUTS: &[(&str, &str)] = &[
    ("builder", include_str!("../layouts/builder.kdl")),
    ("classic", include_str!("../layouts/classic.kdl")),
    ("focused", include_str!("../layouts/focused.kdl")),
    (
        "triple-column",
        include_str!("../layouts/triple-column.kdl"),
    ),
    ("review", include_str!("../layouts/review.kdl")),
    ("log", include_str!("../layouts/log.kdl")),
];

/// Result of resolving a layout identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutSource {
    /// User override file from `{user_root}/{stem}.kdl`.
    User { path: PathBuf, contents: String },
    /// Embedded shipped layout from `SHIPPED_LAYOUTS`.
    Embedded { stem: String, contents: String },
    /// Explicit path provided by the caller.
    Path { path: PathBuf, contents: String },
}

#[derive(Debug, Error)]
pub enum LayoutResolveError {
    #[error("layout stem `{stem}` not found. Available: {available}")]
    NotFound { stem: String, available: String },
    #[error("layout path must end in .kdl (zellij issue #4994): {0:?}")]
    InvalidExtension(PathBuf),
    #[error("layout file does not exist: {0:?}")]
    PathMissing(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Resolves a layout stem or path to a `LayoutSource`.
#[derive(Debug, Clone)]
pub struct LayoutResolver {
    /// Root of user layouts (typically `$XDG_CONFIG_HOME/ark/layouts/`).
    user_root: Option<PathBuf>,
}

impl LayoutResolver {
    pub fn new(user_root: Option<PathBuf>) -> Self {
        Self { user_root }
    }

    /// Resolve a stem-or-path:
    ///
    /// - If input contains `/` OR ends in `.kdl`: treat as an explicit path
    ///   (must exist and end in `.kdl`).
    /// - Otherwise treat as a stem: user-root first, then shipped.
    pub fn resolve(&self, input: &str) -> Result<LayoutSource, LayoutResolveError> {
        let looks_like_path = input.contains('/') || input.ends_with(".kdl");

        if looks_like_path {
            let path = PathBuf::from(input);
            return resolve_path(&path);
        }

        // Treat as stem.
        if let Some(root) = &self.user_root {
            let user_path = root.join(format!("{}.kdl", input));
            if user_path.is_file() {
                let contents = std::fs::read_to_string(&user_path)?;
                return Ok(LayoutSource::User {
                    path: user_path,
                    contents,
                });
            }
        }

        for (stem, contents) in SHIPPED_LAYOUTS {
            if *stem == input {
                return Ok(LayoutSource::Embedded {
                    stem: (*stem).to_string(),
                    contents: (*contents).to_string(),
                });
            }
        }

        Err(LayoutResolveError::NotFound {
            stem: input.to_string(),
            available: self.available_stems().join(", "),
        })
    }

    /// Sorted list of stems the resolver can find: shipped plus any user
    /// overrides present on disk.
    pub fn available_stems(&self) -> Vec<String> {
        let mut stems: Vec<String> = SHIPPED_LAYOUTS
            .iter()
            .map(|(s, _)| (*s).to_string())
            .collect();

        if let Some(root) = &self.user_root
            && let Ok(read) = std::fs::read_dir(root)
        {
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("kdl")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                    && !stems.iter().any(|s| s == stem)
                {
                    stems.push(stem.to_string());
                }
            }
        }

        stems.sort();
        stems
    }
}

fn resolve_path(path: &Path) -> Result<LayoutSource, LayoutResolveError> {
    if path.extension().and_then(|e| e.to_str()) != Some("kdl") {
        return Err(LayoutResolveError::InvalidExtension(path.to_path_buf()));
    }
    if !path.is_file() {
        return Err(LayoutResolveError::PathMissing(path.to_path_buf()));
    }
    let contents = std::fs::read_to_string(path)?;
    Ok(LayoutSource::Path {
        path: path.to_path_buf(),
        contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolve_shipped_stem_returns_embedded() {
        let r = LayoutResolver::new(None);
        let got = r.resolve("builder").unwrap();
        match got {
            LayoutSource::Embedded { stem, contents } => {
                assert_eq!(stem, "builder");
                assert!(contents.contains("layout"));
            }
            other => panic!("expected Embedded, got {other:?}"),
        }
    }

    #[test]
    fn resolve_unknown_stem_returns_notfound_with_list() {
        let r = LayoutResolver::new(None);
        let err = r.resolve("nonexistent").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"), "got: {msg}");
        // Each shipped stem must appear in the listing.
        for (stem, _) in SHIPPED_LAYOUTS {
            assert!(msg.contains(stem), "missing {stem} in: {msg}");
        }
    }

    #[test]
    fn resolve_explicit_kdl_path_returns_path_variant() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("custom.kdl");
        fs::write(&p, "layout { tab { pane } }").unwrap();

        let r = LayoutResolver::new(None);
        let got = r.resolve(p.to_str().unwrap()).unwrap();
        match got {
            LayoutSource::Path { path, contents } => {
                assert_eq!(path, p);
                assert!(contents.contains("layout"));
            }
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn resolve_non_kdl_path_rejects() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("custom.txt");
        fs::write(&p, "layout { tab { pane } }").unwrap();

        let r = LayoutResolver::new(None);
        let err = r.resolve(p.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, LayoutResolveError::InvalidExtension(_)));
    }

    #[test]
    fn resolve_missing_kdl_path_errors() {
        let r = LayoutResolver::new(None);
        // Must contain `/` so it's classified as a path, not a stem.
        let err = r.resolve("./definitely-missing-xyz.kdl").unwrap_err();
        assert!(
            matches!(err, LayoutResolveError::PathMissing(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn user_override_wins_over_embedded() {
        let dir = tempdir().unwrap();
        let user_file = dir.path().join("builder.kdl");
        fs::write(&user_file, "layout { /* user override */ tab { pane } }").unwrap();

        let r = LayoutResolver::new(Some(dir.path().to_path_buf()));
        let got = r.resolve("builder").unwrap();
        match got {
            LayoutSource::User { path, contents } => {
                assert_eq!(path, user_file);
                assert!(contents.contains("user override"));
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn available_stems_returns_sorted_shipped_when_no_user_root() {
        let r = LayoutResolver::new(None);
        let stems = r.available_stems();
        let mut expected: Vec<String> = SHIPPED_LAYOUTS
            .iter()
            .map(|(s, _)| (*s).to_string())
            .collect();
        expected.sort();
        assert_eq!(stems, expected);
        assert_eq!(stems.len(), 6);
    }

    #[test]
    fn available_stems_merges_user_extras() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("mycustom.kdl"), "layout {}").unwrap();
        fs::write(dir.path().join("ignored.txt"), "nope").unwrap();

        let r = LayoutResolver::new(Some(dir.path().to_path_buf()));
        let stems = r.available_stems();
        assert!(stems.contains(&"mycustom".to_string()));
        assert!(!stems.contains(&"ignored".to_string()));
        // Still sorted.
        let mut sorted = stems.clone();
        sorted.sort();
        assert_eq!(stems, sorted);
    }
}
