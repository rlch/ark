//! Scene reload logic (T-127) + re-entry guard (T-129) + hot-reload
//! infrastructure (T-128, T-130..T-134).
//!
//! [`reload_scene`] re-reads the current scene file from disk, runs the
//! full shape-detect → parse → compose → compile pipeline, and returns a
//! [`ReloadResult`] describing what changed. On any parse/compile
//! failure the old scene is left untouched and the error is surfaced in
//! [`ReloadStatus::Failed`].
//!
//! The [`ReloadGuard`] is a single-slot `AtomicBool` lock that prevents
//! concurrent `reload_scene` invocations. A second reload while one is
//! already in progress is silently dropped with a `tracing::debug!` log
//! line (T-129).
//!
//! ## Additional Tier-14 facilities
//!
//! - [`ReloadQueue`] (T-128): Turn-inflight gate — queues reloads while
//!   an ACP turn is active, applies when the turn completes.
//! - [`diff_reactions`] / [`ReactionDiff`] (T-130): Content-hash-based
//!   reaction diff for detecting subscription-set changes.
//! - [`diff_keybinds`] / [`KeybindDiff`] (T-131): Chord-keyed keybind
//!   diff for detecting added/removed/changed binds.
//! - [`trigger_reconcile`] (T-132): Stub wiring that invokes the full
//!   reconciler after a successful reload.
//! - [`FileWatcherConfig`] / [`should_ignore_path`] (T-133): Opt-in
//!   file watcher configuration with debounce and ignore suffixes.
//! - [`reload_telemetry_payload`] (T-134): Converts a [`ReloadResult`]
//!   into a structured event payload for telemetry.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ast::ops::OpNode;
use crate::ast::{BindNode, OnNode};
use crate::compile::{CompiledScene, compile_scene};
use crate::compose::compose_scene;
use crate::error::SceneError;
use crate::parse::parse_scene;
use crate::rhai::Engine;
use crate::shape::detect_and_normalize;

/// Outcome status of a reload attempt.
#[derive(Debug)]
pub enum ReloadStatus {
    /// Full reload succeeded — new scene is ready to apply.
    Ok,
    /// Reload partially succeeded (e.g. layout compiled but some
    /// reactions had warnings). The `String` carries a human-readable
    /// summary of the partial issues.
    Partial(String),
    /// Reload failed entirely — old scene should be retained. The
    /// `String` carries the rendered error message.
    Failed(String),
}

/// Summary of a scene reload attempt.
#[derive(Debug)]
pub struct ReloadResult {
    /// Whether the reload succeeded, partially succeeded, or failed.
    pub status: ReloadStatus,
    /// Wall-clock duration of the reload pipeline in milliseconds.
    pub duration_ms: u64,
    /// Number of reactions added relative to the previous compiled scene.
    pub reactions_added: usize,
    /// Number of reactions removed relative to the previous compiled scene.
    pub reactions_removed: usize,
    /// Number of keybinds that changed (added + removed + modified).
    pub keybinds_changed: usize,
}

/// Re-entry guard: prevents concurrent `reload_scene` execution (T-129).
///
/// Uses a single-slot `AtomicBool`. A concurrent reload while one is
/// already active returns `None` from [`try_acquire`] and logs a
/// `tracing::debug!` message.
pub struct ReloadGuard {
    /// `true` when a reload is in progress.
    in_progress: AtomicBool,
}

impl ReloadGuard {
    /// Create a new guard in the idle state.
    pub fn new() -> Self {
        Self {
            in_progress: AtomicBool::new(false),
        }
    }

    /// Try to acquire the reload lock.
    ///
    /// Returns `Some(ReloadLock)` on success; `None` if another reload is
    /// already in progress (with a debug log line).
    pub fn try_acquire(&self) -> Option<ReloadLock<'_>> {
        if self
            .in_progress
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            Some(ReloadLock { guard: self })
        } else {
            tracing::debug!("reload_scene dropped: another reload already in progress");
            None
        }
    }

    /// Check whether a reload is currently in progress (test helper).
    pub fn is_active(&self) -> bool {
        self.in_progress.load(Ordering::SeqCst)
    }
}

impl Default for ReloadGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII lock returned by [`ReloadGuard::try_acquire`]. Releases the
/// guard when dropped.
pub struct ReloadLock<'a> {
    guard: &'a ReloadGuard,
}

impl Drop for ReloadLock<'_> {
    fn drop(&mut self) {
        self.guard.in_progress.store(false, Ordering::SeqCst);
    }
}

/// Execute a scene reload (T-127).
///
/// Re-reads `scene_path` from disk and runs the full pipeline:
/// shape-detect → parse → compose → compile. Returns `None` if the
/// guard could not be acquired (another reload in progress), otherwise
/// returns `Some(Ok((compiled, result)))` with the new [`CompiledScene`]
/// and a [`ReloadResult`] delta summary, or `Some(Err(..))` on
/// parse/compile failure so callers can keep the old scene while still
/// surfacing the error.
///
/// The `prev` parameter provides the previous compiled scene for delta
/// computation. Pass `None` on first load.
pub fn reload_scene(
    scene_path: &Path,
    guard: &ReloadGuard,
    engine: &Engine,
    prev: Option<&CompiledScene>,
) -> Option<Result<(CompiledScene, ReloadResult), SceneError>> {
    let _lock = guard.try_acquire()?;
    let start = std::time::Instant::now();

    // ── Step 1: read file ──────────────────────────────────────────
    let content = match std::fs::read_to_string(scene_path) {
        Ok(c) => c,
        Err(e) => {
            return Some(Err(SceneError::Parse {
                message: format!("failed to read {}: {e}", scene_path.display()),
                src: miette::NamedSource::new(scene_path.display().to_string(), String::new()),
                span: (0, 0).into(),
            }));
        }
    };

    // ── Step 2: shape detect + normalize ───────────────────────────
    let normalized = match detect_and_normalize(&content, scene_path) {
        Ok(n) => n,
        Err(e) => return Some(Err(e)),
    };

    // ── Step 3: parse ──────────────────────────────────────────────
    let ir = match parse_scene(normalized, scene_path) {
        Ok(ir) => ir,
        Err(e) => return Some(Err(e)),
    };

    // ── Step 4: compose (resolve includes) ─────────────────────────
    let ir = match compose_scene(ir) {
        Ok(ir) => ir,
        Err(e) => return Some(Err(e)),
    };

    // ── Step 5: compile ────────────────────────────────────────────
    let compiled = match compile_scene(engine, ir) {
        Ok(c) => c,
        Err(e) => return Some(Err(e)),
    };

    // ── Step 6: diff against previous scene ────────────────────────
    let (reactions_added, reactions_removed, keybinds_changed) = if let Some(prev) = prev {
        compute_delta(prev, &compiled)
    } else {
        // First load — everything is "new".
        let new_reactions = extract_on_nodes(&compiled).len();
        let new_binds = extract_bind_nodes(&compiled).len();
        (new_reactions, 0, new_binds)
    };

    let duration_ms = start.elapsed().as_millis() as u64;

    Some(Ok((
        compiled,
        ReloadResult {
            status: ReloadStatus::Ok,
            duration_ms,
            reactions_added,
            reactions_removed,
            keybinds_changed,
        },
    )))
}

// ── Delta helpers ──────────────────────────────────────────────────────

use crate::ast::SceneBodyNode;

/// Extract all `OnNode` reactions from a compiled scene body.
fn extract_on_nodes(scene: &CompiledScene) -> Vec<&OnNode> {
    scene
        .ir
        .scene
        .body
        .iter()
        .filter_map(|n| match n {
            SceneBodyNode::On(on) => Some(on),
            _ => None,
        })
        .collect()
}

/// Extract all `BindNode` keybinds from a compiled scene body.
fn extract_bind_nodes(scene: &CompiledScene) -> Vec<&BindNode> {
    scene
        .ir
        .scene
        .body
        .iter()
        .filter_map(|n| match n {
            SceneBodyNode::Bind(b) => Some(b),
            _ => None,
        })
        .collect()
}

/// Compute delta between a previous and new compiled scene using
/// structural diffs ([`diff_reactions`] / [`diff_keybinds`]).
///
/// Returns `(reactions_added, reactions_removed, keybinds_changed)`.
fn compute_delta(prev: &CompiledScene, next: &CompiledScene) -> (usize, usize, usize) {
    let prev_reactions: Vec<&OnNode> = extract_on_nodes(prev);
    let next_reactions: Vec<&OnNode> = extract_on_nodes(next);
    let prev_binds: Vec<&BindNode> = extract_bind_nodes(prev);
    let next_binds: Vec<&BindNode> = extract_bind_nodes(next);

    // Collect owned copies for the diff functions that expect slices of
    // owned nodes. Clone is cheap — these are small AST fragments.
    let prev_on: Vec<OnNode> = prev_reactions.into_iter().cloned().collect();
    let next_on: Vec<OnNode> = next_reactions.into_iter().cloned().collect();
    let prev_kb: Vec<BindNode> = prev_binds.into_iter().cloned().collect();
    let next_kb: Vec<BindNode> = next_binds.into_iter().cloned().collect();

    let rdiff = diff_reactions(&prev_on, &next_on);
    let kdiff = diff_keybinds(&prev_kb, &next_kb);

    let reactions_added = rdiff.added.len();
    let reactions_removed = rdiff.removed.len();
    let keybinds_changed = kdiff.added.len() + kdiff.removed.len() + kdiff.changed.len();

    (reactions_added, reactions_removed, keybinds_changed)
}

// ---------------------------------------------------------------------------
// T-128: ReloadQueue (turn-inflight gate)
// ---------------------------------------------------------------------------

/// Atomic gate that queues a reload when ACP turns are in-flight.
///
/// When an ACP turn is active, callers invoke [`ReloadQueue::queue`]
/// instead of applying the reload immediately. When the turn completes,
/// the turn-completion path calls [`ReloadQueue::take_pending`] — if it
/// returns `true`, the caller applies the queued reload.
///
/// The gate is lock-free (single `AtomicBool`) because at most one
/// reload can be pending at a time — newer file-change events simply
/// re-set the flag.
pub struct ReloadQueue {
    /// `true` = a reload is pending and should be applied when the
    /// current ACP turn completes.
    pending: AtomicBool,
}

impl ReloadQueue {
    /// Construct a new queue with no pending reload.
    pub fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    /// Mark a reload as pending. If a reload is already pending, the
    /// call is idempotent (the newest scene-file version wins either
    /// way, since the reload reads from disk at apply time).
    pub fn queue(&self) {
        self.pending.store(true, Ordering::SeqCst);
    }

    /// Atomically consume the pending flag. Returns `true` if a reload
    /// was pending (caller should apply it), `false` otherwise.
    pub fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }

    /// Check whether a reload is pending without consuming the flag.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
}

impl Default for ReloadQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// T-130: Subscription-set diff (reactions)
// ---------------------------------------------------------------------------

/// Result of diffing two reaction sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionDiff {
    /// Indices into the *new* slice for reactions that were added.
    pub added: Vec<usize>,
    /// Indices into the *old* slice for reactions that were removed.
    pub removed: Vec<usize>,
}

/// Hash a single [`OpNode`] by variant discriminant + key fields,
/// avoiding `Debug`-based formatting which is fragile across refactors.
fn hash_op_node(op: &OpNode, h: &mut impl Hasher) {
    std::mem::discriminant(op).hash(h);
    match op {
        OpNode::Focus(o) => {
            o.handle.hash(h);
            o.when.hash(h);
        }
        OpNode::Close(o) => {
            o.handle.hash(h);
            o.when.hash(h);
        }
        OpNode::Rename(o) => {
            o.handle.hash(h);
            o.to.hash(h);
            o.when.hash(h);
        }
        OpNode::Resize(o) => {
            o.handle.hash(h);
            o.direction.hash(h);
            o.by.hash(h);
            o.when.hash(h);
        }
        OpNode::Move(o) => {
            o.handle.hash(h);
            o.to.hash(h);
            o.when.hash(h);
        }
        OpNode::Pin(o) => {
            o.handle.hash(h);
            o.when.hash(h);
        }
        OpNode::Unpin(o) => {
            o.handle.hash(h);
            o.when.hash(h);
        }
        OpNode::Spawn(o) => {
            o.handle.hash(h);
            o.when.hash(h);
        }
        OpNode::NewTab(o) => {
            o.handle.hash(h);
            o.name.hash(h);
            o.cwd.hash(h);
            o.when.hash(h);
        }
        OpNode::UseMode(o) => {
            o.mode.hash(h);
            o.when.hash(h);
        }
        OpNode::Pipe(o) => {
            o.from.hash(h);
            o.to.hash(h);
            o.payload.hash(h);
            o.when.hash(h);
        }
        OpNode::Emit(o) => {
            o.event_name.hash(h);
            o.when.hash(h);
        }
        OpNode::SetStatus(o) => {
            o.text.hash(h);
            o.severity.hash(h);
            o.ttl_ms.hash(h);
            o.when.hash(h);
        }
        OpNode::Exec(o) => {
            o.script.hash(h);
            o.shell.hash(h);
            o.timeout_ms.hash(h);
            o.when.hash(h);
        }
        OpNode::ReloadScene(o) => {
            o.when.hash(h);
        }
        OpNode::Unknown { verb, .. } => {
            verb.hash(h);
        }
    }
}

/// Hash a single [`OnNode`] to a stable `u64` for diff detection.
///
/// The hash covers the selector (kind + field patterns), the `when`
/// predicate source, and the op list (field-by-field per variant).
/// Two reactions with the same hash are considered identical for
/// reload-diff purposes.
fn hash_reaction(on: &OnNode) -> u64 {
    let mut h = DefaultHasher::new();
    if let Some(sel) = &on.selector {
        sel.kind.hash(&mut h);
        for (k, v) in &sel.field_patterns {
            k.hash(&mut h);
            v.raw.hash(&mut h);
            std::mem::discriminant(&v.match_type).hash(&mut h);
        }
    }
    on.when.hash(&mut h);
    on.ops.len().hash(&mut h);
    for op in &on.ops {
        hash_op_node(op, &mut h);
    }
    h.finish()
}

/// Compute the diff between two reaction slices.
///
/// Returns indices of added reactions (present in `new` but not `old`)
/// and removed reactions (present in `old` but not `new`). Matching is
/// by content hash — two reactions with identical selector, predicate,
/// and ops are considered the same regardless of position.
pub fn diff_reactions(old: &[OnNode], new: &[OnNode]) -> ReactionDiff {
    let old_hashes: Vec<u64> = old.iter().map(hash_reaction).collect();
    let new_hashes: Vec<u64> = new.iter().map(hash_reaction).collect();

    // Multiset diff via count maps.
    let mut old_counts: HashMap<u64, usize> = HashMap::new();
    for &h in &old_hashes {
        *old_counts.entry(h).or_default() += 1;
    }

    let mut new_counts: HashMap<u64, usize> = HashMap::new();
    for &h in &new_hashes {
        *new_counts.entry(h).or_default() += 1;
    }

    // Added: in new but not (fully) in old.
    let mut added = Vec::new();
    let mut remaining_old: HashMap<u64, usize> = old_counts.clone();
    for (i, &h) in new_hashes.iter().enumerate() {
        if let Some(count) = remaining_old.get_mut(&h) {
            if *count > 0 {
                *count -= 1;
                continue;
            }
        }
        added.push(i);
    }

    // Removed: in old but not (fully) in new.
    let mut removed = Vec::new();
    let mut remaining_new: HashMap<u64, usize> = new_counts;
    for (i, &h) in old_hashes.iter().enumerate() {
        if let Some(count) = remaining_new.get_mut(&h) {
            if *count > 0 {
                *count -= 1;
                continue;
            }
        }
        removed.push(i);
    }

    ReactionDiff { added, removed }
}

// ---------------------------------------------------------------------------
// T-131: Keybind diff
// ---------------------------------------------------------------------------

/// Result of diffing two keybind sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindDiff {
    /// Indices into the *new* slice for keybinds whose chord was not
    /// present in the old set.
    pub added: Vec<usize>,
    /// Indices into the *old* slice for keybinds whose chord is not
    /// present in the new set.
    pub removed: Vec<usize>,
    /// Indices into the *new* slice for keybinds whose chord existed
    /// in both sets but whose ops differ.
    pub changed: Vec<usize>,
}

/// Hash the ops of a [`BindNode`] for change detection.
fn hash_bind_ops(bind: &BindNode) -> u64 {
    let mut h = DefaultHasher::new();
    bind.ops.len().hash(&mut h);
    for op in &bind.ops {
        hash_op_node(op, &mut h);
    }
    h.finish()
}

/// Compute the diff between two keybind slices by chord string.
///
/// - **added**: chord present in `new` but not in `old`.
/// - **removed**: chord present in `old` but not in `new`.
/// - **changed**: chord present in both but ops differ.
///
/// When the same chord appears multiple times in a slice, only the
/// *last* occurrence is considered (last-wins semantics per R5).
pub fn diff_keybinds(old: &[BindNode], new: &[BindNode]) -> KeybindDiff {
    let old_map: HashMap<&str, (usize, u64)> = old
        .iter()
        .enumerate()
        .map(|(i, b)| (b.chord.as_str(), (i, hash_bind_ops(b))))
        .collect();

    let new_map: HashMap<&str, (usize, u64)> = new
        .iter()
        .enumerate()
        .map(|(i, b)| (b.chord.as_str(), (i, hash_bind_ops(b))))
        .collect();

    let old_chords: HashSet<&str> = old_map.keys().copied().collect();
    let new_chords: HashSet<&str> = new_map.keys().copied().collect();

    let added: Vec<usize> = new_chords
        .difference(&old_chords)
        .map(|c| new_map[c].0)
        .collect();

    let removed: Vec<usize> = old_chords
        .difference(&new_chords)
        .map(|c| old_map[c].0)
        .collect();

    let changed: Vec<usize> = old_chords
        .intersection(&new_chords)
        .filter(|c| old_map[**c].1 != new_map[**c].1)
        .map(|c| new_map[*c].0)
        .collect();

    KeybindDiff {
        added,
        removed,
        changed,
    }
}

// ---------------------------------------------------------------------------
// T-132: Reload triggers reconciler (stub wiring)
// ---------------------------------------------------------------------------

/// Stub entry point: after a successful reload, the caller should run
/// the full reconciler to converge the live session to the new scene.
///
/// This function is intentionally a stub — the actual wiring lives in
/// the session supervisor (crate `ark-supervisor`) which owns the
/// `Reconciler` instance. The scene crate provides this signature so
/// the reload pipeline has a clear handoff point.
///
/// Returns `true` to indicate that a reconcile was requested.
pub fn trigger_reconcile(reload: &ReloadResult) -> bool {
    match &reload.status {
        ReloadStatus::Ok | ReloadStatus::Partial(_) => {
            tracing::info!(
                reactions_added = reload.reactions_added,
                reactions_removed = reload.reactions_removed,
                keybinds_changed = reload.keybinds_changed,
                "hot-reload: triggering full reconcile"
            );
            true
        }
        ReloadStatus::Failed(_) => false,
    }
}

// ---------------------------------------------------------------------------
// T-133: File watcher (opt-in)
// ---------------------------------------------------------------------------

/// Configuration for the opt-in file watcher that triggers scene
/// reloads on disk changes.
#[derive(Debug, Clone)]
pub struct FileWatcherConfig {
    /// Whether the file watcher is enabled. Defaults to `false` — the
    /// user must opt in via `[watch]` in their config or CLI flag.
    pub enabled: bool,
    /// Debounce window in milliseconds. File-change events within this
    /// window are coalesced into a single reload. Default: 200 ms.
    pub debounce_ms: u64,
    /// File suffixes to ignore (e.g. editor swap files, backups).
    /// Matched against the full file name, not just the extension.
    pub ignore_suffixes: Vec<String>,
    /// File-name prefixes to ignore (e.g. Emacs lock-files like `.#scene.kdl`).
    /// Matched against the file *name* component only.
    pub ignore_prefixes: Vec<String>,
}

impl Default for FileWatcherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debounce_ms: 200,
            ignore_suffixes: vec![".swp", ".tmp", "~", ".bak"]
                .into_iter()
                .map(String::from)
                .collect(),
            ignore_prefixes: vec![".#".to_string()],
        }
    }
}

/// Returns `true` if `path` should be ignored by the file watcher
/// based on `config.ignore_suffixes` and `config.ignore_prefixes`.
///
/// Matching is against the file *name* component (not the full path),
/// checking whether the name ends with any configured suffix or starts
/// with any configured prefix. Directories (paths with no file name
/// component) are always ignored.
pub fn should_ignore_path(path: &Path, config: &FileWatcherConfig) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    config
        .ignore_suffixes
        .iter()
        .any(|suffix| name.ends_with(suffix.as_str()))
        || config
            .ignore_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
}

// ---------------------------------------------------------------------------
// T-134: Reload telemetry
// ---------------------------------------------------------------------------

/// Convert a [`ReloadResult`] into a flat key-value payload suitable
/// for emission as a telemetry / `UserEvent` event.
///
/// The returned map uses string keys and string values so it can be
/// serialized into any telemetry backend without type coercion issues.
pub fn reload_telemetry_payload(result: &ReloadResult) -> HashMap<String, String> {
    let mut payload = HashMap::new();
    payload.insert(
        "status".to_string(),
        match &result.status {
            ReloadStatus::Ok => "ok".to_string(),
            ReloadStatus::Partial(s) => format!("partial: {s}"),
            ReloadStatus::Failed(s) => format!("failed: {s}"),
        },
    );
    payload.insert("duration_ms".to_string(), result.duration_ms.to_string());
    payload.insert(
        "reactions_added".to_string(),
        result.reactions_added.to_string(),
    );
    payload.insert(
        "reactions_removed".to_string(),
        result.reactions_removed.to_string(),
    );
    payload.insert(
        "keybinds_changed".to_string(),
        result.keybinds_changed.to_string(),
    );
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ReloadGuard tests ──────────────────────────────────────────

    #[test]
    fn guard_acquires_when_idle() {
        let guard = ReloadGuard::new();
        assert!(!guard.is_active());
        let lock = guard.try_acquire();
        assert!(lock.is_some(), "should acquire when idle");
        assert!(guard.is_active());
    }

    #[test]
    fn guard_blocks_concurrent_acquire() {
        let guard = ReloadGuard::new();
        let _lock1 = guard.try_acquire().expect("first acquire");
        let lock2 = guard.try_acquire();
        assert!(
            lock2.is_none(),
            "second acquire should fail while first is held"
        );
    }

    #[test]
    fn guard_releases_after_drop() {
        let guard = ReloadGuard::new();
        {
            let _lock = guard.try_acquire().expect("acquire");
            assert!(guard.is_active());
        }
        // Lock dropped — guard should be idle.
        assert!(!guard.is_active());
        let lock2 = guard.try_acquire();
        assert!(
            lock2.is_some(),
            "should acquire after previous lock dropped"
        );
    }

    #[test]
    fn guard_default_is_idle() {
        let guard = ReloadGuard::default();
        assert!(!guard.is_active());
    }

    // ── ReloadResult construction ──────────────────────────────────

    #[test]
    fn reload_result_ok_construction() {
        let result = ReloadResult {
            status: ReloadStatus::Ok,
            duration_ms: 42,
            reactions_added: 3,
            reactions_removed: 1,
            keybinds_changed: 2,
        };
        assert!(matches!(result.status, ReloadStatus::Ok));
        assert_eq!(result.duration_ms, 42);
        assert_eq!(result.reactions_added, 3);
        assert_eq!(result.reactions_removed, 1);
        assert_eq!(result.keybinds_changed, 2);
    }

    #[test]
    fn reload_result_failed_construction() {
        let result = ReloadResult {
            status: ReloadStatus::Failed("parse error".into()),
            duration_ms: 5,
            reactions_added: 0,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        assert!(matches!(result.status, ReloadStatus::Failed(ref s) if s == "parse error"));
    }

    #[test]
    fn reload_result_partial_construction() {
        let result = ReloadResult {
            status: ReloadStatus::Partial("2 warnings".into()),
            duration_ms: 10,
            reactions_added: 1,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        assert!(matches!(result.status, ReloadStatus::Partial(ref s) if s == "2 warnings"));
    }

    // ── reload_scene with missing file returns Failed ──────────────

    #[test]
    fn reload_missing_file_returns_err() {
        let guard = ReloadGuard::new();
        let engine = Engine::new();
        let result = reload_scene(
            Path::new("/tmp/nonexistent_ark_scene_test_file.kdl"),
            &guard,
            &engine,
            None,
        );
        let result = result.expect("should return Some when guard acquired");
        assert!(result.is_err(), "expected Err for missing file");
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("nonexistent"),
            "expected error to mention 'nonexistent', got: {msg}"
        );
    }

    // ── reload_scene respects guard ────────────────────────────────

    #[test]
    fn reload_returns_none_when_guard_held() {
        let guard = ReloadGuard::new();
        let engine = Engine::new();
        let _lock = guard.try_acquire().expect("acquire guard");
        let result = reload_scene(Path::new("scene.kdl"), &guard, &engine, None);
        assert!(result.is_none(), "should return None when guard is held");
    }

    // ── T-128: ReloadQueue ────────────────────────────────────────────

    #[test]
    fn reload_queue_starts_empty() {
        let q = ReloadQueue::new();
        assert!(!q.is_pending());
        assert!(!q.take_pending());
    }

    #[test]
    fn reload_queue_queue_and_take() {
        let q = ReloadQueue::new();
        q.queue();
        assert!(q.is_pending());
        assert!(q.take_pending());
        // Consumed — second take returns false.
        assert!(!q.take_pending());
        assert!(!q.is_pending());
    }

    #[test]
    fn reload_queue_idempotent() {
        let q = ReloadQueue::new();
        q.queue();
        q.queue();
        q.queue();
        // Still only one pending reload.
        assert!(q.take_pending());
        assert!(!q.take_pending());
    }

    #[test]
    fn reload_queue_default_impl() {
        let q = ReloadQueue::default();
        assert!(!q.is_pending());
    }

    // ── T-130: diff_reactions ─────────────────────────────────────────

    use crate::ast::selector::{EventSelector, FieldPattern, MatchType};
    use std::collections::BTreeMap;

    fn make_on(kind: &str, when: Option<&str>) -> OnNode {
        let mut fps = BTreeMap::new();
        fps.insert(
            "path".to_string(),
            FieldPattern {
                raw: "**/*.md".to_string(),
                match_type: MatchType::Glob,
            },
        );
        OnNode {
            selector: Some(EventSelector {
                kind: kind.to_string(),
                field_patterns: fps,
            }),
            when: when.map(String::from),
            ops: Vec::new(),
        }
    }

    #[test]
    fn diff_reactions_identical() {
        let old = vec![make_on("FileEdited", None)];
        let new = vec![make_on("FileEdited", None)];
        let diff = diff_reactions(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_reactions_added() {
        let old: Vec<OnNode> = Vec::new();
        let new = vec![make_on("FileEdited", None)];
        let diff = diff_reactions(&old, &new);
        assert_eq!(diff.added, vec![0]);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_reactions_removed() {
        let old = vec![make_on("FileEdited", None)];
        let new: Vec<OnNode> = Vec::new();
        let diff = diff_reactions(&old, &new);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed, vec![0]);
    }

    #[test]
    fn diff_reactions_when_predicate_differs() {
        let old = vec![make_on("FileEdited", Some("true"))];
        let new = vec![make_on("FileEdited", Some("false"))];
        let diff = diff_reactions(&old, &new);
        assert_eq!(diff.added, vec![0]);
        assert_eq!(diff.removed, vec![0]);
    }

    #[test]
    fn diff_reactions_mixed_add_remove() {
        let old = vec![make_on("FileEdited", None), make_on("Error", None)];
        let new = vec![make_on("FileEdited", None), make_on("ToolUse", None)];
        let diff = diff_reactions(&old, &new);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 1);
    }

    // ── T-131: diff_keybinds ──────────────────────────────────────────

    fn make_bind(chord: &str) -> BindNode {
        BindNode {
            chord: chord.to_string(),
            ops: Vec::new(),
        }
    }

    fn make_bind_with_ops(chord: &str, ops: Vec<crate::ast::ops::OpNode>) -> BindNode {
        BindNode {
            chord: chord.to_string(),
            ops,
        }
    }

    #[test]
    fn diff_keybinds_identical() {
        let old = vec![make_bind("Alt p")];
        let new = vec![make_bind("Alt p")];
        let diff = diff_keybinds(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_keybinds_added() {
        let old: Vec<BindNode> = Vec::new();
        let new = vec![make_bind("Alt p")];
        let diff = diff_keybinds(&old, &new);
        assert_eq!(diff.added, vec![0]);
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_keybinds_removed() {
        let old = vec![make_bind("Alt p")];
        let new: Vec<BindNode> = Vec::new();
        let diff = diff_keybinds(&old, &new);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed, vec![0]);
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn diff_keybinds_changed_ops() {
        use crate::ast::ops::{FocusOp, OpNode};
        let old = vec![make_bind("Alt p")];
        let new = vec![make_bind_with_ops(
            "Alt p",
            vec![OpNode::Focus(FocusOp {
                handle: "@main".to_string(),
                when: None,
            })],
        )];
        let diff = diff_keybinds(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.changed, vec![0]);
    }

    #[test]
    fn diff_keybinds_mixed() {
        let old = vec![make_bind("Alt p"), make_bind("Ctrl c")];
        let new = vec![make_bind("Alt p"), make_bind("Alt d")];
        let diff = diff_keybinds(&old, &new);
        assert_eq!(diff.added.len(), 1); // Alt d
        assert_eq!(diff.removed.len(), 1); // Ctrl c
        assert!(diff.changed.is_empty());
    }

    // ── T-132: trigger_reconcile ──────────────────────────────────────

    #[test]
    fn trigger_reconcile_fires_on_ok() {
        let result = ReloadResult {
            status: ReloadStatus::Ok,
            duration_ms: 10,
            reactions_added: 1,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        assert!(trigger_reconcile(&result));
    }

    #[test]
    fn trigger_reconcile_skips_on_failure() {
        let result = ReloadResult {
            status: ReloadStatus::Failed("parse error".into()),
            duration_ms: 5,
            reactions_added: 0,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        assert!(!trigger_reconcile(&result));
    }

    #[test]
    fn trigger_reconcile_fires_on_partial() {
        let result = ReloadResult {
            status: ReloadStatus::Partial("warnings".into()),
            duration_ms: 5,
            reactions_added: 0,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        assert!(trigger_reconcile(&result));
    }

    // ── T-133: FileWatcherConfig + should_ignore_path ─────────────────

    #[test]
    fn file_watcher_config_defaults() {
        let cfg = FileWatcherConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.debounce_ms, 200);
        assert!(cfg.ignore_suffixes.contains(&".swp".to_string()));
        assert!(cfg.ignore_suffixes.contains(&".tmp".to_string()));
        assert!(cfg.ignore_suffixes.contains(&"~".to_string()));
        assert!(cfg.ignore_suffixes.contains(&".bak".to_string()));
        assert!(cfg.ignore_prefixes.contains(&".#".to_string()));
    }

    #[test]
    fn should_ignore_swap_files() {
        let cfg = FileWatcherConfig::default();
        assert!(should_ignore_path(Path::new("scene.kdl.swp"), &cfg));
        assert!(should_ignore_path(Path::new("/tmp/foo.tmp"), &cfg));
        assert!(should_ignore_path(Path::new("scene.kdl~"), &cfg));
        assert!(should_ignore_path(Path::new("backup.bak"), &cfg));
    }

    #[test]
    fn should_not_ignore_kdl_files() {
        let cfg = FileWatcherConfig::default();
        assert!(!should_ignore_path(Path::new("scene.kdl"), &cfg));
        assert!(!should_ignore_path(Path::new("/home/user/scene.kdl"), &cfg));
    }

    #[test]
    fn should_ignore_bare_directory() {
        let cfg = FileWatcherConfig::default();
        assert!(should_ignore_path(Path::new(""), &cfg));
    }

    #[test]
    fn should_ignore_custom_suffix() {
        let cfg = FileWatcherConfig {
            ignore_suffixes: vec![".lock".to_string()],
            ..Default::default()
        };
        assert!(should_ignore_path(Path::new("scene.lock"), &cfg));
        assert!(!should_ignore_path(Path::new("scene.kdl"), &cfg));
    }

    #[test]
    fn should_ignore_emacs_lock_prefix() {
        let cfg = FileWatcherConfig::default();
        assert!(should_ignore_path(Path::new(".#scene.kdl"), &cfg));
        assert!(should_ignore_path(Path::new("/tmp/.#foo.kdl"), &cfg));
        assert!(!should_ignore_path(Path::new("scene.kdl"), &cfg));
    }

    // ── T-134: reload_telemetry_payload ───────────────────────────────

    #[test]
    fn telemetry_payload_success() {
        let result = ReloadResult {
            status: ReloadStatus::Ok,
            duration_ms: 42,
            reactions_added: 2,
            reactions_removed: 1,
            keybinds_changed: 1,
        };
        let payload = reload_telemetry_payload(&result);
        assert_eq!(payload.get("status").unwrap(), "ok");
        assert_eq!(payload.get("reactions_added").unwrap(), "2");
        assert_eq!(payload.get("reactions_removed").unwrap(), "1");
        assert_eq!(payload.get("keybinds_changed").unwrap(), "1");
        assert_eq!(payload.get("duration_ms").unwrap(), "42");
    }

    #[test]
    fn telemetry_payload_with_error() {
        let result = ReloadResult {
            status: ReloadStatus::Failed("bad KDL".to_string()),
            duration_ms: 5,
            reactions_added: 0,
            reactions_removed: 0,
            keybinds_changed: 0,
        };
        let payload = reload_telemetry_payload(&result);
        assert!(payload.get("status").unwrap().starts_with("failed:"));
        assert!(payload.get("status").unwrap().contains("bad KDL"));
    }
}
