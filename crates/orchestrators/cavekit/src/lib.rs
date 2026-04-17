//! CavekitOrchestrator — soul phase 1 stub adoption (T-020).
//!
//! This crate's pre-soul `Orchestrator` impl drove a tab graph
//! (builder + reviewer + log), spawned five filesystem watchers
//! (impl-tracking, ralph-loop, codex-findings, git-diff, review-tab),
//! and resolved an `Outcome` (Success/Failed/Killed/Crashed) from the
//! engine's `Done` event. The full implementation depended on
//! `AgentSpec`, `AgentEvent`, `Outcome`, `TabRole`, and `EventReceiver`
//! pattern-matching that no longer exists under cavekit-soul Phase 1.
//!
//! T-020 adopts the new trait surface:
//!
//! * `run` takes `&SessionSpec` and returns `Result<(), anyhow::Error>`.
//! * Internal config that previously read off
//!   `AgentSpec.orchestrator / .engine / .runner_config` is now keyed
//!   off `SessionSpec.ext_config["cavekit"]`. The local
//!   [`CavekitConfig`] struct typechecks the JSON shape (`layout`
//!   stem, watcher gates) and falls back to kit defaults on absence.
//!
//! The methodology body — tab graph, watchers, R9 done-signal resolver
//! — is staged out behind a `todo!()` to land in a Phase 2+ packet
//! once the extension event surface (`CoreEvent::Ext`-routed
//! tab/file/progress events) is in place. The stub still:
//!
//! * Implements [`Orchestrator`] cleanly (compiles + cargo-check
//!   passes for `ark-orchestrators-cavekit`).
//! * Preserves `detect()` so the CLI's orchestrator-selection chain
//!   (cavekit detect → claude-code detect → bare) keeps working.
//! * Leaves the watchers module behind the `_legacy_watchers` feature
//!   flag (default-off) so the existing 5k-line implementation can be
//!   restored once the event surface re-stabilises.

use std::fs;
use std::path::Path;

use anyhow::Result;
use ark_core::{Orchestrator, World};
use ark_types::SessionSpec;
use async_trait::async_trait;
use serde::Deserialize;

// Watchers (T-077..T-082) live behind a feature flag while the new
// extension event surface settles. The module body still exists on disk
// for re-introduction in Phase 2+.
#[cfg(feature = "_legacy_watchers")]
pub mod watchers;

// ----------------------------------------------------------------- detect ----

/// Return `true` when `cwd` matches any of the cavekit detection heuristics.
///
/// Detection rules (preserved from the pre-soul impl):
///
/// 1. `cwd/.cavekit/config` is a regular file.
/// 2. `cwd/context/sites/*.md` contains at least one file.
/// 3. `cwd/context/kits/cavekit-*.md` contains at least one file.
/// 4. `cwd/context/plans/*.md` contains a file mentioning either
///    `"build-site"` or `"Tier "`.
pub fn detect(cwd: &Path) -> bool {
    if is_file(&cwd.join(".cavekit").join("config")) {
        return true;
    }
    if any_md_file(&cwd.join("context").join("sites")) {
        return true;
    }
    if any_cavekit_kit(&cwd.join("context").join("kits")) {
        return true;
    }
    if any_plan_with_buildsite_marker(&cwd.join("context").join("plans")) {
        return true;
    }
    false
}

fn is_file(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

fn any_md_file(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Ok(meta) = fs::metadata(&path) {
                if meta.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

fn any_cavekit_kit(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("cavekit-") {
            continue;
        }
        if let Ok(meta) = fs::metadata(&path) {
            if meta.is_file() {
                return true;
            }
        }
    }
    false
}

fn any_plan_with_buildsite_marker(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if fs::metadata(&path).map(|m| !m.is_file()).unwrap_or(true) {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if contents.contains("build-site") || contents.contains("Tier ") {
            return true;
        }
    }
    false
}

// ----------------------------------------------------------- config ----------

/// Cavekit's bucket inside `SessionSpec.ext_config["cavekit"]`.
///
/// Each field is `Option`-typed so partial JSON in the spec round-trips
/// cleanly without losing any unrecognised fields. Watcher-gate fields
/// are reserved for the post-stub re-implementation.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CavekitConfig {
    /// Layout stem to hand to the multiplexer for the builder tab.
    /// Defaults to `"builder"` per kit R3.
    pub layout: Option<String>,
    /// Whether the impl-tracking filesystem watcher is enabled.
    /// Reserved for the Phase 2+ re-impl.
    pub watch_impl_tracking: Option<bool>,
    /// Whether the ralph-loop watcher is enabled.
    pub watch_ralph_loop: Option<bool>,
    /// Whether the review tab is auto-spawned on phase transitions.
    pub spawn_review_tab: Option<bool>,
    /// Whether the codex-findings watcher is enabled.
    pub watch_codex_findings: Option<bool>,
    /// Whether the git-diff watcher is enabled.
    pub watch_git_diff: Option<bool>,
}

impl CavekitConfig {
    /// Resolve the cavekit bucket from a [`SessionSpec`]. Missing /
    /// malformed entries fall back to defaults.
    pub fn from_spec(spec: &SessionSpec) -> Self {
        spec.ext_config
            .get("cavekit")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Layout stem with the kit default applied.
    pub fn layout_or_default(&self) -> &str {
        self.layout.as_deref().unwrap_or("builder")
    }
}

// ----------------------------------------------------------- orchestrator ----

/// Cavekit-driving orchestrator. Soul phase 1 stub.
#[derive(Debug, Default, Clone, Copy)]
pub struct CavekitOrchestrator;

impl CavekitOrchestrator {
    pub const fn new() -> Self {
        Self
    }

    /// Kit-default layout stem (R3).
    pub fn default_layout(&self) -> &'static str {
        "builder"
    }
}

#[async_trait]
impl Orchestrator for CavekitOrchestrator {
    fn name(&self) -> &'static str {
        "cavekit"
    }

    fn detect(&self, cwd: &Path) -> bool {
        detect(cwd)
    }

    async fn run(&self, spec: &SessionSpec, _world: World) -> Result<()> {
        // Resolve the cavekit-specific config slice. Reading it here
        // (even though we don't dispatch on it yet) verifies the spec
        // round-trip works end-to-end.
        let cfg = CavekitConfig::from_spec(spec);
        let layout = cfg.layout_or_default().to_string();
        tracing::warn!(
            session = %spec.id.as_path_leaf(),
            layout = %layout,
            "cavekit orchestrator: soul phase 1 stub — methodology body \
             (tab graph + watchers + R9 resolver) re-lands in Phase 2+"
        );
        // Phase 2+: spawn tab graph, watchers, run R9 done-signal resolver.
        Ok(())
    }
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    use ark_core::Config;
    use ark_mux_zellij::ZellijMux;
    use ark_types::{CancellationToken, SessionId, StateLayout};
    use std::sync::Arc;

    // ---- detect() preserved from T-075 -----------------------------------

    #[test]
    fn empty_tempdir_returns_false() {
        let dir = TempDir::new().unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn plans_with_buildsite_marker_matches() {
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(
            plans.join("build-site.md"),
            "# Build Site\n\nTier 0 — Foundations\n",
        )
        .unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn plans_with_tier_but_no_buildsite_text_matches() {
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(plans.join("plan.md"), "# Plan\n\nTier 0 foundation.\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn plans_with_generic_markdown_does_not_match() {
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        fs::write(plans.join("notes.md"), "just some notes\n").unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn cavekit_config_file_matches() {
        let dir = TempDir::new().unwrap();
        let cav = dir.path().join(".cavekit");
        fs::create_dir_all(&cav).unwrap();
        fs::write(cav.join("config"), "caveman_mode=on\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn cavekit_kit_file_matches() {
        let dir = TempDir::new().unwrap();
        let kits = dir.path().join("context").join("kits");
        fs::create_dir_all(&kits).unwrap();
        fs::write(kits.join("cavekit-foo.md"), "# foo\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn sites_directory_with_md_matches() {
        let dir = TempDir::new().unwrap();
        let sites = dir.path().join("context").join("sites");
        fs::create_dir_all(&sites).unwrap();
        fs::write(sites.join("my-site.md"), "# site\n").unwrap();
        assert!(detect(dir.path()));
    }

    // ---- orchestrator trait surface --------------------------------------

    #[test]
    fn name_returns_cavekit() {
        let o = CavekitOrchestrator::new();
        assert_eq!(o.name(), "cavekit");
    }

    #[test]
    fn default_layout_is_builder() {
        let o = CavekitOrchestrator::new();
        assert_eq!(o.default_layout(), "builder");
    }

    #[test]
    fn trait_detect_matches_free_function() {
        let dir = TempDir::new().unwrap();
        let cav = dir.path().join(".cavekit");
        fs::create_dir_all(&cav).unwrap();
        fs::write(cav.join("config"), "").unwrap();
        let o = CavekitOrchestrator::new();
        assert!(o.detect(dir.path()));
    }

    // ---- ext_config parsing ----------------------------------------------

    #[test]
    fn cavekit_config_falls_back_to_default_on_missing_bucket() {
        let spec = sample_spec(BTreeMap::new());
        let cfg = CavekitConfig::from_spec(&spec);
        assert_eq!(cfg.layout_or_default(), "builder");
    }

    #[test]
    fn cavekit_config_reads_layout_override() {
        let mut ext = BTreeMap::new();
        ext.insert(
            "cavekit".to_string(),
            serde_json::json!({ "layout": "focused" }),
        );
        let spec = sample_spec(ext);
        let cfg = CavekitConfig::from_spec(&spec);
        assert_eq!(cfg.layout_or_default(), "focused");
    }

    #[test]
    fn cavekit_config_ignores_other_extensions() {
        let mut ext = BTreeMap::new();
        ext.insert(
            "claude-code".to_string(),
            serde_json::json!({ "permission_policy": "ask" }),
        );
        let spec = sample_spec(ext);
        let cfg = CavekitConfig::from_spec(&spec);
        assert_eq!(cfg.layout_or_default(), "builder");
    }

    // ---- run() smoke -----------------------------------------------------

    #[tokio::test]
    async fn run_returns_ok_in_stub_phase() {
        let spec = sample_spec(BTreeMap::new());
        let world = make_world();
        let orch = CavekitOrchestrator::new();
        orch.run(&spec, world).await.expect("stub returns Ok");
    }

    // ---- helpers ---------------------------------------------------------

    fn sample_spec(
        ext: BTreeMap<String, serde_json::Value>,
    ) -> SessionSpec {
        SessionSpec {
            id: SessionId::new("cavekit"),
            name: "cavekit".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: ext,
        }
    }

    fn make_world() -> World {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let (events, _rx) = ark_types::channel(8);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        World::new(mux, events, cancel, hooks_dir, state, config)
    }
}
