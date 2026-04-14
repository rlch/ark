//! CavekitOrchestrator — detection (R1) + engine declaration (R2) + builder
//! tab open / Done / cancel handling (R3).
//!
//! Landed in stages:
//! - T-075: `detect(cwd)` (R1) — see module-level detection heuristics below.
//! - T-076: `impl Orchestrator` — `engine()`, `name()`, and a minimal `run`
//!   that opens the builder tab, emits `TabOpened { role: Builder }`, and
//!   waits for engine `Done` or supervisor cancel.
//!
//! Watchers / phase detection / review-tab spawning / artifact diffing
//! (R4–R9) land in later tasks (T-077, T-079, T-080, T-081, T-082, T-083).
//! This file deliberately stays small until those arrive.
//!
//! ## Detection heuristics
//!
//! `detect(cwd)` returns `true` when the directory looks cavekit-managed.
//! Any of the following is sufficient:
//!
//! 1. `cwd/context/sites/*.md` contains at least one file.
//! 2. `cwd/context/plans/*.md` contains at least one file AND at least one
//!    of those markdown files contains the string `"build-site"` or
//!    `"Tier "` (heuristic to separate cavekit build sites from generic
//!    plan docs).
//! 3. `cwd/.cavekit/config` exists (regular file).
//! 4. `cwd/context/kits/cavekit-*.md` contains at least one file.
//!
//! All I/O errors (permission denied, missing intermediate paths, unreadable
//! files) are swallowed and cause `detect` to return `false`. We do not
//! panic.
//!
//! ## Layout resolution (R3)
//!
//! Kit R3 calls for `config.orchestrator.cavekit.default_layout` (default
//! `"builder"`). The orchestrator does not receive a fully-plumbed `Config`
//! for that subsection in v1, so the precedence is:
//!
//! 1. `spec.layout` (user override at spawn time) — takes precedence if set.
//! 2. Hardcoded default `"builder"` (matches the kit default).
//!
//! When a future packet wires `Config::orchestrator.cavekit.default_layout`
//! through, it should slot in between (1) and (2) above.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ark_core::{Orchestrator, World};
use ark_types::{AgentEvent, AgentSpec, Outcome, TabRole};
use async_trait::async_trait;

// Watchers (T-077 / T-079 / T-082). Exposed as standalone async fns for
// future wiring in T-083; see `watchers/mod.rs`.
pub mod watchers;

// ----------------------------------------------------------------- detect ----

/// Return `true` when `cwd` matches any of the cavekit detection heuristics.
pub fn detect(cwd: &Path) -> bool {
    // Rule 3: .cavekit/config — cheapest, check first.
    if is_file(&cwd.join(".cavekit").join("config")) {
        return true;
    }

    // Rule 1: context/sites/*.md
    if any_md_file(&cwd.join("context").join("sites")) {
        return true;
    }

    // Rule 4: context/kits/cavekit-*.md
    if any_cavekit_kit(&cwd.join("context").join("kits")) {
        return true;
    }

    // Rule 2: context/plans/*.md containing "build-site" or "Tier "
    if any_plan_with_buildsite_marker(&cwd.join("context").join("plans")) {
        return true;
    }

    false
}

fn is_file(p: &Path) -> bool {
    fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Any `*.md` regular file in `dir`. Errors → `false`.
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

/// Any `cavekit-*.md` regular file in `dir`. Errors → `false`.
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

/// Any `*.md` in `dir` whose contents contain either `"build-site"` or
/// `"Tier "`. Errors → `false`.
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

// ----------------------------------------------------------- orchestrator ----

/// Cavekit-driving orchestrator. See module docs for which kit requirements
/// are implemented today vs deferred to later packets.
#[derive(Debug, Default, Clone, Copy)]
pub struct CavekitOrchestrator;

impl CavekitOrchestrator {
    pub const fn new() -> Self {
        Self
    }

    /// Kit-default layout stem (R3). When a future packet threads
    /// `Config::orchestrator.cavekit.default_layout` into `World`, that
    /// config value should be preferred over this constant.
    pub fn default_layout(&self) -> &'static str {
        "builder"
    }
}

#[async_trait]
impl Orchestrator for CavekitOrchestrator {
    fn name(&self) -> &'static str {
        "cavekit"
    }

    fn engine(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, cwd: &Path) -> bool {
        detect(cwd)
    }

    async fn run(&self, spec: AgentSpec, world: World) -> Result<Outcome> {
        // R3 layout resolution: spec.layout (user override) > kit default.
        // The mux resolves the stem to a rendered KDL path (see
        // cavekit-mux-zellij R2 / T-026). Template substitution of
        // `{{agent_cmd}}` etc. happens inside the layout-renderer path
        // (T-029/T-030); the orchestrator only passes the stem here.
        let layout_stem = spec
            .layout
            .as_deref()
            .unwrap_or_else(|| self.default_layout())
            .to_string();
        let layout_path = PathBuf::from(&layout_stem);

        // Open the builder tab.
        let tab_handle = world
            .mux
            .create_tab(&spec.session, "builder", &layout_path)
            .await?;

        // Announce on the bus. `label` is always "builder" regardless of
        // which layout was chosen — the role/label is structural, while the
        // layout controls the pane wiring inside it.
        let _ = world.events.send(AgentEvent::TabOpened {
            id: spec.id.clone(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab_handle.clone(),
            label: "builder".to_string(),
        });

        // Subscribe after create_tab so the only events we can observe are
        // ones emitted after the tab is live (prevents racy double-reads of
        // our own TabOpened above — we emit via `send` which goes to all
        // subscribers, and the engine's Done is what we care about here).
        let mut events = world.events.subscribe();

        // Wait on engine Done or supervisor cancel. Engine emits
        // `Done { outcome }`; v1 passes that through unmodified. Upgrades
        // to the outcome (review enrichment, phase-gate checks, etc.) land
        // in T-080+.
        loop {
            tokio::select! {
                biased;
                _ = world.cancel.cancelled() => {
                    // Cancel → close builder tab, return Killed. We
                    // deliberately ignore close_tab errors; the supervisor
                    // will tear down the session on drop anyway.
                    let _ = world.mux.close_tab(&tab_handle).await;
                    return Ok(Outcome::Killed);
                }
                res = events.recv() => {
                    match res {
                        Ok(AgentEvent::Done { id, outcome }) if id == spec.id => {
                            return Ok(outcome);
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Bus closed unexpectedly — treat as empty success.
                            return Ok(Outcome::Success { artifacts: Vec::new() });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                skipped,
                                "cavekit orchestrator lagged on event bus"
                            );
                            continue;
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::{Config, Multiplexer};
    use ark_types::{AgentId, CancellationToken, EventSink, StateLayout, TabHandle};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    // ------- detect() tests (preserved from T-075) ------------------------

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
        // "Tier " alone is sufficient.
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
    fn cavekit_config_directory_does_not_match() {
        // `.cavekit/config` must be a regular file.
        let dir = TempDir::new().unwrap();
        let cav = dir.path().join(".cavekit").join("config");
        fs::create_dir_all(&cav).unwrap();
        assert!(!detect(dir.path()));
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
    fn non_cavekit_kit_does_not_match() {
        let dir = TempDir::new().unwrap();
        let kits = dir.path().join("context").join("kits");
        fs::create_dir_all(&kits).unwrap();
        fs::write(kits.join("other-foo.md"), "# foo\n").unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn sites_directory_with_md_matches() {
        let dir = TempDir::new().unwrap();
        let sites = dir.path().join("context").join("sites");
        fs::create_dir_all(&sites).unwrap();
        fs::write(sites.join("my-site.md"), "# site\n").unwrap();
        assert!(detect(dir.path()));
    }

    #[test]
    fn sites_directory_empty_does_not_match() {
        let dir = TempDir::new().unwrap();
        let sites = dir.path().join("context").join("sites");
        fs::create_dir_all(&sites).unwrap();
        assert!(!detect(dir.path()));
    }

    #[test]
    fn does_not_panic_on_missing_intermediate_paths() {
        // No `context` dir at all — every read_dir returns Err.
        let dir = TempDir::new().unwrap();
        assert!(!detect(dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_returns_false_without_panic() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let plans = dir.path().join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        // Make plans unreadable.
        let mut perms = fs::metadata(&plans).unwrap().permissions();
        perms.set_mode(0o000);
        // On macOS, setting 0o000 on a directory owned by root may still allow
        // the owning user to read. Best-effort only.
        let applied = fs::set_permissions(&plans, perms).is_ok();

        let result = detect(dir.path());

        // Restore so tempdir cleanup works.
        let mut restore = fs::metadata(&plans).unwrap().permissions();
        restore.set_mode(0o755);
        let _ = fs::set_permissions(&plans, restore);

        if applied {
            assert!(!result, "expected false on unreadable dir, got true");
        }
    }

    // ------- orchestrator trait surface (R2) ------------------------------

    #[test]
    fn name_returns_cavekit() {
        let o = CavekitOrchestrator::new();
        assert_eq!(o.name(), "cavekit");
    }

    #[test]
    fn engine_returns_claude_code() {
        let o = CavekitOrchestrator::new();
        assert_eq!(o.engine(), "claude-code");
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

    // ------- run() integration via StubMux (R3) ---------------------------

    struct StubMux {
        created: Mutex<Vec<(String, String, PathBuf)>>,
        closed: Mutex<Vec<TabHandle>>,
    }

    impl StubMux {
        fn new() -> Self {
            Self {
                created: Mutex::new(Vec::new()),
                closed: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Multiplexer for StubMux {
        fn kind(&self) -> &'static str {
            "stub"
        }
        async fn ensure_session(&self, _name: &str) -> Result<()> {
            Ok(())
        }
        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            layout_path: &Path,
        ) -> Result<TabHandle> {
            self.created.lock().unwrap().push((
                session.to_string(),
                name.to_string(),
                layout_path.to_path_buf(),
            ));
            Ok(TabHandle::new(session, 1, name))
        }
        async fn close_tab(&self, handle: &TabHandle) -> Result<()> {
            self.closed.lock().unwrap().push(handle.clone());
            Ok(())
        }
        async fn rename_tab(&self, _handle: &TabHandle, _name: &str) -> Result<()> {
            Ok(())
        }
        async fn pipe(&self, _target: &str, _payload: &str) -> Result<()> {
            Ok(())
        }
    }

    fn make_spec(cwd: PathBuf) -> AgentSpec {
        AgentSpec::new(
            AgentId::new("cavekit", "run"),
            "run",
            "cavekit",
            "claude-code",
            cwd,
            vec!["claude".into(), "--resume".into()],
        )
    }

    fn make_world(spec: AgentSpec, mux: Arc<StubMux>) -> (World, EventSink, CancellationToken) {
        let (events, _rx) = ark_types::channel(16);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        let world = World::new(
            spec,
            mux as Arc<dyn Multiplexer>,
            events.clone(),
            cancel.clone(),
            hooks_dir,
            state,
            config,
        );
        (world, events, cancel)
    }

    #[tokio::test]
    async fn run_creates_builder_tab_with_default_layout() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let mux = Arc::new(StubMux::new());
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let outcome = CavekitOrchestrator::new()
            .run(spec.clone(), world)
            .await
            .expect("run");

        match outcome {
            Outcome::Success { artifacts } => assert!(artifacts.is_empty()),
            other => panic!("expected Success, got {other:?}"),
        }

        let created = mux.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "create_tab should be called exactly once");
        assert_eq!(created[0].0, spec.session);
        assert_eq!(created[0].1, "builder");
        assert_eq!(created[0].2, PathBuf::from("builder"));
    }

    #[tokio::test]
    async fn run_respects_spec_layout_override() {
        let cwd = TempDir::new().unwrap();
        let mut spec = make_spec(cwd.path().to_path_buf());
        spec.layout = Some("focused".to_string());
        let mux = Arc::new(StubMux::new());
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let _ = CavekitOrchestrator::new()
            .run(spec.clone(), world)
            .await
            .expect("run");

        let created = mux.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1);
        // label stays "builder" — role/label is structural, not layout-driven.
        assert_eq!(created[0].1, "builder");
        // layout path reflects the override.
        assert_eq!(created[0].2, PathBuf::from("focused"));
    }

    #[tokio::test]
    async fn run_emits_tab_opened_builder_role() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let mux = Arc::new(StubMux::new());
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());
        let mut rx = events.subscribe();

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let run_handle = tokio::spawn(async move {
            CavekitOrchestrator::new()
                .run(spec.clone(), world)
                .await
                .expect("run");
        });

        let mut saw_tab_opened = false;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(AgentEvent::TabOpened { role, label, .. })) => {
                    assert_eq!(role, TabRole::Builder);
                    assert_eq!(label, "builder");
                    saw_tab_opened = true;
                    break;
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => continue,
            }
        }
        assert!(saw_tab_opened, "did not observe TabOpened event");

        run_handle.await.expect("join");
    }

    #[tokio::test]
    async fn run_passes_engine_done_through() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let mux = Arc::new(StubMux::new());
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Failed {
                    reason: "engine-boom".into(),
                },
            });
        });

        let outcome = CavekitOrchestrator::new()
            .run(spec, world)
            .await
            .expect("run");

        match outcome {
            Outcome::Failed { reason } => assert_eq!(reason, "engine-boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_returns_killed_on_cancel_and_closes_tab() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let mux = Arc::new(StubMux::new());
        let (world, _events, cancel) = make_world(spec.clone(), mux.clone());

        let handle = tokio::spawn(async move {
            CavekitOrchestrator::new()
                .run(spec, world)
                .await
                .expect("run")
        });

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancel.cancel();

        let outcome = handle.await.expect("join");
        assert_eq!(outcome, Outcome::Killed);

        let closed = mux.closed.lock().unwrap().clone();
        assert_eq!(closed.len(), 1, "expected one close_tab, got {closed:?}");
        assert_eq!(closed[0].name, "builder");
    }
}
