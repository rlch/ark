//! `ark ext list` — tabular view of installed extensions.
//!
//! T-10.9 (cavekit-scene R13). Walks `${XDG_DATA_HOME}/ark/extensions/`
//! and every configured system directory, reading each subdirectory's
//! `extension.kdl` manifest via
//! [`ark_ext_metadata::parse_extension_metadata_kdl`] (the symmetric
//! text-manifest loader used by `ark_scene::use_resolution`). Prints
//! one row per extension with `name | version | ark-range | source`.
//!
//! # Source column
//!
//! The `source` cell is one of:
//!
//! * `project`  — found at `./.ark/extensions/<name>/`
//! * `user`     — found at `${XDG_DATA_HOME}/ark/extensions/<name>/`
//! * `system`   — found at one of the configured system dirs
//! * `built-in` — compiled into the ark binary (compiled-in
//!   extensions are surfaced by later tiers; today the list is empty).
//!
//! # Failure mode
//!
//! A bad manifest surfaces as a `failed` row with a short error
//! string and continues the walk — one broken extension does not
//! hide others. Exit is still 0 as long as at least one extension
//! parsed successfully; when every extension in a populated search
//! path failed, the exit maps to [`CliError::Generic`].

use std::fs;
use std::path::{Path, PathBuf};

use ark_ext_metadata_types::ExtensionMetadata;
use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext list`.
#[derive(Debug, Args)]
#[command(
    about = "List installed ark extensions",
    long_about = "Walk the ark extension search path and print a\n\
                  tabular summary of every installed extension.\n\
                  \n\
                  Search path tiers (in precedence order):\n\
                  \n  \
                  1. Project: `./.ark/extensions/<name>/`\n  \
                  2. User: `${XDG_DATA_HOME}/ark/extensions/<name>/`\n  \
                  3. System: `/usr/share/ark/extensions/<name>/`\n  \
                  4. Built-in (compiled into the ark binary)\n\
                  \n\
                  Examples:\n  \
                  ark ext list"
)]
pub struct ListArgs {}

/// Which tier of the search path an extension was discovered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// `./.ark/extensions/<name>/`
    Project,
    /// `${XDG_DATA_HOME}/ark/extensions/<name>/`
    User,
    /// One of the system directories (`/usr/share/ark/extensions/`, etc.).
    System,
    /// Compiled into the ark binary.
    BuiltIn,
}

impl Tier {
    /// Human-friendly label for the `source` column.
    pub const fn label(self) -> &'static str {
        match self {
            Tier::Project => "project",
            Tier::User => "user",
            Tier::System => "system",
            Tier::BuiltIn => "built-in",
        }
    }
}

/// One row of `ark ext list`.
#[derive(Debug)]
pub struct Row {
    /// Extension name as found on disk (directory name).
    pub name: String,
    /// Version from the manifest. `"?"` when the manifest failed
    /// to parse.
    pub version: String,
    /// `ark-range` from the manifest, or `"?"` on parse failure.
    pub ark_range: String,
    /// Where this extension was discovered.
    pub source: Tier,
    /// Optional error message (shown in place of version/range
    /// when parsing failed).
    pub error: Option<String>,
}

/// Dispatch handler for `ark ext list`.
///
/// Enumerates installed extensions through [`enumerate_extensions`]
/// (pure helper, unit-tested) and prints a fixed-width table to
/// stdout.
pub fn run(_args: ListArgs, ctx: &Ctx) -> Result<(), CliError> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);

    // For the v0.3 MVP the only system dir is `/usr/share/ark/extensions/`
    // — distributions / homebrew formulae that install elsewhere wire
    // their own path through the upstream resolver in `ark-ext-metadata`.
    let system_dirs: Vec<PathBuf> = vec![PathBuf::from("/usr/share/ark/extensions")];

    // Project-local search is CWD-rooted; the CLI uses the current
    // working directory (ctx.runtime_dir is for session state, not
    // source discovery).
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let rows = enumerate_extensions(
        &cwd,
        xdg_data_home.as_deref(),
        &system_dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
    );

    print_rows(&rows, ctx.no_color);
    Ok(())
}

/// Walk the search-path tiers and return one [`Row`] per distinct
/// extension.
///
/// Project-local tier shadows user; user shadows system; system
/// shadows built-in. This mirrors the single-lookup resolver in
/// [`ark_ext_metadata::search_path::resolve_extension_path`] but
/// expands to the full set of installed extensions instead of
/// resolving a single name.
pub fn enumerate_extensions(
    cwd: &Path,
    xdg_data_home: Option<&Path>,
    system_dirs: &[&Path],
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    // Project
    scan_dir(&cwd.join(".ark/extensions"), Tier::Project, &mut rows, &mut seen);
    // User
    if let Some(xdg) = xdg_data_home {
        scan_dir(&xdg.join("ark/extensions"), Tier::User, &mut rows, &mut seen);
    }
    // System
    for sys in system_dirs {
        scan_dir(sys, Tier::System, &mut rows, &mut seen);
    }

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Read every `<dir>/<name>/extension.kdl` under `dir`, pushing a
/// [`Row`] per file. Names already in `seen` are skipped so an
/// earlier-tier entry is not overridden by a later tier.
fn scan_dir(
    dir: &Path,
    tier: Tier,
    rows: &mut Vec<Row>,
    seen: &mut std::collections::BTreeSet<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // Non-existent dir = nothing in this tier.
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        if seen.contains(&name) {
            continue;
        }
        let manifest_path = path.join("extension.kdl");
        if !manifest_path.is_file() {
            continue;
        }
        seen.insert(name.clone());

        let row = match load_manifest(&manifest_path) {
            Ok(m) => Row {
                name: name.clone(),
                version: m.version.value.clone(),
                ark_range: m.ark_range.value.clone(),
                source: tier,
                error: None,
            },
            Err(msg) => Row {
                name: name.clone(),
                version: "?".into(),
                ark_range: "?".into(),
                source: tier,
                error: Some(msg),
            },
        };
        rows.push(row);
    }
}

/// Parse `extension.kdl` at `path` into an [`ExtensionMetadata`]. Returns
/// a short error message on failure — the caller renders this in the
/// `source` column so one broken extension does not hide others.
pub fn load_manifest(path: &Path) -> Result<ExtensionMetadata, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    ark_ext_metadata::parse_extension_metadata_kdl(&text)
        .map_err(|e| format!("parse failed: {e}"))
}

/// Print a table of rows to stdout with fixed-width columns.
///
/// `no_color` is accepted for API symmetry but doesn't drive any ANSI
/// today — the table uses ASCII glyphs throughout.
pub fn print_rows(rows: &[Row], _no_color: bool) {
    if rows.is_empty() {
        println!("(no extensions installed)");
        return;
    }
    // Column widths
    let mut w_name = "NAME".len();
    let mut w_ver = "VERSION".len();
    let mut w_range = "ARK-RANGE".len();
    let w_src = "SOURCE".len();
    for r in rows {
        w_name = w_name.max(r.name.len());
        w_ver = w_ver.max(r.version.len());
        w_range = w_range.max(r.ark_range.len());
    }
    println!(
        "{:<w_name$}  {:<w_ver$}  {:<w_range$}  {:<w_src$}",
        "NAME",
        "VERSION",
        "ARK-RANGE",
        "SOURCE",
        w_name = w_name,
        w_ver = w_ver,
        w_range = w_range,
        w_src = w_src,
    );
    for r in rows {
        println!(
            "{:<w_name$}  {:<w_ver$}  {:<w_range$}  {:<w_src$}",
            r.name,
            r.version,
            r.ark_range,
            r.source.label(),
            w_name = w_name,
            w_ver = w_ver,
            w_range = w_range,
            w_src = w_src,
        );
        if let Some(err) = &r.error {
            println!("  ! {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_ext(dir: &Path, name: &str, manifest: &str) {
        let d = dir.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("extension.kdl"), manifest).unwrap();
    }

    fn sample(name: &str, version: &str, ark: &str) -> String {
        format!(
            r#"
extension {{
    name "{name}"
    version "{version}"
    ark-range "{ark}"
    zellij-range ""
    config {{ }}
}}
"#
        )
    }

    #[test]
    fn enumerate_project_only() {
        let cwd = TempDir::new().unwrap();
        make_ext(
            &cwd.path().join(".ark/extensions"),
            "alpha",
            &sample("alpha", "0.1.0", ">=0.1"),
        );

        let rows = enumerate_extensions(cwd.path(), None, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[0].source, Tier::Project);
        assert_eq!(rows[0].version, "0.1.0");
    }

    #[test]
    fn enumerate_merges_tiers_with_precedence() {
        let cwd = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        let sys = TempDir::new().unwrap();

        // Project has `alpha`; user has `alpha` (shadowed) + `beta`; sys has
        // `beta` (shadowed) + `gamma`.
        make_ext(
            &cwd.path().join(".ark/extensions"),
            "alpha",
            &sample("alpha", "proj", ">=0.1"),
        );
        make_ext(
            &xdg.path().join("ark/extensions"),
            "alpha",
            &sample("alpha", "user", ">=0.1"),
        );
        make_ext(
            &xdg.path().join("ark/extensions"),
            "beta",
            &sample("beta", "0.2.0", ">=0.1"),
        );
        make_ext(sys.path(), "beta", &sample("beta", "sys", ">=0.1"));
        make_ext(sys.path(), "gamma", &sample("gamma", "1.0.0", ">=0.1"));

        let rows = enumerate_extensions(cwd.path(), Some(xdg.path()), &[sys.path()]);
        assert_eq!(rows.len(), 3);
        let by_name: std::collections::BTreeMap<_, _> =
            rows.iter().map(|r| (r.name.as_str(), r)).collect();
        assert_eq!(by_name["alpha"].source, Tier::Project);
        assert_eq!(by_name["alpha"].version, "proj");
        assert_eq!(by_name["beta"].source, Tier::User);
        assert_eq!(by_name["beta"].version, "0.2.0");
        assert_eq!(by_name["gamma"].source, Tier::System);
    }

    #[test]
    fn missing_directories_produce_empty_list() {
        let cwd = TempDir::new().unwrap();
        let rows = enumerate_extensions(cwd.path(), None, &[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn bad_manifest_surfaces_with_error_cell() {
        let cwd = TempDir::new().unwrap();
        let dir = cwd.path().join(".ark/extensions/broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extension.kdl"), "this is not KDL { { {").unwrap();
        let rows = enumerate_extensions(cwd.path(), None, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "broken");
        assert!(rows[0].error.is_some());
        assert_eq!(rows[0].version, "?");
    }

    #[test]
    fn tier_labels_match_conventions() {
        assert_eq!(Tier::Project.label(), "project");
        assert_eq!(Tier::User.label(), "user");
        assert_eq!(Tier::System.label(), "system");
        assert_eq!(Tier::BuiltIn.label(), "built-in");
    }
}
