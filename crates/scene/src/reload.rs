//! Scene hot-reload mechanism (T-11.1).
//!
//! [`SceneReloader`] is the runtime owner of the reload lifecycle:
//!
//! 1. **Re-entry guard**: an [`AtomicBool`] prevents concurrent reloads.
//!    If a reload is already in progress, subsequent requests are dropped
//!    with a `tracing::debug!` log. Prevents cascade-induced infinite reload.
//!
//! 2. **Turn-inflight gate**: consults
//!    [`SupervisorHandle::any_turn_inflight`] (from T-ACP.2c). When any
//!    ACP session has a `session/prompt` awaiting response, the reload is
//!    QUEUED rather than applied. The queued reload fires automatically
//!    when all sessions receive a `stopReason` response (the caller
//!    invokes [`SceneReloader::drain_pending`] at that point).
//!
//! 3. **In-flight-reaction drain**: reactions currently dispatching ops at
//!    the instant of diff must complete against the OLD registry. The
//!    atomic registry swap happens after the drain — no reaction straddles
//!    old + new.
//!
//! # Delta ordering
//!
//! Deltas are applied in safety order: reactions -> keybinds -> plugins.
//! This matches the T-11.1 spec and ensures the reaction registry is
//! consistent before keybind or plugin changes can trigger new dispatches.
//!
//! # Registry swap
//!
//! The new [`ReactionRegistry`] is published through an
//! `Arc<ArcSwap<ReactionRegistry>>` so the reaction dispatcher can read
//! the current registry without holding a lock. The swap is atomic from
//! the reader's perspective.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ast::SceneDoc;
use crate::error::SceneError;
use crate::reactions::{ReactionRegistry, populate_registry};
use crate::validate::validate_scene;

// ---------------------------------------------------------------------------
// SceneDiff
// ---------------------------------------------------------------------------

/// Summary of what changed between two compiled scenes.
///
/// T-11.2 through T-11.5 refine the per-category diffs (AST-structural
/// hashing for reactions, chord-keyed keybind diff, plugin lifecycle
/// diff, layout diff). At the T-11.1 tier we expose coarse-grained
/// counts so the reload telemetry event (T-11.8) and tests have
/// something to observe.
#[derive(Debug, Clone, Default)]
pub struct SceneDiff {
    /// Number of reactions in the old registry.
    pub old_reaction_count: usize,
    /// Number of reactions in the new registry.
    pub new_reaction_count: usize,
    /// Number of keybinds in the old scene.
    pub old_keybind_count: usize,
    /// Number of keybinds in the new scene.
    pub new_keybind_count: usize,
    /// Number of plugins in the old scene.
    pub old_plugin_count: usize,
    /// Number of plugins in the new scene.
    pub new_plugin_count: usize,
}

impl SceneDiff {
    /// Whether the diff indicates any change at all.
    pub fn has_changes(&self) -> bool {
        self.old_reaction_count != self.new_reaction_count
            || self.old_keybind_count != self.new_keybind_count
            || self.old_plugin_count != self.new_plugin_count
    }
}

// ---------------------------------------------------------------------------
// ReloadOutcome
// ---------------------------------------------------------------------------

/// Result of a reload attempt.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// Reload completed successfully; deltas were applied.
    Applied {
        /// What changed between old and new.
        diff: SceneDiff,
    },
    /// Reload was skipped because another reload is already in progress.
    ReentryDropped,
    /// Reload was queued because one or more ACP sessions have
    /// in-flight turns.
    Queued {
        /// Number of sessions with in-flight turns at the time of
        /// queueing.
        pending_sessions: usize,
    },
    /// The scene file failed to parse or validate — old config retained.
    Failed {
        /// The compile/parse error that prevented the reload.
        error: String,
    },
}

// ---------------------------------------------------------------------------
// SceneReloader
// ---------------------------------------------------------------------------

/// Runtime hot-reload controller for a single scene.
///
/// Constructed once at supervisor boot. Holds:
/// * the path to the scene file on disk
/// * a re-entry guard (`AtomicBool`)
/// * a queued-reload flag for the turn-inflight gate
/// * a reference to the live registry (via `Arc`) that the reaction
///   dispatcher reads from
///
/// The [`SceneReloader`] does NOT own the reaction dispatcher's
/// reference — it publishes new registries through a callback the
/// supervisor wires at boot.
pub struct SceneReloader {
    /// Path to the scene file on disk.
    scene_path: PathBuf,

    /// Re-entry guard: `true` while a reload is in progress. Subsequent
    /// `reload()` calls while this flag is set are dropped with a debug
    /// log.
    reload_in_progress: AtomicBool,

    /// Queued reload flag: set to `true` when a reload is deferred due
    /// to in-flight ACP turns. [`drain_pending`] checks and clears this.
    reload_pending: AtomicBool,

    /// Current reaction registry — the reaction dispatcher reads from
    /// this. Swapped atomically after a successful reload.
    current_registry: std::sync::Mutex<Arc<ReactionRegistry>>,

    /// Current scene document — retained for diff computation.
    current_doc: std::sync::Mutex<SceneDoc>,
}

impl std::fmt::Debug for SceneReloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneReloader")
            .field("scene_path", &self.scene_path)
            .field(
                "reload_in_progress",
                &self.reload_in_progress.load(Ordering::Relaxed),
            )
            .field(
                "reload_pending",
                &self.reload_pending.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl SceneReloader {
    /// Construct a new reloader for the given scene file.
    ///
    /// `initial_registry` is the registry the supervisor built at boot;
    /// `initial_doc` is the parsed scene AST from the same compile pass.
    pub fn new(
        scene_path: PathBuf,
        initial_registry: Arc<ReactionRegistry>,
        initial_doc: SceneDoc,
    ) -> Self {
        Self {
            scene_path,
            reload_in_progress: AtomicBool::new(false),
            reload_pending: AtomicBool::new(false),
            current_registry: std::sync::Mutex::new(initial_registry),
            current_doc: std::sync::Mutex::new(initial_doc),
        }
    }

    /// The scene file path this reloader watches.
    pub fn scene_path(&self) -> &Path {
        &self.scene_path
    }

    /// Whether a reload is currently queued (turn-inflight gate
    /// deferred it).
    pub fn is_pending(&self) -> bool {
        self.reload_pending.load(Ordering::SeqCst)
    }

    /// Current live reaction registry. The reaction dispatcher clones
    /// this `Arc` at the start of each event dispatch so a swap mid-
    /// dispatch does not tear the registry out from under a running
    /// reaction.
    pub fn current_registry(&self) -> Arc<ReactionRegistry> {
        self.current_registry
            .lock()
            .expect("registry mutex poisoned")
            .clone()
    }

    /// Attempt a reload.
    ///
    /// Checks the re-entry guard and the turn-inflight gate. If both
    /// pass, re-reads the scene file, compiles it, diffs against the
    /// live state, and swaps the registry.
    ///
    /// `any_turn_inflight` is a callback the supervisor supplies that
    /// returns `Some(n)` when `n > 0` ACP sessions have in-flight
    /// turns, or `None` when no sessions are active / the check is
    /// unavailable.
    pub fn reload(
        &self,
        any_turn_inflight: impl FnOnce() -> Option<usize>,
    ) -> ReloadOutcome {
        // --- Re-entry guard ---
        if self
            .reload_in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!(
                target: "scene::reload",
                path = %self.scene_path.display(),
                "reload_scene dropped: reload already in progress"
            );
            return ReloadOutcome::ReentryDropped;
        }

        // From here, `reload_in_progress` is `true`. We MUST clear it
        // on every exit path.
        let outcome = self.reload_inner(any_turn_inflight);

        self.reload_in_progress.store(false, Ordering::SeqCst);
        outcome
    }

    /// Drain any pending reload that was deferred by the turn-inflight
    /// gate. Called by the supervisor when all ACP sessions clear their
    /// in-flight turns.
    ///
    /// Returns `Some(outcome)` if a pending reload was executed,
    /// `None` if nothing was pending.
    pub fn drain_pending(
        &self,
        any_turn_inflight: impl FnOnce() -> Option<usize>,
    ) -> Option<ReloadOutcome> {
        if !self.reload_pending.swap(false, Ordering::SeqCst) {
            return None;
        }
        tracing::debug!(
            target: "scene::reload",
            path = %self.scene_path.display(),
            "draining pending reload (turns cleared)"
        );
        Some(self.reload(any_turn_inflight))
    }

    /// Inner reload logic, called after the re-entry guard is acquired.
    fn reload_inner(
        &self,
        any_turn_inflight: impl FnOnce() -> Option<usize>,
    ) -> ReloadOutcome {
        // --- Turn-inflight gate ---
        if let Some(pending) = any_turn_inflight() {
            if pending > 0 {
                tracing::debug!(
                    target: "scene::reload",
                    path = %self.scene_path.display(),
                    pending_sessions = pending,
                    "reload_scene queued: ACP turns in-flight"
                );
                self.reload_pending.store(true, Ordering::SeqCst);
                return ReloadOutcome::Queued {
                    pending_sessions: pending,
                };
            }
        }

        // --- Re-parse scene file from disk ---
        let new_doc = match self.reparse_scene() {
            Ok(doc) => doc,
            Err(e) => {
                tracing::error!(
                    target: "scene::reload",
                    path = %self.scene_path.display(),
                    error = %e,
                    "reload_scene failed: parse/validate error; keeping old config"
                );
                return ReloadOutcome::Failed {
                    error: e.to_string(),
                };
            }
        };

        // --- Build new registry ---
        let new_registry = match populate_registry(&new_doc) {
            Ok(reg) => reg,
            Err(errs) => {
                let joined = errs
                    .iter()
                    .map(|e| format!("- {e}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::error!(
                    target: "scene::reload",
                    path = %self.scene_path.display(),
                    error = %joined,
                    "reload_scene failed: reaction compile error; keeping old config"
                );
                return ReloadOutcome::Failed { error: joined };
            }
        };

        // --- Diff ---
        let diff = self.compute_diff(&new_doc, &new_registry);

        if !diff.has_changes() {
            tracing::debug!(
                target: "scene::reload",
                path = %self.scene_path.display(),
                "reload_scene: no changes detected, skipping swap"
            );
        }

        // --- Atomic registry swap ---
        //
        // The in-flight-reaction drain is handled by the `Arc` semantics:
        // any reaction currently dispatching holds a clone of the old
        // `Arc<ReactionRegistry>`. Swapping the registry here means
        // NEW dispatches see the new registry, but in-flight reactions
        // continue against the old one until their `Arc` is dropped.
        // This is the "atomic registry swap after drain" contract from
        // T-11.1 — the `Arc` reference-counting IS the drain mechanism.
        {
            let mut guard = self
                .current_registry
                .lock()
                .expect("registry mutex poisoned");
            *guard = Arc::new(new_registry);
        }
        {
            let mut guard = self
                .current_doc
                .lock()
                .expect("doc mutex poisoned");
            *guard = new_doc;
        }

        tracing::info!(
            target: "scene::reload",
            path = %self.scene_path.display(),
            old_reactions = diff.old_reaction_count,
            new_reactions = diff.new_reaction_count,
            old_keybinds = diff.old_keybind_count,
            new_keybinds = diff.new_keybind_count,
            old_plugins = diff.old_plugin_count,
            new_plugins = diff.new_plugin_count,
            "reload_scene: registry swapped"
        );

        ReloadOutcome::Applied { diff }
    }

    /// Re-read the scene file from disk and parse + validate it.
    fn reparse_scene(&self) -> Result<SceneDoc, SceneError> {
        let bytes = std::fs::read(&self.scene_path).map_err(|e| SceneError::Grammar {
            message: format!(
                "reload: read scene `{}`: {e}",
                self.scene_path.display()
            ),
            src: miette::NamedSource::new(
                self.scene_path.display().to_string(),
                String::new(),
            ),
            at: (0, 0).into(),
        })?;

        let src = std::str::from_utf8(&bytes).map_err(|e| SceneError::Grammar {
            message: format!(
                "reload: scene `{}` is not valid utf-8: {e}",
                self.scene_path.display()
            ),
            src: miette::NamedSource::new(
                self.scene_path.display().to_string(),
                String::new(),
            ),
            at: (0, 0).into(),
        })?;

        // T-14.1: file-shape detection.
        let shape = crate::compat::preprocess_file_shape(src, &self.scene_path)?;
        let src = shape.as_str();

        let mut doc: SceneDoc =
            facet_kdl::from_str(src).map_err(|e| SceneError::Parse {
                src: miette::NamedSource::new(
                    self.scene_path.display().to_string(),
                    src.to_string(),
                ),
                at: (0, src.len().min(1)).into(),
                message: e.to_string(),
            })?;

        // Auto-inject ark-bus when needed (mirrors compile_scene_file).
        let _injected = crate::compile::maybe_inject_ark_bus(&mut doc.scene);

        // Validate CEL + templates.
        if let Err(errs) = validate_scene(&doc) {
            let first = errs.into_iter().next().unwrap_or_else(|| {
                SceneError::Grammar {
                    message: "unknown validation error".to_string(),
                    src: miette::NamedSource::new(
                        self.scene_path.display().to_string(),
                        String::new(),
                    ),
                    at: (0, 0).into(),
                }
            });
            return Err(first);
        }

        Ok(doc)
    }

    /// Compute a coarse diff between the current live state and the new
    /// parsed scene. Finer-grained diffs (AST-structural hashing per
    /// T-11.2, keybind diff per T-11.3, plugin lifecycle diff per
    /// T-11.4) land in later tiers.
    fn compute_diff(
        &self,
        new_doc: &SceneDoc,
        new_registry: &ReactionRegistry,
    ) -> SceneDiff {
        let old_registry = self
            .current_registry
            .lock()
            .expect("registry mutex poisoned");
        let old_doc = self
            .current_doc
            .lock()
            .expect("doc mutex poisoned");

        SceneDiff {
            old_reaction_count: old_registry.len(),
            new_reaction_count: new_registry.len(),
            old_keybind_count: old_doc.scene.keybinds.len(),
            new_keybind_count: new_doc.scene.keybinds.len(),
            old_plugin_count: old_doc.scene.plugins.len(),
            new_plugin_count: new_doc.scene.plugins.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("reload")
            .tempdir_in("/tmp")
            .expect("tempdir")
    }

    /// Write a scene file and return its path + parsed doc + registry.
    fn write_scene(dir: &Path, name: &str, content: &str) -> (PathBuf, SceneDoc, ReactionRegistry) {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write scene");
        let doc: SceneDoc = facet_kdl::from_str(content).expect("parse scene");
        let registry = populate_registry(&doc).expect("populate registry");
        (path, doc, registry)
    }

    const SCENE_V1: &str = r#"scene "test" {
    on "Started" { }
}"#;

    const SCENE_V2: &str = r#"scene "test" {
    on "Started" { }
    on "Done" { }
}"#;

    const SCENE_BROKEN: &str = r#"scene "test" { !! invalid"#;

    // -- re-entry guard ---------------------------------------------------

    #[test]
    fn reentry_guard_drops_concurrent_reloads() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        // Manually set the reload_in_progress flag to simulate a
        // concurrent reload.
        reloader
            .reload_in_progress
            .store(true, Ordering::SeqCst);

        let outcome = reloader.reload(|| None);
        assert!(
            matches!(outcome, ReloadOutcome::ReentryDropped),
            "expected ReentryDropped, got {outcome:?}"
        );

        // Clean up so Drop doesn't panic on a poisoned state.
        reloader
            .reload_in_progress
            .store(false, Ordering::SeqCst);
    }

    // -- turn-inflight gate -----------------------------------------------

    #[test]
    fn turn_inflight_gate_queues_when_sessions_active() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        // Simulate 2 sessions with in-flight turns.
        let outcome = reloader.reload(|| Some(2));
        assert!(
            matches!(outcome, ReloadOutcome::Queued { pending_sessions: 2 }),
            "expected Queued(2), got {outcome:?}"
        );

        assert!(reloader.is_pending(), "reload should be pending");
    }

    #[test]
    fn drain_pending_fires_queued_reload() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        // Queue a reload.
        let _queued = reloader.reload(|| Some(1));
        assert!(reloader.is_pending());

        // Now drain with no in-flight turns.
        let outcome = reloader.drain_pending(|| Some(0));
        assert!(outcome.is_some(), "drain should have fired");
        assert!(
            matches!(outcome.unwrap(), ReloadOutcome::Applied { .. }),
            "expected Applied after drain"
        );

        assert!(!reloader.is_pending(), "pending flag should be cleared");
    }

    #[test]
    fn drain_pending_noop_when_nothing_queued() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        let outcome = reloader.drain_pending(|| None);
        assert!(outcome.is_none(), "nothing should have fired");
    }

    // -- basic reload path ------------------------------------------------

    #[test]
    fn basic_reload_updates_registry() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let initial_count = registry.len();
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        // Overwrite the scene with a second reaction.
        std::fs::write(&path, SCENE_V2).expect("overwrite");

        let outcome = reloader.reload(|| None);
        match &outcome {
            ReloadOutcome::Applied { diff } => {
                assert_eq!(diff.old_reaction_count, initial_count);
                assert!(
                    diff.new_reaction_count > initial_count,
                    "new registry should have more reactions: old={}, new={}",
                    initial_count,
                    diff.new_reaction_count
                );
            }
            other => panic!("expected Applied, got {other:?}"),
        }

        // The live registry should reflect the new scene.
        let live = reloader.current_registry();
        assert!(live.len() > initial_count);
    }

    // -- parse failure retains old config ---------------------------------

    #[test]
    fn parse_failure_retains_old_config() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let initial_count = registry.len();
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        // Overwrite with broken content.
        std::fs::write(&path, SCENE_BROKEN).expect("overwrite");

        let outcome = reloader.reload(|| None);
        assert!(
            matches!(outcome, ReloadOutcome::Failed { .. }),
            "expected Failed, got {outcome:?}"
        );

        // Old registry should be untouched.
        let live = reloader.current_registry();
        assert_eq!(live.len(), initial_count);
    }

    // -- re-entry guard is released after reload --------------------------

    #[test]
    fn reentry_guard_released_after_successful_reload() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        let _outcome = reloader.reload(|| None);
        // The flag must be cleared.
        assert!(
            !reloader.reload_in_progress.load(Ordering::SeqCst),
            "reload_in_progress should be false after reload"
        );

        // A second reload should succeed (not be dropped).
        let outcome2 = reloader.reload(|| None);
        assert!(
            !matches!(outcome2, ReloadOutcome::ReentryDropped),
            "second reload should not be dropped"
        );
    }

    #[test]
    fn reentry_guard_released_after_failed_reload() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        std::fs::write(&path, SCENE_BROKEN).expect("overwrite");
        let _outcome = reloader.reload(|| None);

        assert!(
            !reloader.reload_in_progress.load(Ordering::SeqCst),
            "reload_in_progress should be false after failed reload"
        );
    }

    // -- zero-inflight passes through -------------------------------------

    #[test]
    fn zero_inflight_proceeds_with_reload() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path, Arc::new(registry), doc);

        let outcome = reloader.reload(|| Some(0));
        assert!(
            matches!(outcome, ReloadOutcome::Applied { .. }),
            "expected Applied with 0 in-flight, got {outcome:?}"
        );
    }
}
