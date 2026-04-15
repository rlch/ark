//! CavekitOrchestrator — full implementation.
//!
//! Landed in stages:
//! - T-075: `detect(cwd)` (R1) — see module-level detection heuristics below.
//! - T-076: `impl Orchestrator` — `engine()`, `name()`, and a minimal `run`
//!   that opened the builder tab, emitted `TabOpened { role: Builder }`, and
//!   waited for engine `Done` or supervisor cancel.
//! - T-077–T-082: watchers for impl-tracking, ralph-loop, codex findings,
//!   git diff, and phase-driven review tab.
//! - **T-083**: wire all watchers into `run()` and implement the
//!   done-signal resolver (R9): Stop+all-DONE → `Success { artifacts }`,
//!   Stop+pending → wait up to 60s → `Failed`, cancel → `Killed`.
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
//! for that subsection in v1 (ark-core's `Config` is a placeholder), so the
//! precedence is:
//!
//! 1. `spec.layout` (user override at spawn time) — takes precedence if set.
//! 2. Hardcoded default `"builder"` (matches the kit default).
//!
//! ## Watcher gates (R4–R8)
//!
//! The watcher gates (`watch_impl_tracking`, `watch_ralph_loop`,
//! `spawn_review_tab`) are specified by the kit to live under
//! `[orchestrator.cavekit]` in config. Because ark-core's `Config` is a
//! placeholder at T-083 time, we default all gates to `true` at the
//! orchestrator boundary — matching the schema defaults in `ark-config`.
//! When a future packet threads the real config type into `World`, switch
//! `CavekitGates::from_world` to read it.
//!
//! ## Done-signal resolver (R9)
//!
//! 1. On engine `Done` (emitted by claude-code from `Stop` / `SessionEnd`
//!    hooks), query the latest [`ImplTrackingSnapshot`].
//!    - `total == 0` → R9 case d: `Success { artifacts: [] }`.
//!    - `done >= total` → R9 case b: `Success { artifacts: trim(diff) }`.
//!    - else → R9 case c: wait up to 60s for snapshot to flip to
//!      `done >= total`. Timeout → `Failed { reason: "tasks still pending
//!      after 60s" }`.
//! 2. On `world.cancel` → close opened tabs + any `TabOpened`-tracked child
//!    tabs, drain watchers briefly, return `Killed`.
//! 3. Success path waits 500ms for pending tab closes (auto-close,
//!    review-tab-close) to propagate so final status is observable.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use ark_core::{Orchestrator, World};
use ark_types::{AgentEvent, AgentSpec, EventReceiver, Outcome, TabHandle, TabRole};
use async_trait::async_trait;
use tokio::task::JoinSet;

// Watchers (T-077 / T-079 / T-080 / T-081 / T-082). Exposed as standalone
// async fns for wiring into `run()` below.
pub mod watchers;

pub use watchers::{ImplTrackingSnapshot, spawn_impl_tracking_with_snapshot};

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

// ----------------------------------------------------------- constants -------

/// Maximum number of artifact paths we keep on a `Success` outcome. The kit
/// asks for "unique set" without a hard cap; we impose one to keep
/// state.json / status pipe payloads bounded in projects with a noisy
/// worktree. 100 covers the realistic cavekit iteration (single tier of a
/// build site touches tens of files, not hundreds); anything above this is
/// almost certainly an unstaged noise.
const MAX_ARTIFACTS: usize = 100;

/// How long to wait for pending tasks to flip DONE after engine `Stop`
/// before declaring `Failed`. Per R9 case c.
const PENDING_GRACE: Duration = Duration::from_secs(60);

/// Short grace period for watchers (review tab close, auto-close) to drain
/// after a Success outcome so final status is observable. Per R9:
/// "Success path should also wait briefly (500ms) for pending tab closes".
const SUCCESS_DRAIN: Duration = Duration::from_millis(500);

// ----------------------------------------------------------- orchestrator ----

/// Gate flags controlling which watchers spin up. Matches the
/// `[orchestrator.cavekit]` schema in `ark-config`. Defaults are all-on,
/// matching `OrchestratorCavekitSection::default` in the real config.
#[derive(Clone, Copy, Debug)]
pub struct CavekitGates {
    pub watch_impl_tracking: bool,
    pub watch_ralph_loop: bool,
    pub spawn_review_tab: bool,
    pub watch_codex_findings: bool,
    pub watch_git_diff: bool,
}

impl Default for CavekitGates {
    fn default() -> Self {
        Self {
            watch_impl_tracking: true,
            watch_ralph_loop: true,
            spawn_review_tab: true,
            watch_codex_findings: true,
            watch_git_diff: true,
        }
    }
}

/// Cavekit-driving orchestrator. Implements R1–R9 of
/// cavekit-orchestrator-cavekit.md.
#[derive(Debug, Default, Clone, Copy)]
pub struct CavekitOrchestrator {
    gates: CavekitGates,
}

impl CavekitOrchestrator {
    pub const fn new() -> Self {
        Self {
            gates: CavekitGates {
                watch_impl_tracking: true,
                watch_ralph_loop: true,
                spawn_review_tab: true,
                watch_codex_findings: true,
                watch_git_diff: true,
            },
        }
    }

    /// Override watcher gates. Primarily for tests and future config
    /// plumbing. Runtime callers should use [`Self::new`] which takes the
    /// kit defaults.
    pub const fn with_gates(gates: CavekitGates) -> Self {
        Self { gates }
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

    fn engine(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, cwd: &Path) -> bool {
        detect(cwd)
    }

    async fn run(&self, spec: AgentSpec, world: World) -> Result<Outcome> {
        // R3: pick the layout stem. spec.layout overrides the kit default.
        let layout_stem = spec
            .layout
            .as_deref()
            .unwrap_or_else(|| self.default_layout())
            .to_string();
        let layout_path = PathBuf::from(&layout_stem);

        // Open the builder tab.
        let builder_tab = world
            .mux
            .create_tab(&spec.session, "builder", &layout_path)
            .await?;

        let _ = world.events.send(AgentEvent::TabOpened {
            id: spec.id.clone(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: builder_tab.clone(),
            label: "builder".to_string(),
        });

        // Subscribe BEFORE spawning the watchers so we can observe their
        // own `TabOpened` (review) events and collect them as child tabs to
        // close on cancel.
        let mut events = world.events.subscribe();

        // Spawn watchers. Each watcher is gated independently; disabled
        // ones short-circuit with `Ok(())` on their first await so it's
        // fine to unconditionally register them — this keeps the JoinSet
        // child-count stable across gate permutations.
        let mut watchers: JoinSet<Result<()>> = JoinSet::new();

        // R4: impl-tracking + snapshot channel for the R9 resolver.
        let (snapshot_rx, impl_handle) = spawn_impl_tracking_with_snapshot(
            spec.cwd.clone(),
            spec.id.clone(),
            world.events.clone(),
            world.cancel.clone(),
            self.gates.watch_impl_tracking,
        );
        // Turn the bare JoinHandle from spawn_impl_tracking_with_snapshot
        // into a JoinSet entry so cancel-path draining is uniform.
        watchers.spawn(async move {
            match impl_handle.await {
                Ok(res) => res,
                Err(e) => Err(anyhow::anyhow!("impl-tracking join error: {e}")),
            }
        });

        // R5: ralph-loop.
        watchers.spawn(watchers::watch_ralph_loop(
            spec.cwd.clone(),
            spec.id.clone(),
            world.events.clone(),
            world.cancel.clone(),
            self.gates.watch_ralph_loop,
        ));

        // R7: codex findings.
        watchers.spawn(watchers::watch_codex_findings(
            spec.cwd.clone(),
            spec.id.clone(),
            world.events.clone(),
            world.cancel.clone(),
            self.gates.watch_codex_findings,
        ));

        // R8: git diff / FileEdited.
        if self.gates.watch_git_diff {
            watchers.spawn(watchers::watch_git_diff(
                spec.cwd.clone(),
                spec.id.clone(),
                world.events.clone(),
                world.cancel.clone(),
            ));
        }

        // R6: phase detection → review tab. Needs its own event-bus
        // receiver.
        let review_rx = world.events.subscribe();
        watchers.spawn(watchers::watch_phase_and_review(
            spec.cwd.clone(),
            spec.id.clone(),
            review_rx,
            world.mux.clone(),
            spec.session.clone(),
            world.events.clone(),
            world.cancel.clone(),
            self.gates.spawn_review_tab,
        ));

        // ---- main wait loop -------------------------------------------------

        // Accumulators, populated from the event bus as watchers emit.
        let mut diff_paths: Vec<PathBuf> = Vec::new();
        let mut child_tabs: HashMap<String, TabHandle> = HashMap::new(); // key = label

        loop {
            tokio::select! {
                biased;
                _ = world.cancel.cancelled() => {
                    // R9 path 3: Cancel → close all tabs, drain watchers,
                    // return Killed.
                    close_all_tabs(&world, &builder_tab, &child_tabs).await;
                    drain_watchers(&mut watchers, SUCCESS_DRAIN).await;
                    return Ok(Outcome::Killed);
                }
                res = events.recv() => {
                    match res {
                        Ok(AgentEvent::TabOpened {
                            label,
                            tab_handle,
                            role:
                                TabRole::Reviewer
                                | TabRole::Subagent
                                | TabRole::Log
                                | TabRole::Custom(_),
                            ..
                        }) => {
                            // Track non-builder tabs so cancel can close them.
                            child_tabs.insert(label, tab_handle);
                        }
                        Ok(AgentEvent::TabClosed { tab_handle, .. }) => {
                            child_tabs.retain(|_, h| h != &tab_handle);
                        }
                        Ok(AgentEvent::FileEdited { path, .. }) => {
                            diff_paths.push(path);
                        }
                        Ok(AgentEvent::Done { id, outcome }) if id == spec.id => {
                            // R9 resolution happens here. The engine emits
                            // Done { Success } on Stop/SessionEnd; we may
                            // override it based on impl-tracking state.
                            let outcome = resolve_done_outcome(
                                outcome,
                                &snapshot_rx,
                                &mut events,
                                &mut diff_paths,
                            )
                            .await;

                            // R9: brief drain so tab-close propagation is
                            // observable to downstream consumers.
                            drain_watchers(&mut watchers, SUCCESS_DRAIN).await;
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

// ----------------------------------------------------- done-signal resolver --

/// Apply R9 logic to the engine-level `Done` outcome. Returns the final
/// outcome the orchestrator should surface.
///
/// `events` is kept alive so we can observe additional `FileEdited` events
/// that arrive while we wait out the 60s pending-grace window (tests drive
/// this via `tokio::time::pause`).
async fn resolve_done_outcome(
    engine_outcome: Outcome,
    snapshot_rx: &tokio::sync::watch::Receiver<ImplTrackingSnapshot>,
    events: &mut EventReceiver,
    diff_paths: &mut Vec<PathBuf>,
) -> Outcome {
    // If the engine already decided Failed/Killed/Timeout/Crashed, pass
    // through. The resolver only upgrades/confirms a Success.
    match &engine_outcome {
        Outcome::Success { .. } => {}
        _ => return engine_outcome,
    }

    let snap = snapshot_rx.borrow().clone();

    // R9 case d: unknown total → trivial Success with empty artifacts.
    if snap.total == 0 {
        return Outcome::Success {
            artifacts: Vec::new(),
        };
    }

    // R9 case b: all DONE.
    if snap.done >= snap.total {
        let artifacts = trim_artifacts(diff_paths.clone());
        return Outcome::Success { artifacts };
    }

    // R9 case c: tasks still pending → wait up to 60s for the snapshot to
    // flip done>=total (or for a `TaskDone` event to land). We poll via
    // `changed()` which wakes on every snapshot republish.
    let mut rx = snapshot_rx.clone();
    let wait_result = tokio::time::timeout(PENDING_GRACE, async {
        loop {
            // Also consume FileEdited events while waiting so artifacts
            // remain fresh when we hit the timeout path.
            tokio::select! {
                biased;
                changed = rx.changed() => {
                    if changed.is_err() {
                        // Snapshot sender dropped — bail out with whatever
                        // we have.
                        return false;
                    }
                    let s = rx.borrow().clone();
                    if s.total > 0 && s.done >= s.total {
                        return true;
                    }
                }
                ev = events.recv() => {
                    match ev {
                        Ok(AgentEvent::FileEdited { path, .. }) => {
                            diff_paths.push(path);
                        }
                        // Ignore everything else while waiting.
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return false;
                        }
                    }
                }
            }
        }
    })
    .await;

    match wait_result {
        Ok(true) => Outcome::Success {
            artifacts: trim_artifacts(diff_paths.clone()),
        },
        _ => Outcome::Failed {
            reason: "tasks still pending after 60s".to_string(),
        },
    }
}

/// Dedupe + sort + cap a list of artifact paths.
///
/// Called on the Success path. Per R9, artifacts are the set of files
/// modified during the run (derived from `FileEdited` events on the bus,
/// which originate in the T-082 git-diff watcher). We sort for
/// deterministic output and cap at [`MAX_ARTIFACTS`] to keep state payloads
/// bounded.
pub fn trim_artifacts(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_ARTIFACTS {
        paths.truncate(MAX_ARTIFACTS);
    }
    paths
}

/// Close the builder tab + any tracked child tabs. Errors are logged but
/// swallowed — the supervisor tears down the session on drop.
async fn close_all_tabs(
    world: &World,
    builder: &TabHandle,
    child_tabs: &HashMap<String, TabHandle>,
) {
    for (_, handle) in child_tabs.iter() {
        if let Err(e) = world.mux.close_tab(handle).await {
            tracing::debug!(error = %e, name = %handle.name, "close_tab failed");
        }
    }
    if let Err(e) = world.mux.close_tab(builder).await {
        tracing::debug!(error = %e, name = %builder.name, "close_tab (builder) failed");
    }
}

/// Give the watcher JoinSet `grace` to drain. We don't cancel it here —
/// cancelling is the cancel-path's job via `world.cancel`; this is the
/// post-Success hand-off where we want watchers to keep emitting briefly
/// so final state is observable.
async fn drain_watchers(watchers: &mut JoinSet<Result<()>>, grace: Duration) {
    let deadline = tokio::time::Instant::now() + grace;
    while !watchers.is_empty() {
        match tokio::time::timeout_at(deadline, watchers.join_next()).await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break, // grace expired
        }
    }
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::Config;
    use ark_mux_zellij::ZellijMux;
    use ark_mux_zellij::executor::{CommandOutput, StubExecutor};
    use ark_types::{AgentId, CancellationToken, EventSink, StateLayout};
    use std::sync::Arc;
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
        let mut perms = fs::metadata(&plans).unwrap().permissions();
        perms.set_mode(0o000);
        let applied = fs::set_permissions(&plans, perms).is_ok();

        let result = detect(dir.path());

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

    // ------- trim_artifacts ----------------------------------------------

    #[test]
    fn trim_artifacts_dedupes_sorts_and_caps() {
        let input: Vec<PathBuf> = vec![
            PathBuf::from("b.rs"),
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"), // duplicate
            PathBuf::from("c.rs"),
        ];
        let out = trim_artifacts(input);
        assert_eq!(
            out,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("c.rs"),
            ]
        );
    }

    #[test]
    fn trim_artifacts_caps_at_100() {
        let many: Vec<PathBuf> = (0..250)
            .map(|i| PathBuf::from(format!("f{i:04}.rs")))
            .collect();
        let out = trim_artifacts(many);
        assert_eq!(out.len(), 100);
        // After sort, first 100 are f0000..f0099.
        assert_eq!(out[0], PathBuf::from("f0000.rs"));
        assert_eq!(out[99], PathBuf::from("f0099.rs"));
    }

    #[test]
    fn trim_artifacts_empty() {
        assert!(trim_artifacts(Vec::new()).is_empty());
    }

    // ------- run() integration via ZellijMux(StubExecutor) ----------------

    /// Build a `ZellijMux` backed by a `StubExecutor` pre-seeded with `n`
    /// ok-status responses. We use the inside-zellij path so
    /// `create_tab`'s first-tab spawn routes through `zellij action
    /// switch-session --layout <p>` (observable via the executor) rather
    /// than the outside-zellij pty path (which tries to spawn a real
    /// zellij binary, unavailable in this test env).
    async fn test_mux(n: usize) -> (Arc<ZellijMux>, Arc<StubExecutor>) {
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let responses: Vec<CommandOutput> = (0..n)
            .map(|_| CommandOutput {
                status: ok_status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .collect();
        let (mux, stub) = ZellijMux::for_test_in_zellij(responses);
        (Arc::new(mux), stub)
    }

    /// Count zellij argv sequences that correspond to `create_tab`.
    /// Inside zellij the first tab uses `switch-session --layout`; any
    /// additional tab uses `action new-tab --layout`.
    fn count_create_tab_calls(stub: &StubExecutor) -> usize {
        stub.recorded_calls()
            .iter()
            .filter(|(_, args)| {
                let has_switch = args.iter().any(|a| a == "switch-session");
                let has_new_tab = args.iter().any(|a| a == "new-tab");
                has_switch || has_new_tab
            })
            .count()
    }

    /// Argv sequences recorded as `close-tab-at-index` calls.
    fn close_argvs(stub: &StubExecutor) -> Vec<Vec<String>> {
        stub.recorded_calls()
            .into_iter()
            .filter(|(_, args)| args.iter().any(|a| a == "close-tab-at-index"))
            .map(|(_, args)| args)
            .collect()
    }

    fn count_close_tab_calls(stub: &StubExecutor) -> usize {
        close_argvs(stub).len()
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

    fn make_world(
        spec: AgentSpec,
        mux: Arc<ZellijMux>,
    ) -> (World, EventSink, CancellationToken) {
        let (events, _rx) = ark_types::channel(256);
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
            mux,
            events.clone(),
            cancel.clone(),
            hooks_dir,
            state,
            config,
        );
        (world, events, cancel)
    }

    /// Orchestrator with all gates OFF except we still hand-craft the
    /// impl-tracking snapshot via the channel we care about for the test.
    /// Used for run() tests where we want to drive the resolver without
    /// real filesystem watchers.
    fn all_gates_off() -> CavekitOrchestrator {
        CavekitOrchestrator::with_gates(CavekitGates {
            watch_impl_tracking: false,
            watch_ralph_loop: false,
            spawn_review_tab: false,
            watch_codex_findings: false,
            watch_git_diff: false,
        })
    }

    #[tokio::test]
    async fn run_creates_builder_tab_with_default_layout() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        // One scripted ok for the builder create_tab; plus cushion.
        let (mux, stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let outcome = all_gates_off().run(spec.clone(), world).await.expect("run");

        match outcome {
            Outcome::Success { artifacts } => assert!(artifacts.is_empty()),
            other => panic!("expected Success, got {other:?}"),
        }

        assert_eq!(
            count_create_tab_calls(&stub),
            1,
            "create_tab should be called exactly once; got: {:?}",
            stub.recorded_calls()
        );
        // Argv should mention the session and the default "builder" layout.
        let calls = stub.recorded_calls();
        let (_, argv) = calls
            .iter()
            .find(|(_, args)| args.iter().any(|a| a == "switch-session"))
            .expect("expected switch-session call");
        assert!(argv.iter().any(|a| a == spec.session.as_str()));
        let layout_pos = argv.iter().position(|a| a == "--layout").unwrap();
        assert_eq!(argv[layout_pos + 1], "builder");
    }

    #[tokio::test]
    async fn run_respects_spec_layout_override() {
        let cwd = TempDir::new().unwrap();
        let mut spec = make_spec(cwd.path().to_path_buf());
        spec.layout = Some("focused".to_string());
        let (mux, stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let _ = all_gates_off().run(spec.clone(), world).await.expect("run");

        assert_eq!(count_create_tab_calls(&stub), 1);
        let calls = stub.recorded_calls();
        let (_, argv) = calls
            .iter()
            .find(|(_, args)| args.iter().any(|a| a == "switch-session"))
            .expect("expected switch-session call");
        let layout_pos = argv.iter().position(|a| a == "--layout").unwrap();
        assert_eq!(
            argv[layout_pos + 1],
            "focused",
            "layout override must propagate to zellij argv"
        );
    }

    #[tokio::test]
    async fn run_emits_tab_opened_builder_role() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());
        let mut rx = events.subscribe();

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let run_handle = tokio::spawn(async move {
            all_gates_off().run(spec.clone(), world).await.expect("run");
        });

        let mut saw_tab_opened = false;
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
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
    async fn run_passes_engine_failed_through() {
        // Engine Failed is NOT overridden by the R9 resolver — only Success
        // is re-evaluated.
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Failed {
                    reason: "engine-boom".into(),
                },
            });
        });

        let outcome = all_gates_off().run(spec, world).await.expect("run");
        match outcome {
            Outcome::Failed { reason } => assert_eq!(reason, "engine-boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_returns_killed_on_cancel_and_closes_tab() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        // create + close
        let (mux, stub) = test_mux(4).await;
        let (world, _events, cancel) = make_world(spec.clone(), mux.clone());

        let handle =
            tokio::spawn(async move { all_gates_off().run(spec, world).await.expect("run") });

        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();

        let outcome = handle.await.expect("join");
        assert_eq!(outcome, Outcome::Killed);

        // The builder tab's index is 0 inside zellij (first tab). Assert
        // the close argv mentions index 0.
        assert!(
            count_close_tab_calls(&stub) >= 1,
            "expected at least the builder to close; calls: {:?}",
            stub.recorded_calls()
        );
        let closes = close_argvs(&stub);
        let indices: Vec<&str> = closes
            .iter()
            .map(|a| {
                let pos = a.iter().position(|x| x == "close-tab-at-index").unwrap();
                a[pos + 1].as_str()
            })
            .collect();
        assert!(
            indices.contains(&"0"),
            "builder tab (index 0) must be closed; got indices: {indices:?}"
        );
    }

    // ------- R9 resolver tests — filesystem-backed impl tracking -----------

    /// Helper: write an impl-tracking file with the given rows.
    /// Each row is `(task_id, status, notes)`.
    fn write_impl_file(cwd: &Path, rows: &[(&str, &str, &str)]) {
        let impl_dir = cwd.join("context").join("impl");
        fs::create_dir_all(&impl_dir).unwrap();
        let mut body = String::from("| Task | Status | Notes |\n| --- | --- | --- |\n");
        for (id, status, notes) in rows {
            body.push_str(&format!("| {id} | {status} | {notes} |\n"));
        }
        fs::write(impl_dir.join("impl-site.md"), body).unwrap();
    }

    /// Helper: write a build-site file with the given task ids (drives
    /// `Progress.total`).
    fn write_build_site(cwd: &Path, task_ids: &[&str]) {
        let plans = cwd.join("context").join("plans");
        fs::create_dir_all(&plans).unwrap();
        let mut body = String::new();
        for id in task_ids {
            body.push_str(&format!("| {id} | desc | S |\n"));
        }
        fs::write(plans.join("build-site.md"), body).unwrap();
    }

    /// Drive `run()` with watchers enabled. Helper returns the JoinHandle so
    /// callers can trigger a `Done` event and await the outcome.
    fn spawn_run(
        orch: CavekitOrchestrator,
        spec: AgentSpec,
        world: World,
    ) -> tokio::task::JoinHandle<Result<Outcome>> {
        tokio::spawn(async move { orch.run(spec, world).await })
    }

    async fn wait_for_progress_total(
        rx: &mut ark_types::EventReceiver,
        expected_total: u32,
        timeout: Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(AgentEvent::Progress { total, .. })) if total == expected_total => {
                    return true;
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => continue,
            }
        }
        false
    }

    #[tokio::test]
    async fn run_stop_with_all_done_returns_success() {
        let cwd = TempDir::new().unwrap();
        // 2 tasks, both DONE.
        write_build_site(cwd.path(), &["T-001", "T-002"]);
        write_impl_file(
            cwd.path(),
            &[("T-001", "DONE", "shipped"), ("T-002", "DONE", "shipped")],
        );

        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(8).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());
        let mut probe = events.subscribe();

        let orch = CavekitOrchestrator::with_gates(CavekitGates {
            watch_impl_tracking: true,
            watch_ralph_loop: false,
            spawn_review_tab: false,
            watch_codex_findings: false,
            watch_git_diff: false,
        });
        let handle = spawn_run(orch, spec.clone(), world);

        // Wait for the initial Progress to publish total=2 so we know the
        // snapshot is loaded before we fire Done.
        assert!(
            wait_for_progress_total(&mut probe, 2, Duration::from_secs(3)).await,
            "did not observe Progress with total=2"
        );

        let _ = events.send(AgentEvent::Done {
            id: spec.id.clone(),
            outcome: Outcome::Success {
                artifacts: Vec::new(),
            },
        });

        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run timeout")
            .expect("join")
            .expect("run ok");
        match outcome {
            Outcome::Success { .. } => {}
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_stop_no_build_site_returns_success_empty_artifacts() {
        // R9 case d: no build-site, total=0 → Success with empty artifacts.
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(8).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let orch = CavekitOrchestrator::with_gates(CavekitGates {
            watch_impl_tracking: true,
            watch_ralph_loop: false,
            spawn_review_tab: false,
            watch_codex_findings: false,
            watch_git_diff: false,
        });
        let handle = spawn_run(orch, spec.clone(), world);

        // No progress to wait on — the empty context/impl means the watcher
        // publishes a snapshot with total=0. Give it a short beat to run
        // the initial parse.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let _ = events.send(AgentEvent::Done {
            id: spec.id.clone(),
            outcome: Outcome::Success {
                artifacts: Vec::new(),
            },
        });

        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run timeout")
            .expect("join")
            .expect("run ok");
        match outcome {
            Outcome::Success { artifacts } => assert!(artifacts.is_empty()),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_stop_with_pending_then_done_within_grace_returns_success() {
        // R9 case c-success: Stop arrives with a PENDING task, then the
        // task flips DONE within the grace window → Success.
        let cwd = TempDir::new().unwrap();
        write_build_site(cwd.path(), &["T-001", "T-002"]);
        write_impl_file(
            cwd.path(),
            &[
                ("T-001", "DONE", "shipped"),
                ("T-002", "PENDING", "not yet"),
            ],
        );

        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(8).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());
        let mut probe = events.subscribe();

        let orch = CavekitOrchestrator::with_gates(CavekitGates {
            watch_impl_tracking: true,
            watch_ralph_loop: false,
            spawn_review_tab: false,
            watch_codex_findings: false,
            watch_git_diff: false,
        });
        let handle = spawn_run(orch, spec.clone(), world);

        assert!(
            wait_for_progress_total(&mut probe, 2, Duration::from_secs(3)).await,
            "did not observe initial Progress"
        );

        let _ = events.send(AgentEvent::Done {
            id: spec.id.clone(),
            outcome: Outcome::Success {
                artifacts: Vec::new(),
            },
        });

        // Give the resolver a moment to enter the waiting branch, then flip
        // T-002 to DONE. The watcher's 500ms debounce + re-parse will
        // republish the snapshot and unblock the resolver.
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_impl_file(
            cwd.path(),
            &[("T-001", "DONE", "shipped"), ("T-002", "DONE", "finally")],
        );

        let outcome = tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("run timeout")
            .expect("join")
            .expect("run ok");
        match outcome {
            Outcome::Success { .. } => {}
            other => panic!("expected Success after pending→done, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn run_stop_with_pending_times_out_to_failed() {
        // R9 case c-failure: Stop + still-PENDING after 60s → Failed.
        //
        // Uses `start_paused = true` so the 60s grace compresses to an
        // instant advance. The filesystem watcher's initial parse runs
        // async but doesn't block on the paused clock (it uses
        // `tokio::fs::read_dir`, which is CPU-bound, not timer-bound).
        //
        // Strategy: we don't rely on the real impl-tracking watcher here.
        // Instead, we construct the orchestrator with all gates off and
        // directly feed a `Done` event plus a seeded snapshot via the
        // `resolve_done_outcome` fn — but since `run()` doesn't expose the
        // snapshot_rx, we take the integration path: build a spec that
        // causes the watcher to observe a PENDING task, fire Done, and
        // advance past the grace.
        let cwd = TempDir::new().unwrap();
        write_build_site(cwd.path(), &["T-001", "T-002"]);
        write_impl_file(
            cwd.path(),
            &[
                ("T-001", "DONE", "shipped"),
                ("T-002", "PENDING", "will never finish"),
            ],
        );

        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(8).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let orch = CavekitOrchestrator::with_gates(CavekitGates {
            watch_impl_tracking: true,
            watch_ralph_loop: false,
            spawn_review_tab: false,
            watch_codex_findings: false,
            watch_git_diff: false,
        });
        let mut probe = events.subscribe();
        let handle = spawn_run(orch, spec.clone(), world);

        // Yield so the watcher's initial parse can publish a snapshot.
        // Under `start_paused`, only explicit advances tick the clock, so
        // we use a tiny advance to let the scheduler run waiting tasks.
        for _ in 0..50 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
            if matches!(probe.try_recv(), Ok(AgentEvent::Progress { total: 2, .. })) {
                break;
            }
        }

        // Fire Done.
        let _ = events.send(AgentEvent::Done {
            id: spec.id.clone(),
            outcome: Outcome::Success {
                artifacts: Vec::new(),
            },
        });

        // Advance past the 60s grace. The resolver's `tokio::time::timeout`
        // deadline fires on next poll.
        tokio::time::advance(Duration::from_secs(61)).await;
        // Advance past the post-resolver `drain_watchers` grace too.
        tokio::time::advance(SUCCESS_DRAIN + Duration::from_millis(100)).await;

        // Join the handle. Under paused-clock mode, use try_join loops
        // rather than timeout-based waits (timeouts also consult the
        // paused clock and need an advance to fire).
        let mut handle = handle;
        let outcome = loop {
            tokio::task::yield_now().await;
            match tokio::time::timeout(Duration::from_millis(10), &mut handle).await {
                Ok(res) => break res.expect("join").expect("run ok"),
                Err(_) => {
                    tokio::time::advance(Duration::from_millis(50)).await;
                }
            }
        };
        match outcome {
            Outcome::Failed { reason } => {
                assert!(
                    reason.contains("60s"),
                    "expected 60s in reason, got: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_cancel_closes_all_tabs_including_children() {
        // Simulate a child tab opened by a watcher, then cancel, and
        // verify the builder AND the child are closed.
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, stub) = test_mux(8).await;
        let (world, events, cancel) = make_world(spec.clone(), mux.clone());

        let orch = all_gates_off();
        let handle = spawn_run(orch, spec.clone(), world);

        // Emit a synthetic TabOpened { Reviewer } to populate child_tabs.
        // (In production, watch_phase_and_review would emit this.)
        tokio::time::sleep(Duration::from_millis(50)).await;
        let child_handle = TabHandle::new(&spec.session, 99, "review");
        let _ = events.send(AgentEvent::TabOpened {
            id: spec.id.clone(),
            parent: None,
            role: TabRole::Reviewer,
            tab_handle: child_handle.clone(),
            label: "review".to_string(),
        });

        // Give the orchestrator a moment to observe the TabOpened.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let outcome = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run timeout")
            .expect("join")
            .expect("run ok");
        assert_eq!(outcome, Outcome::Killed);

        // Builder tab lives at index 0 (first inside-zellij tab). Child
        // TabHandle was constructed with index 99 above. Assert both
        // appear among close-tab-at-index argvs.
        let closes = close_argvs(&stub);
        let indices: Vec<String> = closes
            .iter()
            .map(|a| {
                let pos = a.iter().position(|x| x == "close-tab-at-index").unwrap();
                a[pos + 1].clone()
            })
            .collect();
        assert!(
            indices.contains(&"0".to_string()),
            "builder (index 0) not closed; got indices: {indices:?}"
        );
        assert!(
            indices.contains(&"99".to_string()),
            "review child (index 99) not closed; got indices: {indices:?}"
        );
    }
}
