//! `ark ext update` — re-fetch an extension from its recorded source.
//!
//! T-12.10 (cavekit-scene R13). For each targeted extension the
//! update flow is:
//!
//! 1. Read `${XDG_DATA_HOME}/ark/extensions/<name>/.ark-install` to
//!    recover the original `source: <specifier>` line written at install
//!    time (T-12.9).
//! 2. Parse the specifier through the same
//!    [`super::add::parse_source`] so `path:`, `url:`, and `github:`
//!    sources reuse the add-path semantics byte-for-byte.
//! 3. Remove the existing directory.
//! 4. Invoke [`super::add::install_from_source`] to stage + verify +
//!    atomically rename the new copy into place.
//! 5. Diff old vs new metadata; when the version bumped, re-prompt the
//!    user for any newly-declared capabilities (T-13.5 hook; stubbed
//!    here so the CLI surface already honours the contract — the
//!    capability surface itself lands with the trust-file work).
//!
//! When invoked without a `<name>`, every subdirectory in
//! `${XDG_DATA_HOME}/ark/extensions/` is updated in turn. Failures on
//! one extension do not abort the run: each is reported and the command
//! exits 0 if at least one update succeeded, non-zero only when every
//! attempted update failed.

use std::fs;
use std::path::Path;

use ark_ext_metadata::parse_extension_metadata_kdl;
use ark_ext_metadata_types::ExtensionMetadata;
use clap::Args;

use super::add::{
    InstallOutcome, Source, decide_capability_disclosure, install_from_source_with_cap_decision,
    parse_source,
};
use super::remove::resolve_xdg_data_home;
use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext update`.
#[derive(Debug, Args)]
#[command(
    about = "Re-fetch an extension from its recorded install source",
    long_about = "Re-run the install pipeline for one or every\n\
                  user-tier extension. The source specifier is read from\n\
                  the `.ark-install` dotfile written by `ark ext add`,\n\
                  so updates are idempotent with the original install.\n\
                  \n\
                  When the extension version bumps and its manifest\n\
                  declares new capabilities (T-13.5), the CLI surface\n\
                  re-prompts for acceptance before the new copy lands.\n\
                  Without `--accept-all`, an unattended run will fail\n\
                  loudly on any version-bumped extension with new caps.\n\
                  \n\
                  Examples:\n  \
                  ark ext update picker\n  \
                  ark ext update                # every installed extension\n  \
                  ark ext update --accept-all   # CI mode, no prompts"
)]
pub struct UpdateArgs {
    /// Extension to update. Updates every installed extension when
    /// omitted.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// Skip capability re-prompt on version bumps (for CI).
    #[arg(long = "accept-all")]
    pub accept_all: bool,
}

/// Dispatch handler for `ark ext update`.
pub fn run(args: UpdateArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let xdg_data_home = resolve_xdg_data_home().map_err(|reason| CliError::Generic {
        reason: format!("ext/update: {reason}"),
    })?;
    let extensions_root = xdg_data_home.join("ark/extensions");

    let targets = collect_targets(&extensions_root, args.name.as_deref())?;
    if targets.is_empty() {
        // Explicit name → NotFound; bulk mode → informational message.
        return match args.name {
            Some(name) => Err(CliError::NotFound {
                what: format!("extension `{name}` is not installed"),
            }),
            None => {
                println!(
                    "no extensions installed under {} — nothing to update",
                    extensions_root.display()
                );
                Ok(())
            }
        };
    }

    let mut successes = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for name in &targets {
        match update_one(&extensions_root, name, args.accept_all) {
            Ok(outcome) => {
                successes += 1;
                println!(
                    "updated `{}` (version {} -> {}) from {}",
                    outcome.name,
                    outcome.old_version,
                    outcome.new_version,
                    outcome.source_specifier
                );
                for cap in &outcome.new_capabilities {
                    println!("  new capability: {cap}");
                }
            }
            Err(reason) => {
                failures.push(format!("`{name}`: {reason}"));
                eprintln!("update `{name}` failed: {reason}");
            }
        }
    }

    if successes == 0 && !failures.is_empty() {
        return Err(CliError::Generic {
            reason: format!(
                "ext/update: every update failed ({} total): {}",
                failures.len(),
                failures.join("; ")
            ),
        });
    }
    Ok(())
}

/// Successful-update summary surfaced to the CLI + tests.
#[derive(Debug)]
pub struct UpdateOutcome {
    /// Extension name (directory name under extensions root).
    pub name: String,
    /// Previously installed version string (from old manifest).
    pub old_version: String,
    /// Version after update (from new manifest).
    pub new_version: String,
    /// Source specifier read from `.ark-install`.
    pub source_specifier: String,
    /// Capabilities that appeared in the new manifest but weren't in
    /// the old one. v1 surfaces them; the trust prompt lands with T-13.x.
    pub new_capabilities: Vec<String>,
}

/// Pure helper: produce the ordered list of extension names we'll
/// attempt to update.
///
/// * `Some(name)` → just that one (even if it doesn't exist; the
///   outer dispatcher surfaces NotFound).
/// * `None` → every subdirectory of `extensions_root` (sorted).
fn collect_targets(
    extensions_root: &Path,
    explicit: Option<&str>,
) -> Result<Vec<String>, CliError> {
    if let Some(name) = explicit {
        let dir = extensions_root.join(name);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        return Ok(vec![name.to_string()]);
    }
    if !extensions_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut targets: Vec<String> = fs::read_dir(extensions_root)
        .map_err(|e| CliError::Generic {
            reason: format!("ext/update: reading {}: {e}", extensions_root.display()),
        })?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    targets.sort();
    Ok(targets)
}

/// Execute one update: read `.ark-install`, fetch fresh copy, diff caps.
pub fn update_one(
    extensions_root: &Path,
    name: &str,
    accept_all: bool,
) -> Result<UpdateOutcome, String> {
    let install_dir = extensions_root.join(name);
    if !install_dir.is_dir() {
        return Err(format!(
            "no such extension directory at {}",
            install_dir.display()
        ));
    }

    // ---- Step 1: read `.ark-install` ----------------------------------
    let install_meta =
        read_install_metadata(&install_dir).map_err(|e| format!("reading .ark-install: {e}"))?;
    let source = parse_source(&install_meta.source)
        .map_err(|e| format!("parsing recorded source `{}`: {e}", install_meta.source))?;

    // ---- Step 2: capture old manifest for diff ------------------------
    let old_metadata = read_installed_metadata(&install_dir).ok();
    let old_version = old_metadata
        .as_ref()
        .map(|m| m.version.value.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let old_caps = capabilities_of(&old_metadata);

    // ---- Step 3: wipe + re-install ------------------------------------
    //
    // `install_from_source*` refuses when the target dir already exists
    // (by design — prevents clobbers during `ark ext add`). Update
    // deliberately removes first, then reinstalls. A partial failure
    // between remove + reinstall leaves the extension uninstalled; the
    // user can retry `ark ext update <name>` once the remote is
    // reachable again.
    //
    // T-13.5: route the cap decision through
    // [`decide_capability_disclosure`] so a version bump that adds
    // caps triggers the same prompt / --accept-all path as a fresh
    // install. The prior-version carry-forward logic inside that
    // function means caps already trusted on the old version are auto-
    // granted — only genuinely new caps require acceptance.
    fs::remove_dir_all(&install_dir)
        .map_err(|e| format!("removing old install at {}: {e}", install_dir.display()))?;
    let outcome: InstallOutcome =
        install_from_source_with_cap_decision(&source, extensions_root, accept_all, &|meta| {
            decide_capability_disclosure(meta, accept_all)
        })?;

    // ---- Step 4: diff caps for the summary line ----------------------
    //
    // The prompt path already happened inside install_from_source_*;
    // the diff we report here is purely for the `updated X (version A
    // -> B)` summary line. Shows "new capability: <cap>" for caps that
    // are in the new manifest but weren't declared in the old one,
    // independent of which version they were trusted under.
    let new_caps = capabilities_of(&Some(outcome.metadata.clone()));
    let new_capabilities: Vec<String> = new_caps
        .iter()
        .filter(|cap: &&String| !old_caps.contains(cap))
        .cloned()
        .collect::<Vec<String>>();

    Ok(UpdateOutcome {
        name: outcome.metadata.name.value.clone(),
        old_version,
        new_version: outcome.metadata.version.value.clone(),
        source_specifier: install_meta.source,
        new_capabilities,
    })
}

/// Parsed `.ark-install` payload. Only `source:` is load-bearing; the
/// other fields are captured so future tooling (e.g. "show me every
/// extension installed after date X") can reuse the read path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallMetadata {
    /// Source specifier exactly as written at install time
    /// (e.g. `path:/home/user/ext`, `url:https://…`, `github:user/repo@v1`).
    pub source: String,
    /// RFC3339 timestamp of the install.
    pub installed_at: Option<String>,
    /// Extension name as recorded at install time.
    pub name: Option<String>,
}

/// Parse `<dir>/.ark-install`. Format (lines of `key: value`):
///
/// ```text
/// source: <specifier>
/// installed-at: <rfc3339>
/// name: <extension-name>
/// ```
///
/// Tolerant of unknown keys (silently ignored) so future fields can be
/// added without breaking older CLIs. Missing `source:` is an error —
/// without the specifier we have nothing to re-fetch from.
pub fn read_install_metadata(dir: &Path) -> Result<InstallMetadata, String> {
    let path = dir.join(".ark-install");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut source: Option<String> = None;
    let mut installed_at: Option<String> = None;
    let mut name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "source" => source = Some(value),
            "installed-at" => installed_at = Some(value),
            "name" => name = Some(value),
            _ => {} // forward-compat
        }
    }
    Ok(InstallMetadata {
        source: source.ok_or_else(|| {
            format!(
                "{} is missing a `source:` line — cannot determine update source",
                path.display()
            )
        })?,
        installed_at,
        name,
    })
}

/// Read `<dir>/extension.kdl` and parse as metadata. Returns a
/// `Result<_, String>` so callers can treat a missing manifest as
/// non-fatal via `.ok()` when that's desired (e.g. first-install diff
/// where the old manifest is absent).
fn read_installed_metadata(dir: &Path) -> Result<ExtensionMetadata, String> {
    let path = dir.join("extension.kdl");
    let text = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse_extension_metadata_kdl(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Extract the declared capability list from a manifest. T-13.3
/// added the `capabilities: Vec<StringNode>` field; this helper peels
/// the `StringNode` wrapper so the update-summary caller can diff
/// raw cap names without reaching into the metadata types directly.
///
/// Returns an empty vec when the old manifest is absent (first-time
/// bookkeeping on a legacy install without a stored old copy) so the
/// diff treats every declared cap as new and surfaces them in the
/// update summary — the install pipeline's cap decision still gates
/// the actual acceptance.
fn capabilities_of(metadata: &Option<ExtensionMetadata>) -> Vec<String> {
    metadata
        .as_ref()
        .map(|m| m.capability_names().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// Public helper for tests / scripts: parse a raw source string. Exposed
/// to keep `update`'s public surface self-contained — callers outside
/// the CLI should not have to dig into `ext::add` directly.
pub fn parse_install_source(raw: &str) -> Result<Source, String> {
    parse_source(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn sample_manifest(name: &str, version: &str) -> String {
        format!(
            r#"
extension {{
    name "{name}"
    version "{version}"
    ark-range ">=0.1"
    zellij-range ""
    config {{ }}
    capabilities {{ }}
}}
"#
        )
    }

    fn seed_installed(
        extensions_root: &Path,
        name: &str,
        version: &str,
        install_source: &str,
    ) -> PathBuf {
        let dir = extensions_root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extension.kdl"), sample_manifest(name, version)).unwrap();
        fs::write(
            dir.join(".ark-install"),
            format!("source: {install_source}\ninstalled-at: 2026-01-01T00:00:00Z\nname: {name}\n"),
        )
        .unwrap();
        dir
    }

    // --- install-metadata parser ---

    #[test]
    fn read_install_metadata_round_trip() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".ark-install"),
            "source: path:/tmp/src\ninstalled-at: 2026-01-01T00:00:00Z\nname: picker\n",
        )
        .unwrap();
        let meta = read_install_metadata(tmp.path()).unwrap();
        assert_eq!(meta.source, "path:/tmp/src");
        assert_eq!(meta.name.as_deref(), Some("picker"));
    }

    #[test]
    fn read_install_metadata_rejects_missing_source() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".ark-install"),
            "installed-at: 2026-01-01T00:00:00Z\nname: picker\n",
        )
        .unwrap();
        let err = read_install_metadata(tmp.path()).unwrap_err();
        assert!(err.contains("missing a `source:`"), "{err}");
    }

    #[test]
    fn read_install_metadata_skips_blank_and_comment_lines() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".ark-install"),
            "# comment\n\nsource: url:https://x/y.tar.gz\n",
        )
        .unwrap();
        let meta = read_install_metadata(tmp.path()).unwrap();
        assert_eq!(meta.source, "url:https://x/y.tar.gz");
    }

    #[test]
    fn read_install_metadata_ignores_unknown_keys() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".ark-install"),
            "source: path:/a\ncaps: exec,pipe\nfuture: whatever\n",
        )
        .unwrap();
        let meta = read_install_metadata(tmp.path()).unwrap();
        assert_eq!(meta.source, "path:/a");
    }

    // --- collect_targets ---

    #[test]
    fn collect_targets_explicit_returns_only_that_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark/extensions");
        seed_installed(&root, "picker", "0.1.0", "path:/tmp/picker");
        seed_installed(&root, "status", "0.1.0", "path:/tmp/status");
        let targets = collect_targets(&root, Some("picker")).unwrap();
        assert_eq!(targets, vec!["picker".to_string()]);
    }

    #[test]
    fn collect_targets_explicit_missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark/extensions");
        fs::create_dir_all(&root).unwrap();
        let targets = collect_targets(&root, Some("ghost")).unwrap();
        assert!(targets.is_empty());
    }

    #[test]
    fn collect_targets_bulk_lists_sorted_ext_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark/extensions");
        seed_installed(&root, "zebra", "0.1.0", "path:/tmp/z");
        seed_installed(&root, "apple", "0.1.0", "path:/tmp/a");
        // Seed a dotdir that must be skipped (e.g. staging leftovers).
        fs::create_dir_all(root.join(".ark-staging-xxx")).unwrap();
        let targets = collect_targets(&root, None).unwrap();
        assert_eq!(targets, vec!["apple".to_string(), "zebra".to_string()]);
    }

    #[test]
    fn collect_targets_bulk_empty_when_root_missing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("nope");
        let targets = collect_targets(&root, None).unwrap();
        assert!(targets.is_empty());
    }

    // --- update_one ---

    #[test]
    fn update_one_refetches_from_path_source() {
        // Stage a source dir containing a newer manifest; seed the
        // "installed" copy with an older one pointing at that source.
        let work = TempDir::new().unwrap();
        let src_dir = work.path().join("src/picker");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("extension.kdl"),
            sample_manifest("picker", "0.2.0"),
        )
        .unwrap();

        let extensions_root = work.path().join("xdg/ark/extensions");
        seed_installed(
            &extensions_root,
            "picker",
            "0.1.0",
            &format!("path:{}", src_dir.display()),
        );

        let outcome = update_one(&extensions_root, "picker", true).expect("update should succeed");
        assert_eq!(outcome.name, "picker");
        assert_eq!(outcome.old_version, "0.1.0");
        assert_eq!(outcome.new_version, "0.2.0");
        // The `.ark-install` dotfile is regenerated by install_from_source.
        let dotfile =
            fs::read_to_string(extensions_root.join("picker").join(".ark-install")).unwrap();
        assert!(dotfile.contains(&format!("path:{}", src_dir.display())));
    }

    #[test]
    fn update_one_fails_when_install_dir_missing() {
        let work = TempDir::new().unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");
        fs::create_dir_all(&extensions_root).unwrap();
        let err = update_one(&extensions_root, "ghost", true).unwrap_err();
        assert!(err.contains("no such extension directory"), "{err}");
    }

    #[test]
    fn update_one_fails_when_ark_install_missing_source() {
        let work = TempDir::new().unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");
        let dir = extensions_root.join("broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.kdl"),
            sample_manifest("broken", "0.1.0"),
        )
        .unwrap();
        // `.ark-install` missing entirely.
        let err = update_one(&extensions_root, "broken", true).unwrap_err();
        assert!(err.contains(".ark-install"), "{err}");
    }

    // --- run() dispatch ---

    #[test]
    fn run_bulk_updates_every_installed_extension() {
        let work = TempDir::new().unwrap();
        let xdg = work.path().join("xdg");
        let extensions_root = xdg.join("ark/extensions");
        fs::create_dir_all(&extensions_root).unwrap();

        // Two sources, two "installed" extensions pointing at them.
        let src_a = work.path().join("src/a");
        fs::create_dir_all(&src_a).unwrap();
        fs::write(src_a.join("extension.kdl"), sample_manifest("a", "0.2.0")).unwrap();
        let src_b = work.path().join("src/b");
        fs::create_dir_all(&src_b).unwrap();
        fs::write(src_b.join("extension.kdl"), sample_manifest("b", "0.2.0")).unwrap();

        seed_installed(
            &extensions_root,
            "a",
            "0.1.0",
            &format!("path:{}", src_a.display()),
        );
        seed_installed(
            &extensions_root,
            "b",
            "0.1.0",
            &format!("path:{}", src_b.display()),
        );

        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
        }

        let result = run(
            UpdateArgs {
                name: None,
                accept_all: true,
            },
            &Ctx::default(),
        );

        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        result.expect("bulk update should succeed");

        // Both versions bumped on disk.
        let a_meta = fs::read_to_string(extensions_root.join("a").join("extension.kdl")).unwrap();
        assert!(a_meta.contains("0.2.0"));
        let b_meta = fs::read_to_string(extensions_root.join("b").join("extension.kdl")).unwrap();
        assert!(b_meta.contains("0.2.0"));
    }

    #[test]
    fn run_named_missing_returns_not_found() {
        let work = TempDir::new().unwrap();
        let xdg = work.path().join("xdg");
        fs::create_dir_all(xdg.join("ark/extensions")).unwrap();

        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
        }

        let result = run(
            UpdateArgs {
                name: Some("ghost".into()),
                accept_all: true,
            },
            &Ctx::default(),
        );

        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
        match result {
            Err(CliError::NotFound { .. }) => { /* expected */ }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // ---- T-13.5: version-bump cap wiring through update_one --------------

    #[test]
    fn capabilities_of_returns_empty_for_absent_metadata() {
        // capabilities_of is the diff helper surfaced in the update
        // summary line. A None metadata (pre-install or legacy-missing
        // manifest) should yield an empty vec so the diff treats
        // every declared cap as "new" rather than panicking.
        let got = capabilities_of(&None);
        assert!(got.is_empty());
    }

    #[test]
    fn capabilities_of_reads_declared_caps_from_metadata() {
        // With T-13.3 the `capabilities` field lands on
        // ExtensionMetadata. capabilities_of must now peel the
        // StringNode wrapper and return the raw strings so update_one
        // can diff old vs new sets without reaching into the metadata
        // types directly.
        use ark_ext_metadata::{CapabilitySet, ConfigSchema, StringNode};
        let meta = ExtensionMetadata {
            name: StringNode::new("picker"),
            version: StringNode::new("1.2"),
            ark_range: StringNode::new(">=0.1"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::from_strs(&["exec", "pipe"]),
            config_sections: vec![],
            reload_gates: vec![],
        };
        let got = capabilities_of(&Some(meta));
        assert_eq!(got, vec!["exec".to_string(), "pipe".to_string()]);
    }

    #[test]
    fn update_one_routes_cap_decision_via_accept_all() {
        // update_one must hand the cap decision off to
        // decide_capability_disclosure — the old T-12.10 stub returned
        // a bogus "re-run with --accept-all" error even when
        // --accept-all was set. This test proves the accept-all path
        // now works end-to-end even though the sample manifest has
        // no caps (facet-kdl 0.42 can't round-trip the caps Vec yet,
        // so verifying the non-cap accept-all path is the strongest
        // assertion we can make at this layer; the cap-diff surface
        // itself is covered by the add.rs T-13.5 tests).
        let work = TempDir::new().unwrap();
        let src_dir = work.path().join("src/picker");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(
            src_dir.join("extension.kdl"),
            sample_manifest("picker", "0.2.0"),
        )
        .unwrap();
        let extensions_root = work.path().join("xdg/ark/extensions");
        seed_installed(
            &extensions_root,
            "picker",
            "0.1.0",
            &format!("path:{}", src_dir.display()),
        );

        // Isolate XDG_CONFIG_HOME so the cap decision (which reads
        // `${XDG_CONFIG_HOME}/ark/extension-trust.kdl`) doesn't touch
        // the real user's trust file.
        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let xdg_cfg = work.path().join("xdg-cfg");
        fs::create_dir_all(&xdg_cfg).unwrap();
        let prior_cfg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg_cfg);
        }

        let outcome = update_one(&extensions_root, "picker", true);

        unsafe {
            match prior_cfg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }

        let outcome = outcome.expect("update should succeed");
        assert_eq!(outcome.old_version, "0.1.0");
        assert_eq!(outcome.new_version, "0.2.0");
        // No caps in the sample manifest → empty diff.
        assert!(outcome.new_capabilities.is_empty());
    }

    #[test]
    fn parse_install_source_passes_through_to_add() {
        let s = parse_install_source("github:foo/bar@v1.0").unwrap();
        match s {
            Source::Github { slug, git_ref } => {
                assert_eq!(slug, "foo/bar");
                assert_eq!(git_ref.as_deref(), Some("v1.0"));
            }
            other => panic!("expected Github source, got {other:?}"),
        }
    }
}
