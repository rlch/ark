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

use crate::layout_template::{LayoutTemplateError, LayoutVars, render as render_layout};

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

/// One entry returned by [`LayoutResolver::list`] (T-037).
///
/// Source is the static string `"user"` or `"embedded"` so the CLI can
/// render the diagnostic line without an extra match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutListEntry {
    pub stem: String,
    pub source: &'static str,
}

/// Result of validating a single user-authored layout (T-038).
#[derive(Debug)]
pub struct LayoutValidation {
    pub path: PathBuf,
    pub result: Result<(), LayoutTemplateError>,
}

/// Default layout stem for a given orchestrator slug. v1 mapping per
/// cavekit-layouts.md R6 (T-036).
pub fn default_layout_for_orchestrator(orchestrator: &str) -> &'static str {
    match orchestrator {
        "cavekit" => "builder",
        "claude-code" => "classic",
        "review" => "review",
        _ => "classic", // unknown falls back to the generic 2-pane
    }
}

/// Given all precedence inputs, return the final stem-or-path to resolve.
///
/// Precedence (T-036, cavekit-layouts.md R6):
///   1. `--layout` CLI flag
///   2. `AgentSpec.layout`
///   3. `config.defaults.layout`
///   4. `default_layout_for_orchestrator(orchestrator)`
pub fn effective_layout(
    cli_flag: Option<&str>,
    spec_layout: Option<&str>,
    config_default: Option<&str>,
    orchestrator: &str,
) -> String {
    cli_flag
        .or(spec_layout)
        .or(config_default)
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_layout_for_orchestrator(orchestrator).to_string())
}

impl LayoutResolver {
    /// Enumerate all layouts available via this resolver: shipped first, then
    /// any user-rooted `.kdl` files. User layouts shadowing embedded appear
    /// only as `"user"`. Output is sorted alphabetically by stem (T-037).
    pub fn list(&self) -> Vec<LayoutListEntry> {
        let mut entries: Vec<LayoutListEntry> = Vec::new();
        let mut user_stems: std::collections::BTreeSet<String> = Default::default();

        if let Some(root) = &self.user_root
            && let Ok(read) = std::fs::read_dir(root)
        {
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("kdl")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    user_stems.insert(stem.to_string());
                }
            }
        }

        for stem in &user_stems {
            entries.push(LayoutListEntry {
                stem: stem.clone(),
                source: "user",
            });
        }
        for (stem, _) in SHIPPED_LAYOUTS {
            if !user_stems.contains(*stem) {
                entries.push(LayoutListEntry {
                    stem: (*stem).to_string(),
                    source: "embedded",
                });
            }
        }

        entries.sort_by(|a, b| a.stem.cmp(&b.stem));
        entries
    }

    /// Validate every user-authored `.kdl` under `user_root` by rendering it
    /// against a dummy `LayoutVars` and KDL-validating the output. Used by
    /// the `ark doctor` hook (T-038).
    ///
    /// Returns one entry per user file found. If `user_root` is unset or
    /// missing, returns an empty Vec.
    pub fn validate_user_layouts(&self) -> Vec<LayoutValidation> {
        let mut out: Vec<LayoutValidation> = Vec::new();
        let Some(root) = &self.user_root else {
            return out;
        };
        let Ok(read) = std::fs::read_dir(root) else {
            return out;
        };
        let dummy = LayoutVars {
            cwd: "/tmp".into(),
            agent_cmd: "placeholder".into(),
            agent_args: Vec::new(),
            id: "cavekit-dummy-00000000000000000000000000".into(),
            name: "dummy".into(),
        };
        let mut paths: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("kdl") && p.is_file())
            .collect();
        paths.sort();
        for path in paths {
            let result = match std::fs::read_to_string(&path) {
                Ok(src) => render_layout(&src, &dummy).map(|_| ()),
                Err(e) => Err(LayoutTemplateError::Syntax(format!(
                    "could not read {:?}: {e}",
                    path
                ))),
            };
            out.push(LayoutValidation { path, result });
        }
        out
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

    // ------- T-036: orchestrator-default mapping + precedence -------

    #[test]
    fn default_layout_for_orchestrator_known_slugs() {
        assert_eq!(default_layout_for_orchestrator("cavekit"), "builder");
        assert_eq!(default_layout_for_orchestrator("claude-code"), "classic");
        assert_eq!(default_layout_for_orchestrator("review"), "review");
    }

    #[test]
    fn default_layout_for_orchestrator_unknown_falls_back_to_classic() {
        assert_eq!(default_layout_for_orchestrator("totally-new"), "classic");
    }

    #[test]
    fn effective_layout_precedence_cli_beats_all() {
        let got = effective_layout(Some("focused"), Some("review"), Some("classic"), "cavekit");
        assert_eq!(got, "focused");
    }

    #[test]
    fn effective_layout_precedence_spec_beats_config_and_default() {
        let got = effective_layout(None, Some("review"), Some("classic"), "cavekit");
        assert_eq!(got, "review");
    }

    #[test]
    fn effective_layout_precedence_config_beats_default() {
        let got = effective_layout(None, None, Some("focused"), "cavekit");
        assert_eq!(got, "focused");
    }

    #[test]
    fn effective_layout_falls_back_to_orchestrator_default() {
        assert_eq!(effective_layout(None, None, None, "cavekit"), "builder");
        assert_eq!(effective_layout(None, None, None, "claude-code"), "classic");
        assert_eq!(effective_layout(None, None, None, "review"), "review");
        assert_eq!(effective_layout(None, None, None, "unknown"), "classic");
    }

    // ------- T-037: list() diagnostic -------

    #[test]
    fn list_returns_all_six_embedded_when_no_user_root() {
        let r = LayoutResolver::new(None);
        let entries = r.list();
        assert_eq!(entries.len(), 6);
        assert!(entries.iter().all(|e| e.source == "embedded"));
        // Sorted alphabetically.
        let stems: Vec<&str> = entries.iter().map(|e| e.stem.as_str()).collect();
        let mut sorted = stems.clone();
        sorted.sort();
        assert_eq!(stems, sorted);
    }

    #[test]
    fn list_marks_user_overrides_as_user_and_embeds_others() {
        let dir = tempdir().unwrap();
        // Shadow `builder`.
        fs::write(dir.path().join("builder.kdl"), "layout { tab { pane } }").unwrap();
        // A user-only stem.
        fs::write(dir.path().join("mine.kdl"), "layout { tab { pane } }").unwrap();
        // A non-kdl file is ignored.
        fs::write(dir.path().join("notes.md"), "ignored").unwrap();

        let r = LayoutResolver::new(Some(dir.path().to_path_buf()));
        let entries = r.list();

        let lookup: std::collections::HashMap<_, _> =
            entries.iter().map(|e| (e.stem.clone(), e.source)).collect();
        assert_eq!(lookup.get("builder").copied(), Some("user"));
        assert_eq!(lookup.get("mine").copied(), Some("user"));
        // Non-shadowed shipped layouts remain embedded.
        assert_eq!(lookup.get("classic").copied(), Some("embedded"));
        assert_eq!(lookup.get("focused").copied(), Some("embedded"));
        assert_eq!(lookup.get("log").copied(), Some("embedded"));
        assert_eq!(lookup.get("review").copied(), Some("embedded"));
        assert_eq!(lookup.get("triple-column").copied(), Some("embedded"));

        // 6 shipped + 1 user-only = 7 total (builder counted once as "user").
        assert_eq!(entries.len(), 7);

        // Sorted alphabetically.
        let stems: Vec<String> = entries.iter().map(|e| e.stem.clone()).collect();
        let mut sorted = stems.clone();
        sorted.sort();
        assert_eq!(stems, sorted);
    }

    // ------- T-038: validate_user_layouts -------

    #[test]
    fn validate_user_layouts_empty_when_no_user_root() {
        let r = LayoutResolver::new(None);
        assert!(r.validate_user_layouts().is_empty());
    }

    #[test]
    fn validate_user_layouts_reports_one_ok_one_err() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("ok.kdl"),
            r#"layout { tab name="{{ name }}" { pane } }"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("broken.kdl"),
            // Unbalanced brace post-render.
            r#"layout { tab name="{{ name }}" { pane }"#,
        )
        .unwrap();

        let r = LayoutResolver::new(Some(dir.path().to_path_buf()));
        let mut results = r.validate_user_layouts();
        results.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(results.len(), 2);

        // `broken.kdl` sorts before `ok.kdl`.
        assert!(results[0].path.ends_with("broken.kdl"));
        assert!(results[0].result.is_err());

        assert!(results[1].path.ends_with("ok.kdl"));
        assert!(results[1].result.is_ok());
    }

    #[test]
    fn validate_user_layouts_renders_shipped_layouts_when_dropped_in() {
        // Sanity check: the shipped layouts are themselves valid templates +
        // valid KDL. Drop them into a temp user_root and validate.
        let dir = tempdir().unwrap();
        for (stem, contents) in SHIPPED_LAYOUTS {
            fs::write(dir.path().join(format!("{stem}.kdl")), contents).unwrap();
        }
        let r = LayoutResolver::new(Some(dir.path().to_path_buf()));
        for v in r.validate_user_layouts() {
            assert!(v.result.is_ok(), "{:?} failed: {:?}", v.path, v.result);
        }
    }
}
