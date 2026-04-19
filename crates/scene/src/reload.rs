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
//! - [`ReloadQueue`] (T-128, original): single-bit "reload pending" flag.
//! - [`TurnInflightGate`] (T-128, repurposed post-ACP): per-extension
//!   turn-inflight tracker. Blocks reloads while any registered extension
//!   has a turn in flight (e.g. claude-code between `UserPromptSubmit`
//!   and `Stop`). Post-2026-04-18 pivot: replaces the original ACP-turn
//!   tracker — the ACP surface was CUT, but the claude-code extension's
//!   hook-event lifecycle (SessionStart/UserPromptSubmit → Stop) gives
//!   an equivalent mid-turn signal. If no extension registers, no gate —
//!   reloads pass through immediately.
//! - [`diff_reactions`] / [`ReactionDiff`] (T-130): Content-hash-based
//!   reaction diff for detecting subscription-set changes.
//! - [`diff_keybinds`] / [`KeybindDiff`] (T-131): Chord-keyed keybind
//!   diff for detecting added/removed/changed binds.
//! - [`trigger_reconcile`] (T-132): Stub wiring that invokes the full
//!   reconciler after a successful reload.
//! - [`FileWatcherConfig`] / [`should_ignore_path`] (T-133): Opt-in
//!   file watcher configuration with debounce and ignore suffixes.
//! - [`SceneFileWatcher`] (T-133, runtime): `notify`-backed watcher
//!   thread that re-hashes the scene file on disk changes and emits
//!   debounced reload-request events on content changes.
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
// T-128 (repurposed post-ACP): TurnInflightGate
// ---------------------------------------------------------------------------

/// Tracks whether any registered extension is mid-turn, so scene-reload
/// requests that arrive during a turn can be queued and flushed when the
/// turn ends.
///
/// Original T-128 blocked reloads while an ACP turn was in flight; that
/// surface was CUT in the 2026-04-18 pivot. The honest repurposing
/// tracks per-extension turn inflight state driven by extension hook
/// events — e.g. claude-code-ext marks inflight on
/// `claude-code.user.prompt-submit` and clears it on `claude-code.stop`.
///
/// If no extension has ever registered an inflight turn, the gate is a
/// no-op and [`try_gate_or_queue`] always returns [`GateDecision::Pass`]
/// — keeping the fast path fast for scenes that don't load any
/// turn-producing extensions.
///
/// Thread-safety: interior `std::sync::Mutex` around a `HashMap`. Lock
/// is held only while mutating the map or consulting it — never across a
/// reload call.
pub struct TurnInflightGate {
    /// Map of extension name → inflight-turn count. A count > 0 means
    /// the extension has at least one open turn; pending reloads must
    /// queue.
    ///
    /// The count (rather than a bool) guards against partially overlapping
    /// start/stop pairs on the same extension — e.g. claude-code's
    /// `UserPromptSubmit` then `Stop` while a `SessionStart` is still
    /// bookkeeping-open.
    inflight: std::sync::Mutex<HashMap<String, u32>>,
    /// Set alongside the inflight map when a reload was queued during a
    /// turn. Flushed by [`take_pending`].
    pending: AtomicBool,
}

/// Decision returned by [`TurnInflightGate::try_gate_or_queue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// No turns in flight — caller should apply the reload immediately.
    Pass,
    /// At least one turn is in flight — caller queued the reload; it
    /// will be flushed when every turn completes.
    Queued,
}

impl TurnInflightGate {
    /// Construct an empty gate. No registered extensions → all reloads
    /// pass through.
    pub fn new() -> Self {
        Self {
            inflight: std::sync::Mutex::new(HashMap::new()),
            pending: AtomicBool::new(false),
        }
    }

    /// Mark a new turn as in-flight for `ext`. Idempotency is by
    /// reference count — pair every call with a matching
    /// [`turn_ended`].
    ///
    /// Typical wiring: subscribe to `<ext>.user.prompt-submit` /
    /// `<ext>.session.start` and call `turn_started(ext)`.
    pub fn turn_started(&self, ext: &str) {
        let mut map = match self.inflight.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        *map.entry(ext.to_string()).or_insert(0) += 1;
    }

    /// Mark a turn as finished for `ext`. If the reference count drops
    /// to zero the extension is removed from the map. Calls beyond the
    /// open-count are saturating (no underflow panic) — extensions that
    /// emit spurious `Stop` events won't crash the supervisor.
    ///
    /// Returns `true` if after this call there are no more turns in
    /// flight across any registered extension — the caller uses this to
    /// decide whether to flush a queued reload via [`take_pending`].
    pub fn turn_ended(&self, ext: &str) -> bool {
        let mut map = match self.inflight.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        if let Some(count) = map.get_mut(ext) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(ext);
            }
        } else {
            tracing::debug!(
                ext = %ext,
                "TurnInflightGate::turn_ended with no matching turn_started (dropping)"
            );
        }
        map.is_empty()
    }

    /// Clear every inflight turn for `ext` (e.g. on session end or
    /// extension shutdown). Returns `true` if the gate is now fully
    /// idle.
    pub fn clear_ext(&self, ext: &str) -> bool {
        let mut map = match self.inflight.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        map.remove(ext);
        map.is_empty()
    }

    /// `true` when at least one registered extension has a turn in
    /// flight.
    pub fn any_inflight(&self) -> bool {
        let map = match self.inflight.lock() {
            Ok(m) => m,
            Err(p) => p.into_inner(),
        };
        !map.is_empty()
    }

    /// Consult the gate for a reload request. When a turn is in flight
    /// the caller flips the internal `pending` flag and returns
    /// [`GateDecision::Queued`]; otherwise [`GateDecision::Pass`] —
    /// the caller applies the reload immediately and leaves pending
    /// untouched.
    pub fn try_gate_or_queue(&self) -> GateDecision {
        if self.any_inflight() {
            self.pending.store(true, Ordering::SeqCst);
            GateDecision::Queued
        } else {
            GateDecision::Pass
        }
    }

    /// Atomically consume the pending flag. Returns `true` if a reload
    /// was queued while turns were inflight and the caller should now
    /// apply it.
    pub fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::SeqCst)
    }

    /// Check the pending flag without consuming it.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
}

impl Default for TurnInflightGate {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TurnInflightGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let map_dbg = match self.inflight.lock() {
            Ok(m) => format!("{:?}", *m),
            Err(_) => "<poisoned>".to_string(),
        };
        f.debug_struct("TurnInflightGate")
            .field("inflight", &map_dbg)
            .field("pending", &self.is_pending())
            .finish()
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
// T-133 (runtime): SceneFileWatcher — notify-backed debounced watcher
// ---------------------------------------------------------------------------

/// Event emitted by [`SceneFileWatcher`] when the watched scene file's
/// content has changed (debounced + content-hash-filtered).
///
/// Receivers re-read the scene from disk, feed it into [`reload_scene`],
/// and drive the reconciler (T-132) on a successful reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneFileEvent {
    /// The watched scene file changed on disk and its content hash no
    /// longer matches the previously-observed hash. The `path` is the
    /// watched scene path as configured.
    Changed(std::path::PathBuf),
    /// The watched scene file was removed or became unreadable. The
    /// caller should decide whether to abort the watcher or keep
    /// retrying — this watcher does neither on its own.
    Vanished(std::path::PathBuf),
}

/// Runtime primitive that owns a `notify::RecommendedWatcher` plus a
/// debounce thread, and emits [`SceneFileEvent`]s on the receiver
/// returned by [`SceneFileWatcher::start`].
///
/// Lifecycle:
///
/// 1. [`start`][Self::start] registers a recursive watch on the scene
///    file's **parent directory** (watching the file itself fails on
///    editor-save patterns that rename-in-place — atomic-save rewrites
///    the file inode, which breaks a file-level watch).
/// 2. A background thread reads notify events, filters by path
///    equality to the configured scene path + the
///    [`FileWatcherConfig`] ignore lists, and debounces with the
///    configured window (default 200 ms, shared with the T-043
///    reconciler debounce).
/// 3. After the debounce window elapses, the thread re-reads the scene
///    file, computes its blake3 hash via [`SceneId::from_file`], and
///    compares to the last observed hash. On content change it emits
///    `SceneFileEvent::Changed`; on first-read failure (e.g. mid-save)
///    it logs a debug and retries on the next event.
/// 4. Watcher errors log WARN and are dropped — the watcher keeps
///    running so transient filesystem issues don't crash the session.
///
/// Dropping the returned `SceneFileWatcher` stops the watch cleanly.
pub struct SceneFileWatcher {
    /// Keeps the notify watcher alive. Dropping this stops watching.
    _watcher: notify::RecommendedWatcher,
    /// Shutdown signal for the debounce thread.
    shutdown: std::sync::Arc<AtomicBool>,
    /// Join handle for the debounce thread. Kept so `Drop` can join it.
    debouncer: Option<std::thread::JoinHandle<()>>,
    /// Watched scene path (absolute). Retained for diagnostics.
    scene_path: std::path::PathBuf,
}

impl SceneFileWatcher {
    /// Start watching `scene_path`'s parent directory for changes.
    ///
    /// Returns the watcher handle (keep it alive — dropping stops the
    /// watch) and a `std::sync::mpsc::Receiver<SceneFileEvent>` the
    /// caller drains on its own thread or tokio task.
    ///
    /// # Errors
    ///
    /// Returns a `notify::Error` if:
    /// - the scene path has no parent directory (e.g. bare file name
    ///   like `"."`),
    /// - notify cannot install a watch on the parent directory
    ///   (e.g. missing directory, permission denied).
    ///
    /// Missing file at start-time is NOT an error — notify watches the
    /// parent directory, so a later write creating the file will trigger
    /// a `Changed` event.
    pub fn start(
        scene_path: impl Into<std::path::PathBuf>,
        config: FileWatcherConfig,
    ) -> notify::Result<(Self, std::sync::mpsc::Receiver<SceneFileEvent>)> {
        use notify::{RecursiveMode, Watcher};

        let scene_path: std::path::PathBuf = scene_path.into();
        let parent = scene_path.parent().ok_or_else(|| {
            notify::Error::generic(&format!(
                "scene path {} has no parent directory",
                scene_path.display()
            ))
        })?;
        // Snapshot an immutable copy for the debouncer thread.
        let watched_path = scene_path.clone();

        // Internal channel: notify handler → debouncer thread.
        let (raw_tx, raw_rx) = std::sync::mpsc::channel::<std::path::PathBuf>();
        // External channel: debouncer thread → caller.
        let (out_tx, out_rx) = std::sync::mpsc::channel::<SceneFileEvent>();

        let ignore_cfg = config.clone();
        let target_for_filter = watched_path.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    for path in &event.paths {
                        // Fast-reject paths that aren't the watched file
                        // or are matched by the ignore lists. macOS
                        // FSEvents reports `/private`-prefixed paths, so
                        // compare by file-name as a fallback when the
                        // full-path equality misses.
                        if path != &target_for_filter
                            && path.file_name() != target_for_filter.file_name()
                        {
                            continue;
                        }
                        if should_ignore_path(path, &ignore_cfg) {
                            continue;
                        }
                        // Deliver the event to the debouncer. Receiver
                        // dropped = watcher is being torn down.
                        if raw_tx.send(path.clone()).is_err() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SceneFileWatcher: notify error");
                }
            })?;

        watcher.watch(parent, RecursiveMode::NonRecursive)?;

        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = shutdown.clone();
        let debounce = std::time::Duration::from_millis(config.debounce_ms);
        let debouncer_path = watched_path.clone();

        let handle = std::thread::Builder::new()
            .name("ark-scene-file-watcher".into())
            .spawn(move || {
                debounce_loop(
                    raw_rx,
                    out_tx,
                    debouncer_path,
                    debounce,
                    shutdown_for_thread,
                )
            })
            .map_err(|e| notify::Error::generic(&format!("spawn debouncer: {e}")))?;

        Ok((
            Self {
                _watcher: watcher,
                shutdown,
                debouncer: Some(handle),
                scene_path: watched_path,
            },
            out_rx,
        ))
    }

    /// Path the watcher is bound to.
    pub fn scene_path(&self) -> &Path {
        &self.scene_path
    }
}

impl Drop for SceneFileWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.debouncer.take() {
            // Best-effort join; drop path should not panic on poison.
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for SceneFileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneFileWatcher")
            .field("scene_path", &self.scene_path)
            .field("debouncer_alive", &self.debouncer.is_some())
            .finish()
    }
}

/// Debouncer event loop: collects raw notify paths, waits `debounce` ms
/// for the dust to settle, then re-hashes the scene file. Only emits a
/// `Changed` event when the content hash differs from the last observed
/// hash — purely-metadata touches (`touch scene.kdl`) are silently
/// dropped.
fn debounce_loop(
    raw_rx: std::sync::mpsc::Receiver<std::path::PathBuf>,
    out_tx: std::sync::mpsc::Sender<SceneFileEvent>,
    scene_path: std::path::PathBuf,
    debounce: std::time::Duration,
    shutdown: std::sync::Arc<AtomicBool>,
) {
    use std::sync::mpsc::RecvTimeoutError;

    // Track the last observed content hash so we can suppress
    // metadata-only touches. Initialize from disk so a watcher
    // installed after the scene was compiled doesn't falsely report
    // the first post-start write as "changed when it wasn't".
    let mut last_hash = crate::id::SceneId::from_file(&scene_path)
        .ok()
        .map(|id| id.content_hash);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        // Wait for the first event (bounded) so shutdown can propagate.
        let first = match raw_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(p) => p,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };

        // Drain any further events within the debounce window.
        let deadline = std::time::Instant::now() + debounce;
        let mut _last_path = first;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match raw_rx.recv_timeout(remaining) {
                Ok(p) => _last_path = p,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        // Re-hash the scene file; decide whether to emit.
        match crate::id::SceneId::from_file(&scene_path) {
            Ok(id) => {
                let new_hash = id.content_hash;
                let content_changed = match last_hash {
                    Some(prev) => prev != new_hash,
                    None => true, // first successful read after a vanish
                };
                if content_changed {
                    last_hash = Some(new_hash);
                    if out_tx
                        .send(SceneFileEvent::Changed(scene_path.clone()))
                        .is_err()
                    {
                        return;
                    }
                } else {
                    tracing::debug!(
                        path = %scene_path.display(),
                        "SceneFileWatcher: content hash unchanged, suppressing reload"
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File vanished — emit once, then null the hash so the
                // next successful read reports as a change.
                if last_hash.is_some() {
                    last_hash = None;
                    if out_tx
                        .send(SceneFileEvent::Vanished(scene_path.clone()))
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    path = %scene_path.display(),
                    error = %e,
                    "SceneFileWatcher: re-hash failed, keeping last hash"
                );
            }
        }
    }
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

    // ── T-128 (repurposed): TurnInflightGate ──────────────────────────

    #[test]
    fn turn_gate_idle_passes_reload() {
        let gate = TurnInflightGate::new();
        assert!(!gate.any_inflight());
        assert_eq!(gate.try_gate_or_queue(), GateDecision::Pass);
        assert!(!gate.is_pending());
    }

    #[test]
    fn turn_gate_inflight_queues_reload() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        assert!(gate.any_inflight());
        assert_eq!(gate.try_gate_or_queue(), GateDecision::Queued);
        assert!(gate.is_pending());
    }

    #[test]
    fn turn_gate_take_pending_flushes_once() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        assert_eq!(gate.try_gate_or_queue(), GateDecision::Queued);
        assert!(gate.take_pending());
        // Second take → false: flag is consumed.
        assert!(!gate.take_pending());
        assert!(!gate.is_pending());
    }

    #[test]
    fn turn_gate_ref_counts_nested_turns() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        gate.turn_started("claude-code");
        // One end still leaves a turn open.
        let idle = gate.turn_ended("claude-code");
        assert!(!idle);
        assert!(gate.any_inflight());
        // Second end drops the last turn → gate is idle.
        let idle = gate.turn_ended("claude-code");
        assert!(idle);
        assert!(!gate.any_inflight());
    }

    #[test]
    fn turn_gate_saturating_end_does_not_panic() {
        let gate = TurnInflightGate::new();
        // Spurious end with no start logs a debug but doesn't panic
        // (claude-code sends bare `Stop` on crash recovery).
        let idle = gate.turn_ended("claude-code");
        assert!(idle);
    }

    #[test]
    fn turn_gate_multiple_exts_both_block() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        gate.turn_started("codex");
        assert!(gate.any_inflight());
        // Ending one ext still leaves another inflight.
        let idle = gate.turn_ended("claude-code");
        assert!(!idle);
        let idle = gate.turn_ended("codex");
        assert!(idle);
    }

    #[test]
    fn turn_gate_clear_ext() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        gate.turn_started("claude-code");
        gate.turn_started("claude-code");
        let idle = gate.clear_ext("claude-code");
        assert!(idle);
        assert!(!gate.any_inflight());
    }

    #[test]
    fn turn_gate_clear_unknown_ext_is_no_op() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        let idle = gate.clear_ext("unknown");
        assert!(!idle);
        assert!(gate.any_inflight());
    }

    #[test]
    fn turn_gate_queue_persists_across_partial_end() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        gate.turn_started("codex");
        assert_eq!(gate.try_gate_or_queue(), GateDecision::Queued);
        // Ending one extension doesn't flush the pending bit yet.
        gate.turn_ended("claude-code");
        assert!(gate.is_pending());
        assert!(gate.any_inflight());
        // Only after every ext is idle is it safe to flush.
        gate.turn_ended("codex");
        assert!(!gate.any_inflight());
        assert!(gate.take_pending());
    }

    #[test]
    fn turn_gate_debug_impl_does_not_panic() {
        let gate = TurnInflightGate::new();
        gate.turn_started("claude-code");
        let _ = format!("{gate:?}");
    }

    // ── T-133 (runtime): SceneFileWatcher ────────────────────────────

    use std::time::Duration;
    use std::time::Instant;
    use tempfile::TempDir;

    /// Poll `rx` for up to `deadline_secs` seconds waiting for a
    /// `SceneFileEvent::Changed(_)`. Intermediate events (e.g. vanish
    /// due to rename-in-place save patterns) are ignored.
    fn wait_for_change(
        rx: &std::sync::mpsc::Receiver<SceneFileEvent>,
        deadline_secs: u64,
    ) -> Option<SceneFileEvent> {
        let deadline = Instant::now() + Duration::from_secs(deadline_secs);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ev @ SceneFileEvent::Changed(_)) => return Some(ev),
                Ok(_) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    fn minimal_scene(marker: &str) -> String {
        format!(
            r#"scene "t-{marker}" {{
    tab {{
        pane command="echo {marker}" !@p
    }}
}}
"#
        )
    }

    #[test]
    fn watcher_emits_changed_on_content_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scene.kdl");
        std::fs::write(&path, minimal_scene("one")).unwrap();

        let cfg = FileWatcherConfig {
            enabled: true,
            debounce_ms: 100,
            ..FileWatcherConfig::default()
        };
        let (_w, rx) = SceneFileWatcher::start(&path, cfg).expect("watcher starts");

        // Give notify a moment to register.
        std::thread::sleep(Duration::from_millis(100));

        // Rewrite with different content.
        std::fs::write(&path, minimal_scene("two")).unwrap();

        let ev = wait_for_change(&rx, 3).expect("Changed within 3s");
        match ev {
            SceneFileEvent::Changed(p) => {
                assert_eq!(p.file_name(), path.file_name(), "event path mismatch");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn watcher_suppresses_metadata_only_touch() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scene.kdl");
        let src = minimal_scene("stable");
        std::fs::write(&path, &src).unwrap();

        let cfg = FileWatcherConfig {
            enabled: true,
            debounce_ms: 100,
            ..FileWatcherConfig::default()
        };
        let (_w, rx) = SceneFileWatcher::start(&path, cfg).expect("watcher starts");
        std::thread::sleep(Duration::from_millis(100));

        // Rewrite with *identical* content — hash unchanged, should be
        // suppressed. Use write() which triggers a notify event despite
        // byte-identical content.
        std::fs::write(&path, &src).unwrap();

        // Poll for ~500 ms; no event should arrive.
        let ev = rx.recv_timeout(Duration::from_millis(500));
        assert!(
            ev.is_err(),
            "expected no Changed event for metadata-only touch, got {ev:?}"
        );
    }

    #[test]
    fn watcher_ignores_swap_files() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scene.kdl");
        std::fs::write(&path, minimal_scene("base")).unwrap();

        let cfg = FileWatcherConfig {
            enabled: true,
            debounce_ms: 100,
            ..FileWatcherConfig::default()
        };
        let (_w, rx) = SceneFileWatcher::start(&path, cfg).expect("watcher starts");
        std::thread::sleep(Duration::from_millis(100));

        // Write a swap file — should NOT trigger a scene event because
        // the file name doesn't match, AND even if it did, it's
        // ignore-suffix-matched.
        std::fs::write(tmp.path().join("scene.kdl.swp"), "junk").unwrap();

        let ev = rx.recv_timeout(Duration::from_millis(500));
        assert!(
            ev.is_err(),
            "expected no event for swap-file write, got {ev:?}"
        );
    }

    #[test]
    fn watcher_debounces_rapid_writes_into_single_event() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scene.kdl");
        std::fs::write(&path, minimal_scene("v0")).unwrap();

        let cfg = FileWatcherConfig {
            enabled: true,
            debounce_ms: 200,
            ..FileWatcherConfig::default()
        };
        let (_w, rx) = SceneFileWatcher::start(&path, cfg).expect("watcher starts");
        std::thread::sleep(Duration::from_millis(100));

        // Three rapid writes within the debounce window — expect ONE
        // `Changed` event carrying the final state.
        std::fs::write(&path, minimal_scene("v1")).unwrap();
        std::fs::write(&path, minimal_scene("v2")).unwrap();
        std::fs::write(&path, minimal_scene("v3")).unwrap();

        let first = wait_for_change(&rx, 3).expect("first Changed");
        assert!(matches!(first, SceneFileEvent::Changed(_)));

        // No *additional* Changed event in the next 400 ms (all writes
        // were coalesced into the first).
        let extra = rx.recv_timeout(Duration::from_millis(400));
        assert!(
            extra.is_err(),
            "expected coalesced single event, got extra: {extra:?}"
        );
    }

    #[test]
    fn watcher_start_fails_on_missing_parent() {
        // No parent dir → notify can't watch.
        let cfg = FileWatcherConfig::default();
        let result = SceneFileWatcher::start(std::path::PathBuf::from("/"), cfg);
        assert!(result.is_err(), "root path has no parent");
    }

    #[test]
    fn watcher_drop_stops_debouncer_thread() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("scene.kdl");
        std::fs::write(&path, minimal_scene("drop")).unwrap();

        let cfg = FileWatcherConfig {
            enabled: true,
            debounce_ms: 50,
            ..FileWatcherConfig::default()
        };
        let (w, rx) = SceneFileWatcher::start(&path, cfg).expect("watcher starts");

        // Drop the watcher — the debouncer thread should exit cleanly
        // within a bounded window, and the receiver side should see its
        // sender dropped.
        drop(w);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                _ => continue,
            }
        }
        panic!("debouncer thread did not exit within 2s of Drop");
    }
}
