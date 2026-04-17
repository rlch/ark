//! Reconciler — drives zellij toward the scene-declared desired state
//! via `override-layout` (R9 / T-041..T-046).
//!
//! # Model
//!
//! The reconciler owns a compiled [`CompiledScene`] and the current Rhai
//! evaluation scope. On a reconciliation pass it walks the scene's layout
//! subtree, evaluates every `when="<Rhai>"` predicate against the latest
//! scope, elides subtrees whose predicates returned `false`, lowers the
//! resulting [`LayoutNode`] into zellij KDL via
//! [`crate::compile::compile_layout_kdl`], writes the rendered KDL to
//! disk (T-040), and issues the matching `zellij action override-layout`
//! command through a [`LayoutApplier`] adapter.
//!
//! # Drift tolerance (T-044, R9.10)
//!
//! User-initiated state changes (e.g. manually closing a pane) are
//! tolerated — the reconciler only forces convergence on:
//!
//! 1. `when=` predicate transitions (any predicate changed truth value
//!    since the last pass).
//! 2. Mode switches (`use_mode "name"` op — invokes
//!    [`Reconciler::reconcile_mode`]).
//! 3. Explicit forced reconciles (hot-reload, T-132).
//!
//! Between those triggers, the reconciler does not run. That means users
//! can freely `close-pane` inside zellij without the reconciler reviving
//! the pane on the next tick.
//!
//! # Debounce (T-043, R9.7 / R9.8)
//!
//! Rapid back-to-back reconciliation requests are coalesced to a single
//! pass with a 200 ms tail window. The [`Debouncer`] helper records the
//! latest request timestamp and callers check
//! [`Debouncer::should_fire_now`] inside their own event loops.
//!
//! # LayoutApplier
//!
//! The reconciler is deliberately decoupled from `ark_mux_zellij::ZellijMux`
//! — tests substitute a [`RecordingApplier`] that captures invocations
//! without spawning subprocesses. The production impl lives in
//! [`ZellijCommandApplier`] and drives zellij through the mux crate's
//! [`ark_mux_zellij::CommandExecutor`] trait.

// The scene error enum is intentionally heavy for miette diagnostics.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use kdl::KdlDocument;
use rhai::Scope as RhaiScope;
use tokio::sync::Mutex;

use crate::ast::layout::{ColNode, LayoutChild, PaneNode, RowNode, TabNode};
use crate::ast::{LayoutNode, SceneBodyNode, SceneNode};
use crate::compile::layout::{compile_layout_kdl, write_layout_artifact, write_layout_artifact_in};
use crate::compile::modes::{compile_modes, write_mode_artifacts, write_mode_artifacts_in};
use crate::compile::CompiledScene;
use crate::error::SceneError;
use crate::rhai::{eval_bool, Engine, Program};
use crate::view::ViewRegistry;

/// Default debounce window for coalescing rapid reconciliation requests.
pub const DEFAULT_DEBOUNCE_MS: u64 = 200;

// ---------------------------------------------------------------------------
// LayoutApplier — the ZellijMux-facing seam (T-042 / T-046)
// ---------------------------------------------------------------------------

/// Flags passed to `zellij action override-layout`. Mirrors the zellij
/// CLI one-for-one so callers can reason about the on-the-wire invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverrideLayoutFlags {
    /// `--retain-existing-terminal-panes`.
    pub retain_existing_terminal_panes: bool,
    /// `--retain-existing-plugin-panes`.
    pub retain_existing_plugin_panes: bool,
    /// `--apply-only-to-active-tab` (mode switches only).
    pub apply_only_to_active_tab: bool,
}

impl OverrideLayoutFlags {
    /// Full-layout reconcile: retain existing panes, no tab restriction.
    pub fn full_reconcile() -> Self {
        Self {
            retain_existing_terminal_panes: true,
            retain_existing_plugin_panes: true,
            apply_only_to_active_tab: false,
        }
    }

    /// Mode switch: retain existing panes, apply only to the active tab.
    pub fn mode_switch() -> Self {
        Self {
            retain_existing_terminal_panes: true,
            retain_existing_plugin_panes: true,
            apply_only_to_active_tab: true,
        }
    }

    /// Render into a `zellij action override-layout …` argv (minus the
    /// leading `zellij action override-layout <path>`).
    pub fn to_cli_flags(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.retain_existing_terminal_panes {
            out.push("--retain-existing-terminal-panes".to_string());
        }
        if self.retain_existing_plugin_panes {
            out.push("--retain-existing-plugin-panes".to_string());
        }
        if self.apply_only_to_active_tab {
            out.push("--apply-only-to-active-tab".to_string());
        }
        out
    }
}

/// Seam over the zellij command invocation. Production uses
/// [`ZellijCommandApplier`]; tests use [`RecordingApplier`].
#[async_trait]
pub trait LayoutApplier: Send + Sync {
    /// Invoke `zellij action override-layout <path> <flags…>` against the
    /// current session.
    async fn override_layout(
        &self,
        layout_path: &Path,
        flags: OverrideLayoutFlags,
    ) -> Result<(), SceneError>;
}

/// In-memory recording applier for tests — captures every invocation as
/// a `(path, flags)` tuple for later assertion.
#[derive(Debug, Default)]
pub struct RecordingApplier {
    /// Ordered record of `(layout_path, flags)` tuples seen by this
    /// applier. Wrapped in a tokio `Mutex` so multiple tasks can share
    /// one applier instance in async tests.
    pub calls: Mutex<Vec<(PathBuf, OverrideLayoutFlags)>>,
}

impl RecordingApplier {
    /// Construct an empty recording applier.
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Snapshot every call recorded so far.
    pub async fn snapshot(&self) -> Vec<(PathBuf, OverrideLayoutFlags)> {
        self.calls.lock().await.clone()
    }
}

#[async_trait]
impl LayoutApplier for RecordingApplier {
    async fn override_layout(
        &self,
        layout_path: &Path,
        flags: OverrideLayoutFlags,
    ) -> Result<(), SceneError> {
        self.calls
            .lock()
            .await
            .push((layout_path.to_path_buf(), flags));
        Ok(())
    }
}

/// Production applier that shells out via
/// [`ark_mux_zellij::CommandExecutor`] (either the real `tokio::process`
/// executor or a stubbed one in higher-level tests).
pub struct ZellijCommandApplier {
    executor: Arc<dyn ark_mux_zellij::CommandExecutor>,
    session: Option<String>,
}

impl ZellijCommandApplier {
    /// Construct with an explicit executor. Use [`Self::new`] for the
    /// standard tokio::process-backed executor.
    pub fn with_executor(executor: Arc<dyn ark_mux_zellij::CommandExecutor>) -> Self {
        Self {
            executor,
            session: None,
        }
    }

    /// Convenience constructor that pairs with the standard
    /// [`ark_mux_zellij::RealExecutor`].
    pub fn new() -> Self {
        Self::with_executor(Arc::new(ark_mux_zellij::RealExecutor))
    }

    /// Set the target zellij session name (`zellij --session <name> …`).
    /// `None` = run against the default / current session.
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }
}

impl Default for ZellijCommandApplier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LayoutApplier for ZellijCommandApplier {
    async fn override_layout(
        &self,
        layout_path: &Path,
        flags: OverrideLayoutFlags,
    ) -> Result<(), SceneError> {
        let path_str = layout_path.display().to_string();
        let cli_flags = flags.to_cli_flags();

        let mut args: Vec<&str> = Vec::new();
        if let Some(session) = &self.session {
            args.push("--session");
            args.push(session);
        }
        args.push("action");
        args.push("override-layout");
        args.push(&path_str);
        for f in &cli_flags {
            args.push(f);
        }

        let output = self
            .executor
            .run("zellij", &args)
            .await
            .map_err(|e| SceneError::OpFailed {
                op: "override-layout".to_string(),
                message: format!("zellij action override-layout failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SceneError::OpFailed {
                op: "override-layout".to_string(),
                message: format!(
                    "zellij action override-layout exited non-zero: {}",
                    stderr.trim()
                ),
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Debouncer (T-043)
// ---------------------------------------------------------------------------

/// Combined state for the [`Debouncer`], held under a single `Mutex` to
/// eliminate the ABBA lock-ordering hazard that would arise from holding
/// two separate mutexes (`dirty` and `last_mark`) across method calls.
#[derive(Debug, Default)]
struct DebouncerState {
    /// Whether at least one reconciliation request has been recorded
    /// since the last fire (or since construction).
    dirty: bool,
    /// Timestamp of the most recent [`Debouncer::mark_dirty`] call.
    last_mark: Option<Instant>,
}

/// 200 ms debounce coalescer.
///
/// Callers call [`Debouncer::mark_dirty`] every time a reconciliation is
/// requested. When they're ready to fire, they call
/// [`Debouncer::should_fire_now`] — it returns `true` exactly once per
/// quiescence window (≥ `window` has elapsed since the last `mark_dirty`
/// and a dirty flag is set).
#[derive(Debug)]
pub struct Debouncer {
    window: Duration,
    state: Mutex<DebouncerState>,
}

impl Debouncer {
    /// Construct a new debouncer with the given tail window.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            state: Mutex::new(DebouncerState::default()),
        }
    }

    /// Default 200 ms window (R9.7 / R9.8).
    pub fn default_window() -> Self {
        Self::new(Duration::from_millis(DEFAULT_DEBOUNCE_MS))
    }

    /// Record a reconciliation request. Callers use this on every
    /// predicate-input-change / file-edit signal.
    pub async fn mark_dirty(&self) {
        let mut state = self.state.lock().await;
        state.last_mark = Some(Instant::now());
        state.dirty = true;
    }

    /// Should the reconciliation fire now? Fires exactly once per
    /// quiescent window, consuming the dirty flag when it does.
    pub async fn should_fire_now(&self) -> bool {
        let mut state = self.state.lock().await;
        if !state.dirty {
            return false;
        }
        let Some(t) = state.last_mark else {
            return false;
        };
        if t.elapsed() >= self.window {
            state.dirty = false;
            true
        } else {
            false
        }
    }

    /// Is a reconciliation request pending (dirty flag set)?
    pub async fn is_dirty(&self) -> bool {
        self.state.lock().await.dirty
    }

    /// Manually clear the dirty flag — e.g. after a forced reconcile.
    pub async fn clear(&self) {
        self.state.lock().await.dirty = false;
    }
}

// ---------------------------------------------------------------------------
// Reconciler (T-041 / T-042 / T-046)
// ---------------------------------------------------------------------------

/// Driving type for the scene reconciliation loop.
pub struct Reconciler {
    /// Fully compiled scene (AST + pre-compiled Rhai surfaces).
    pub compiled: CompiledScene,
    /// View registry used for pane alias resolution during layout lowering.
    pub registry: ViewRegistry,
    /// Shared Rhai engine — owned by the reconciler because predicate
    /// evaluation runs inside reconciliation passes.
    pub engine: Engine,
    /// Layout applier seam — real `zellij action` in production,
    /// [`RecordingApplier`] in tests.
    pub applier: Arc<dyn LayoutApplier>,
    /// Cached predicate-truth map from the most recent pass; a new pass
    /// that produces different values for any entry forces convergence
    /// (R9.10 drift tolerance).
    last_predicate_truth: HashMap<String, bool>,
    /// 200 ms debouncer.
    pub debouncer: Debouncer,
    /// Pre-rendered mode layouts (`mode "<name>" { … }` → path on disk).
    /// Populated by [`Reconciler::render_modes`].
    pub mode_paths: HashMap<String, PathBuf>,
    /// Override for the layouts output directory. `None` = use the
    /// default `${XDG_RUNTIME_DIR}/ark/layouts` path. Tests use this to
    /// avoid mutating process-global env state.
    pub layouts_dir_override: Option<PathBuf>,
}

impl Reconciler {
    /// Construct a fresh reconciler.
    pub fn new(
        compiled: CompiledScene,
        registry: ViewRegistry,
        applier: Arc<dyn LayoutApplier>,
    ) -> Self {
        Self {
            compiled,
            registry,
            engine: Engine::new(),
            applier,
            last_predicate_truth: HashMap::new(),
            debouncer: Debouncer::default_window(),
            mode_paths: HashMap::new(),
            layouts_dir_override: None,
        }
    }

    /// Override the layouts output directory. Primarily for tests.
    pub fn with_layouts_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.layouts_dir_override = Some(dir.into());
        self
    }

    fn write_layout(&self, doc: &KdlDocument) -> Result<PathBuf, std::io::Error> {
        if let Some(dir) = &self.layouts_dir_override {
            write_layout_artifact_in(doc, &self.compiled.ir.id, dir)
        } else {
            write_layout_artifact(doc, &self.compiled.ir.id)
        }
    }

    fn write_modes(
        &self,
        docs: &std::collections::BTreeMap<String, KdlDocument>,
    ) -> Result<std::collections::BTreeMap<String, PathBuf>, std::io::Error> {
        if let Some(dir) = &self.layouts_dir_override {
            write_mode_artifacts_in(docs, &self.compiled.ir.id, dir)
        } else {
            write_mode_artifacts(docs, &self.compiled.ir.id)
        }
    }

    /// Look up a compiled predicate by its AST path.
    fn find_predicate(&self, path: &str) -> Option<&Program> {
        self.compiled
            .predicates
            .iter()
            .find_map(|(p, prog)| (p == path).then_some(prog))
    }

    // -----------------------------------------------------------------
    // reconcile() — full pass (T-042)
    // -----------------------------------------------------------------

    /// Run a full reconciliation pass: re-eval every `when=` predicate
    /// against `scope`, render the filtered layout to KDL, write the
    /// artifact, and invoke `zellij action override-layout`.
    pub async fn reconcile(
        &mut self,
        scope: &mut RhaiScope<'static>,
    ) -> Result<ReconcileOutcome, SceneError> {
        // Render the filtered layout using the new scope.
        let (filtered, truth_map) = self.render_desired_layout(scope)?;
        let doc = compile_layout_kdl(&filtered, &self.registry)?;

        let changed_truth = truth_map != self.last_predicate_truth;
        self.last_predicate_truth = truth_map;

        let path = self
            .write_layout(&doc)
            .map_err(|e| SceneError::OpFailed {
                op: "write-layout".to_string(),
                message: format!("failed to write layout artifact: {e}"),
            })?;

        self.applier
            .override_layout(&path, OverrideLayoutFlags::full_reconcile())
            .await?;

        self.debouncer.clear().await;
        Ok(ReconcileOutcome {
            layout_path: path,
            predicates_changed: changed_truth,
        })
    }

    /// Render the filtered [`LayoutNode`] + predicate truth map without
    /// writing to disk or invoking zellij. Exposed for testing and for
    /// `ark scene dry-run` later.
    pub fn render_desired_layout(
        &self,
        scope: &mut RhaiScope<'static>,
    ) -> Result<(LayoutNode, HashMap<String, bool>), SceneError> {
        let mut truth = HashMap::new();
        let filtered = self.filter_layout(&self.compiled.ir.scene, scope, &mut truth)?;
        Ok((filtered, truth))
    }

    /// Render the filtered layout to zellij KDL (no disk I/O, no zellij
    /// invocation). Exposed for `ark scene render` / dry-run tests.
    #[allow(clippy::result_large_err)]
    pub fn render_desired_layout_kdl(
        &self,
        scope: &mut RhaiScope<'static>,
    ) -> Result<KdlDocument, SceneError> {
        let (filtered, _) = self.render_desired_layout(scope)?;
        compile_layout_kdl(&filtered, &self.registry)
    }

    // -----------------------------------------------------------------
    // reconcile_mode() — mode switch (T-046)
    // -----------------------------------------------------------------

    /// Pre-render every declared `mode "<name>" { … }` block to disk.
    /// Populates [`Self::mode_paths`]. Called once at scene bring-up.
    pub fn render_modes(&mut self) -> Result<(), SceneError> {
        let docs = compile_modes(&self.compiled.ir, &self.registry)?;
        let paths = self.write_modes(&docs).map_err(|e| SceneError::OpFailed {
            op: "write-modes".to_string(),
            message: format!("failed to write mode artifacts: {e}"),
        })?;
        self.mode_paths = paths.into_iter().collect();
        Ok(())
    }

    /// Switch to the named mode via `zellij action override-layout
    /// --apply-only-to-active-tab`. `"default"` is special-cased (R7.11):
    /// it falls back to [`Self::reconcile`] against the current scope so
    /// the base layout is restored.
    pub async fn reconcile_mode(
        &mut self,
        mode_name: &str,
        scope: &mut RhaiScope<'static>,
    ) -> Result<ReconcileOutcome, SceneError> {
        if mode_name == "default" {
            return self.reconcile(scope).await;
        }

        // Lazy-render modes the first time we switch to one.
        if self.mode_paths.is_empty() {
            self.render_modes()?;
        }

        let path = self
            .mode_paths
            .get(mode_name)
            .cloned()
            .ok_or_else(|| SceneError::OpFailed {
                op: "use_mode".to_string(),
                message: format!("unknown mode `{mode_name}`"),
            })?;

        self.applier
            .override_layout(&path, OverrideLayoutFlags::mode_switch())
            .await?;

        Ok(ReconcileOutcome {
            layout_path: path,
            predicates_changed: false,
        })
    }

    // -----------------------------------------------------------------
    // Layout filtering — evaluate `when=` predicates (R9.1 / R9.10)
    // -----------------------------------------------------------------

    fn filter_layout(
        &self,
        scene: &SceneNode,
        scope: &mut RhaiScope<'static>,
        truth: &mut HashMap<String, bool>,
    ) -> Result<LayoutNode, SceneError> {
        let mut out = LayoutNode { tabs: Vec::new() };
        for (i, node) in scene.body.iter().enumerate() {
            let base = format!("scene.body[{i}]");
            if let SceneBodyNode::Layout(layout) = node {
                for (j, tab) in layout.tabs.iter().enumerate() {
                    let path = format!("{base}.layout.tabs[{j}]");
                    if let Some(new_tab) = self.filter_tab(tab, &path, scope, truth)? {
                        out.tabs.push(new_tab);
                    }
                }
            }
        }
        Ok(out)
    }

    fn filter_tab(
        &self,
        tab: &TabNode,
        path: &str,
        scope: &mut RhaiScope<'static>,
        truth: &mut HashMap<String, bool>,
    ) -> Result<Option<TabNode>, SceneError> {
        let include = self.eval_when(&tab.when, &format!("{path}.when"), scope, truth)?;
        if !include {
            return Ok(None);
        }
        let mut body = Vec::new();
        for (i, child) in tab.body.iter().enumerate() {
            let child_path = format!("{path}.body[{i}]");
            if let Some(c) = self.filter_child(child, &child_path, scope, truth)? {
                body.push(c);
            }
        }
        Ok(Some(TabNode {
            handle: tab.handle.clone(),
            cwd: tab.cwd.clone(),
            name: tab.name.clone(),
            focus: tab.focus.clone(),
            when: None,
            body,
        }))
    }

    fn filter_child(
        &self,
        child: &LayoutChild,
        path: &str,
        scope: &mut RhaiScope<'static>,
        truth: &mut HashMap<String, bool>,
    ) -> Result<Option<LayoutChild>, SceneError> {
        Ok(match child {
            LayoutChild::Row(row) => {
                let include = self.eval_when(&row.when, &format!("{path}.when"), scope, truth)?;
                if !include {
                    return Ok(None);
                }
                let mut body = Vec::new();
                for (i, c) in row.body.iter().enumerate() {
                    let p = format!("{path}.body[{i}]");
                    if let Some(c) = self.filter_child(c, &p, scope, truth)? {
                        body.push(c);
                    }
                }
                Some(LayoutChild::Row(RowNode {
                    body,
                    when: None,
                    span: row.span,
                    cells: row.cells,
                    min: row.min,
                    max: row.max,
                }))
            }
            LayoutChild::Col(col) => {
                let include = self.eval_when(&col.when, &format!("{path}.when"), scope, truth)?;
                if !include {
                    return Ok(None);
                }
                let mut body = Vec::new();
                for (i, c) in col.body.iter().enumerate() {
                    let p = format!("{path}.body[{i}]");
                    if let Some(c) = self.filter_child(c, &p, scope, truth)? {
                        body.push(c);
                    }
                }
                Some(LayoutChild::Col(ColNode {
                    body,
                    when: None,
                    span: col.span,
                    cells: col.cells,
                    min: col.min,
                    max: col.max,
                }))
            }
            LayoutChild::Pane(pane) => {
                let include = self.eval_when(&pane.when, &format!("{path}.when"), scope, truth)?;
                if !include {
                    return Ok(None);
                }
                Some(LayoutChild::Pane(PaneNode {
                    handle: pane.handle.clone(),
                    span: pane.span,
                    cells: pane.cells,
                    min: pane.min,
                    max: pane.max,
                    when: None,
                    overlay: pane.overlay.clone(),
                    view: pane.view.clone(),
                }))
            }
        })
    }

    /// Evaluate a predicate; absent predicate defaults to `true`
    /// (include). Records the truth value in `truth` for drift detection.
    fn eval_when(
        &self,
        when: &Option<String>,
        path: &str,
        scope: &mut RhaiScope<'static>,
        truth: &mut HashMap<String, bool>,
    ) -> Result<bool, SceneError> {
        if when.is_none() {
            return Ok(true);
        }
        let program = self
            .find_predicate(path)
            .ok_or_else(|| SceneError::RhaiEval {
                message: format!("predicate at `{path}` not found in compiled scene"),
            })?;
        let value = eval_bool(&self.engine, program, scope)?;
        truth.insert(path.to_string(), value);
        Ok(value)
    }

    // -----------------------------------------------------------------
    // Introspection for tests / CLI
    // -----------------------------------------------------------------

    /// Read-only view of the most recent predicate-truth snapshot.
    pub fn last_truth_snapshot(&self) -> &HashMap<String, bool> {
        &self.last_predicate_truth
    }
}

/// Summary of what a single reconciliation pass did.
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    /// Path to the rendered layout KDL that was handed to zellij.
    pub layout_path: PathBuf,
    /// Whether any predicate flipped truth value compared to the previous
    /// pass. A `false` here means the scene converged without any
    /// `when=` transition — still emits the override-layout to ensure
    /// the desired state is materialised, but callers can treat this as
    /// a quiet reconciliation for telemetry.
    pub predicates_changed: bool,
}

// ---------------------------------------------------------------------------
// Unit tests — pure Rhai-less paths
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_flags_render_full_reconcile() {
        let flags = OverrideLayoutFlags::full_reconcile();
        let cli = flags.to_cli_flags();
        assert!(cli.contains(&"--retain-existing-terminal-panes".to_string()));
        assert!(cli.contains(&"--retain-existing-plugin-panes".to_string()));
        assert!(!cli.contains(&"--apply-only-to-active-tab".to_string()));
    }

    #[test]
    fn override_flags_render_mode_switch() {
        let flags = OverrideLayoutFlags::mode_switch();
        let cli = flags.to_cli_flags();
        assert!(cli.contains(&"--apply-only-to-active-tab".to_string()));
        assert!(cli.contains(&"--retain-existing-terminal-panes".to_string()));
    }

    #[tokio::test]
    async fn debouncer_fires_after_window() {
        let deb = Debouncer::new(Duration::from_millis(25));
        assert!(!deb.should_fire_now().await);
        deb.mark_dirty().await;
        assert!(!deb.should_fire_now().await);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(deb.should_fire_now().await);
        // Second check in same quiescent window — already consumed.
        assert!(!deb.should_fire_now().await);
    }

    #[tokio::test]
    async fn debouncer_coalesces_rapid_marks() {
        let deb = Debouncer::new(Duration::from_millis(25));
        for _ in 0..10 {
            deb.mark_dirty().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Still within the window because the last mark reset the clock.
        assert!(!deb.should_fire_now().await);
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Exactly one fire event.
        assert!(deb.should_fire_now().await);
        assert!(!deb.should_fire_now().await);
    }

    #[tokio::test]
    async fn recording_applier_captures_calls() {
        let applier = RecordingApplier::new();
        applier
            .override_layout(Path::new("/tmp/x.kdl"), OverrideLayoutFlags::mode_switch())
            .await
            .unwrap();
        let calls = applier.snapshot().await;
        assert_eq!(calls.len(), 1);
        assert!(calls[0].1.apply_only_to_active_tab);
    }
}
