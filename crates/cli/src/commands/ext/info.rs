//! `ark ext info <name>` — full metadata for a single installed extension.
//!
//! T-10.9 (cavekit-scene R13). Complements `ark ext list`:
//!
//! * `list` produces one row per extension with the summary columns.
//! * `info` reads the full manifest of a named extension, re-emits it
//!   as KDL, and appends the `.ark-install` source annotation when
//!   present.
//!
//! # Resolution order
//!
//! Uses the same precedence ladder as `list`:
//! project → user → system → built-in. The first tier that carries an
//! `<name>/extension.kdl` wins.
//!
//! # `.ark-install` dotfile
//!
//! Extensions installed via `ark ext install` (future tier) drop a
//! sibling `.ark-install` file that records the upstream source (URL,
//! tag, crate version, …). When present, this command renders it
//! verbatim after the KDL. Absent file = no annotation.

use std::fs;
use std::path::{Path, PathBuf};

use ark_ext_metadata::extension_metadata_kdl_string;
use ark_ext_metadata_types::ExtensionMetadata;
use clap::Args;

use super::list::{Tier, load_manifest};
use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext info`.
#[derive(Debug, Args)]
#[command(
    about = "Show full metadata for an installed extension",
    long_about = "Locate an installed extension by name and print its\n\
                  full metadata — intents, events, config schema,\n\
                  capabilities — plus the `.ark-install` annotation\n\
                  recording how it was installed.\n\
                  \n\
                  Examples:\n  \
                  ark ext info picker\n  \
                  ark ext info engine-claude"
)]
pub struct InfoArgs {
    /// Extension name (directory name under the search path).
    pub name: String,
}

/// Outcome of a successful lookup. Exposed for tests.
#[derive(Debug)]
pub struct InfoResult {
    /// Parsed manifest.
    pub metadata: ExtensionMetadata,
    /// Tier the extension was discovered on.
    pub tier: Tier,
    /// Path to the extension's root directory.
    pub root: PathBuf,
    /// Contents of `<root>/.ark-install`, verbatim, when present.
    pub install_annotation: Option<String>,
}

/// Dispatch handler for `ark ext info`.
///
/// Locates the named extension and prints its full metadata + the
/// `.ark-install` annotation (when present). Returns
/// [`CliError::NotFound`] when no search-path tier carries the
/// extension.
pub fn run(args: InfoArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let system_dirs: Vec<PathBuf> =
        vec![PathBuf::from("/usr/share/ark/extensions")];
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let res = locate_extension(
        &args.name,
        &cwd,
        xdg_data_home.as_deref(),
        &system_dirs.iter().map(|p| p.as_path()).collect::<Vec<_>>(),
    )
    .ok_or_else(|| CliError::NotFound {
        what: format!("extension `{}`", args.name),
    })?;

    print_info(&res)?;
    Ok(())
}

/// Pure helper: search every tier for an extension named `name` and
/// load its manifest + `.ark-install` annotation (when present).
///
/// Returns `None` when no tier carries the extension.
pub fn locate_extension(
    name: &str,
    cwd: &Path,
    xdg_data_home: Option<&Path>,
    system_dirs: &[&Path],
) -> Option<InfoResult> {
    // Project
    if let Some(r) = try_tier(&cwd.join(".ark/extensions").join(name), Tier::Project) {
        return Some(r);
    }
    if let Some(xdg) = xdg_data_home {
        if let Some(r) = try_tier(&xdg.join("ark/extensions").join(name), Tier::User) {
            return Some(r);
        }
    }
    for sys in system_dirs {
        if let Some(r) = try_tier(&sys.join(name), Tier::System) {
            return Some(r);
        }
    }
    None
}

/// Load `<root>/extension.kdl` from `root`; if missing or parse fails,
/// return `None`.
fn try_tier(root: &Path, tier: Tier) -> Option<InfoResult> {
    let manifest_path = root.join("extension.kdl");
    if !manifest_path.is_file() {
        return None;
    }
    let metadata = load_manifest(&manifest_path).ok()?;
    let install_annotation = fs::read_to_string(root.join(".ark-install")).ok();
    Some(InfoResult {
        metadata,
        tier,
        root: root.to_path_buf(),
        install_annotation,
    })
}

/// Render an [`InfoResult`] to stdout. The KDL body comes from
/// `ark_ext_metadata`'s emitter so hand-rendered output stays
/// byte-identical with the on-disk form.
pub fn print_info(res: &InfoResult) -> Result<(), CliError> {
    println!(
        "# source: {} ({})",
        res.tier.label(),
        res.root.display()
    );
    let kdl = extension_metadata_kdl_string(&res.metadata).map_err(|e| {
        CliError::Generic {
            reason: format!("ext/info: failed to emit metadata as KDL: {e}"),
        }
    })?;
    print!("{kdl}");
    if !kdl.ends_with('\n') {
        println!();
    }
    if let Some(ann) = &res.install_annotation {
        println!("# .ark-install");
        for line in ann.lines() {
            println!("#   {line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(name: &str) -> String {
        format!(
            r#"
extension {{
    name "{name}"
    version "0.1.0"
    ark-range ""
    zellij-range ""
    config {{ }}
    capabilities {{ }}
}}
"#
        )
    }

    fn make_ext(dir: &Path, name: &str, extras: &[(&str, &str)]) -> PathBuf {
        let d = dir.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("extension.kdl"), sample(name)).unwrap();
        for (fname, contents) in extras {
            fs::write(d.join(fname), contents).unwrap();
        }
        d
    }

    #[test]
    fn locate_uses_project_tier_first() {
        let cwd = TempDir::new().unwrap();
        make_ext(&cwd.path().join(".ark/extensions"), "demo", &[]);
        let res = locate_extension("demo", cwd.path(), None, &[]).expect("found");
        assert_eq!(res.tier, Tier::Project);
    }

    #[test]
    fn locate_falls_through_to_user() {
        let cwd = TempDir::new().unwrap();
        let xdg = TempDir::new().unwrap();
        make_ext(&xdg.path().join("ark/extensions"), "demo", &[]);
        let res = locate_extension("demo", cwd.path(), Some(xdg.path()), &[])
            .expect("found");
        assert_eq!(res.tier, Tier::User);
    }

    #[test]
    fn locate_returns_none_when_missing() {
        let cwd = TempDir::new().unwrap();
        assert!(locate_extension("nope", cwd.path(), None, &[]).is_none());
    }

    #[test]
    fn install_annotation_captured_when_present() {
        let cwd = TempDir::new().unwrap();
        make_ext(
            &cwd.path().join(".ark/extensions"),
            "demo",
            &[(".ark-install", "source: https://example.com/demo\ntag: v0.1.0\n")],
        );
        let res = locate_extension("demo", cwd.path(), None, &[]).expect("found");
        let ann = res.install_annotation.expect("annotation present");
        assert!(ann.contains("https://example.com/demo"));
        assert!(ann.contains("v0.1.0"));
    }

    #[test]
    fn no_install_annotation_when_dotfile_absent() {
        let cwd = TempDir::new().unwrap();
        make_ext(&cwd.path().join(".ark/extensions"), "demo", &[]);
        let res = locate_extension("demo", cwd.path(), None, &[]).expect("found");
        assert!(res.install_annotation.is_none());
    }

    #[test]
    fn print_info_renders_ok() {
        let cwd = TempDir::new().unwrap();
        let root = make_ext(&cwd.path().join(".ark/extensions"), "demo", &[]);
        let metadata =
            ark_ext_metadata::parse_extension_metadata_kdl(&sample("demo")).unwrap();
        let res = InfoResult {
            metadata,
            tier: Tier::Project,
            root,
            install_annotation: Some("src: local".into()),
        };
        print_info(&res).unwrap();
    }
}
