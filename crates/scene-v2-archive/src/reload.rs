//! Scene hot-reload mechanism (T-11.1 through T-11.8).
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
//! # Delta ordering (T-11.7 staged recovery)
//!
//! Deltas are applied in safety order: reactions -> keybinds -> plugins
//! -> layout. This matches the T-11.1 spec and ensures the reaction
//! registry is consistent before keybind or plugin changes can trigger
//! new dispatches. Per T-11.7, the stages are attempted in order; a
//! failure mid-sequence leaves prior stages applied and aborts the
//! failed stage + later stages. The [`ReloadStatus`] on the returned
//! telemetry reflects whether every stage ran (`Ok`), some stages were
//! aborted (`Partial`), or the reload failed before any stage ran
//! (`Failed`).
//!
//! # Diff engine (T-11.2 — T-11.5)
//!
//! * **Reactions (T-11.2):** every compiled reaction is reduced to a
//!   32-byte blake3 hash of `(normalised selector, predicate source,
//!   compiled op IR)`. Comment-only and whitespace-only edits are
//!   squashed by the KDL round-trip and the CEL compiler, so those
//!   edits DO NOT produce a hash change and DO NOT touch the reaction
//!   registry.
//! * **Keybinds (T-11.3):** compared by chord. Added / removed /
//!   changed (same chord, different op body) chords are surfaced as a
//!   [`KeybindChange`] list; the supervisor's `ark-bus` wiring picks
//!   those up and emits batched `rebind_keys` — this module owns the
//!   diff, not the transport.
//! * **Plugins (T-11.4):** compared by name. Four cases are classified
//!   on the [`PluginChange`] enum — source change, mount change, config
//!   change, lifecycle change — mirroring the R14 spec so the
//!   supervisor can route each case to the correct lifecycle op.
//! * **Layout (T-11.5):** ANY structural layout change (tab shape
//!   change, pane shape change) trips a coarse "layout changed" flag.
//!   v0.1 DOES NOT apply layout deltas at runtime; instead the
//!   reloader emits `UserEvent:ark.scene.reload_partial` and the other
//!   (reactions / keybinds / plugins) stages still apply. Layout
//!   changes require a session restart in v0.1.
//!
//! # Telemetry (T-11.8)
//!
//! Every completed reload produces a [`ReloadTelemetry`] record with
//! `duration_ms`, `reactions_added`, `reactions_removed`,
//! `keybinds_changed`, `plugins_changed`, and a [`ReloadStatus`]. The
//! record is surfaced two ways:
//!
//! * via the [`ReloadOutcome`] returned to the caller (so the caller
//!   can synthesise a `UserEvent:ark.scene.reloaded` broadcast), and
//! * via a structured `tracing::info!(target = "scene::reload", …)`
//!   event.
//!
//! The reloader itself does NOT depend on the ark-bus event broadcast
//! surface — the caller owns event emission so the scene crate keeps a
//! narrow dependency footprint. The emitted telemetry struct is the
//! payload the caller serialises into the broadcast.
//!
//! # Registry swap
//!
//! The new [`ReactionRegistry`] is published through an
//! `Arc<Mutex<Arc<ReactionRegistry>>>` so the reaction dispatcher can
//! read the current registry without holding a lock. The swap is atomic
//! from the reader's perspective.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::ast::{LayoutNode, PaneNode, PluginNode, SceneDoc, TabNode};
use crate::error::SceneError;
use crate::reactions::{populate_registry, ReactionEntry, ReactionRegistry};
use crate::validate::validate_scene;

// ---------------------------------------------------------------------------
// Reaction hashing (T-11.2)
// ---------------------------------------------------------------------------

/// Structural hash of a single compiled reaction.
///
/// Computed by [`reaction_hash`] as `blake3(normalised_selector ‖
/// predicate_source ‖ op_ir)`. The hash is stable across pure
/// whitespace / comment edits because:
///
/// * the selector is trimmed before hashing;
/// * the CEL predicate is rendered as its source expression, which the
///   CEL grammar treats whitespace-insensitively (the `cel_interpreter`
///   parser normalises the token stream, so two predicates that differ
///   only in whitespace produce identical stored source — we hash the
///   stored source, not the raw bytes);
/// * the op list is hashed by walking each `CompiledOp`'s `KdlNode`
///   `Display` rendering, which is the canonical single-line
///   re-serialisation and squashes formatting differences.
///
/// Two semantically-identical reactions (differing only in layout or
/// comments) therefore map to the same `ReactionHash`.
pub type ReactionHash = [u8; 32];

/// Hash a single [`ReactionEntry`] into a structural fingerprint.
///
/// The wire format fed to blake3 is a length-prefixed concatenation:
///
/// ```text
/// [u32 len][selector bytes]
/// [u32 len][predicate source bytes]  // empty length when no predicate
/// [u32 op_count]
///   [u32 len][op name bytes]
///   [u32 len][op kdl-node bytes]
///   …
/// ```
///
/// Length prefixes guard against collisions from ambiguous
/// concatenation boundaries (e.g. two ops whose serialised forms
/// together happen to alias one longer op's serialisation).
pub fn reaction_hash(entry: &ReactionEntry) -> ReactionHash {
    let mut hasher = blake3::Hasher::new();

    // --- selector ---
    // Normalise by trimming outer whitespace; inner whitespace within a
    // field-pattern selector is already significant (`"Kind field=val"`).
    let selector = entry.selector.trim();
    write_len_prefixed(&mut hasher, selector.as_bytes());

    // --- predicate ---
    //
    // cel_interpreter::Program does not expose a canonical source
    // accessor. We carry the predicate SOURCE separately if the entry
    // was built via the registry path — but `ReactionEntry` currently
    // only stores the compiled `Arc<Program>`. For the reload diff we
    // treat the presence/absence of a predicate as one bit of hash
    // input, and the compiled `Program`'s `Debug` rendering as the
    // semantic content (stable across whitespace because the Program's
    // internal AST is the output of the CEL parser, which throws away
    // layout).
    //
    // This is not a closed form — a future upstream change that
    // perturbs `Program`'s `Debug` output would invalidate cached
    // hashes — but within one ark binary build the rendering is
    // deterministic, which is the contract the reload diff needs.
    match &entry.predicate {
        Some(prog) => {
            // Tag byte = 1 for "has predicate".
            hasher.update(&[1u8]);
            let rendered = format!("{prog:?}");
            write_len_prefixed(&mut hasher, rendered.as_bytes());
        }
        None => {
            // Tag byte = 0 for "no predicate".
            hasher.update(&[0u8]);
            write_len_prefixed(&mut hasher, &[]);
        }
    }

    // --- ops ---
    let op_count = entry.ops.len() as u32;
    hasher.update(&op_count.to_le_bytes());
    for op in &entry.ops {
        write_len_prefixed(&mut hasher, op.name.as_bytes());
        // `KdlNode: Display` is the canonical single-line rendering.
        // Whitespace-only edits in the source collapse here.
        let rendered = op.node.to_string();
        write_len_prefixed(&mut hasher, rendered.as_bytes());
    }

    *hasher.finalize().as_bytes()
}

/// Length-prefix a byte slice into the hasher.
fn write_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    let len = bytes.len() as u32;
    hasher.update(&len.to_le_bytes());
    hasher.update(bytes);
}

/// Multiset of reaction hashes keyed by hash value, with a count per
/// hash (two structurally-identical reactions collide in the multiset;
/// the diff-add/remove sets count those occurrences separately).
fn reaction_hash_multiset(registry: &ReactionRegistry) -> BTreeMap<ReactionHash, usize> {
    let mut set: BTreeMap<ReactionHash, usize> = BTreeMap::new();
    for (_kind, entries) in registry.iter_primary() {
        for entry in entries {
            *set.entry(reaction_hash(entry)).or_insert(0) += 1;
        }
    }
    set
}

/// Diff the reaction registries structurally.
///
/// Returns `(added_count, removed_count)` — the cardinality of the
/// symmetric difference split by direction. Two reactions with the
/// same hash in both old and new cancel out (no change). Comment-only
/// and whitespace-only edits produce identical hashes on both sides,
/// so they surface as `(0, 0)`.
pub fn diff_reactions(
    old: &ReactionRegistry,
    new: &ReactionRegistry,
) -> (usize, usize) {
    let old_set = reaction_hash_multiset(old);
    let new_set = reaction_hash_multiset(new);

    let mut added = 0usize;
    let mut removed = 0usize;

    // Hashes present in new but not old (or present with higher count).
    for (h, n_new) in &new_set {
        let n_old = old_set.get(h).copied().unwrap_or(0);
        if *n_new > n_old {
            added += n_new - n_old;
        }
    }
    // Hashes present in old but not new (or present with higher count).
    for (h, n_old) in &old_set {
        let n_new = new_set.get(h).copied().unwrap_or(0);
        if *n_old > n_new {
            removed += n_old - n_new;
        }
    }

    (added, removed)
}

// ---------------------------------------------------------------------------
// Keybind diff (T-11.3)
// ---------------------------------------------------------------------------

/// Per-keybind change classification.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum KeybindChangeKind {
    /// Chord didn't exist in the old scene; exists in the new.
    Added,
    /// Chord existed in the old scene; gone in the new.
    Removed,
    /// Chord exists in both, but the bound body (intent or op list)
    /// differs semantically.
    Changed,
}

/// One entry in the keybind diff.
///
/// The `chord` is the author-written form (spaces, not `+`), matching
/// zellij's chord grammar. The supervisor's `ark-bus` wiring translates
/// this list into batched `rebind_keys` messages — this module is
/// transport-agnostic.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct KeybindChange {
    /// The chord this change targets.
    pub chord: String,

    /// Kind of change.
    pub kind: KeybindChangeKind,
}

/// Hash one keybind's body (intent + op list) to compare two chords
/// bound to different bodies.
fn keybind_body_hash(kb: &crate::ast::KeybindNode) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();

    // intent= attribute.
    match &kb.intent {
        Some(s) => {
            hasher.update(&[1u8]);
            write_len_prefixed(&mut hasher, s.as_bytes());
        }
        None => {
            hasher.update(&[0u8]);
        }
    }

    // op list (same canonical form as reaction ops, via KdlNode Display
    // round-trip).  Today's AST `OpNode` does not carry the KdlNode
    // verbatim, only positional args. We hash the flattened args so
    // whitespace changes are still squashed.
    let op_count = kb.ops.len() as u32;
    hasher.update(&op_count.to_le_bytes());
    for op in &kb.ops {
        let arg_count = op.args.len() as u32;
        hasher.update(&arg_count.to_le_bytes());
        for arg in &op.args {
            write_len_prefixed(&mut hasher, arg.as_bytes());
        }
    }

    hasher.finalize()
}

/// Diff the keybind sections of two scenes by chord.
///
/// Returns every chord that has an Added / Removed / Changed delta, in
/// a stable order (alphabetical by chord) so the supervisor's batching
/// layer sees a deterministic stream.
pub fn diff_keybinds(old: &SceneDoc, new: &SceneDoc) -> Vec<KeybindChange> {
    let old_map: BTreeMap<&str, &crate::ast::KeybindNode> = old
        .scene
        .keybinds
        .iter()
        .map(|kb| (kb.chord.as_str(), kb))
        .collect();
    let new_map: BTreeMap<&str, &crate::ast::KeybindNode> = new
        .scene
        .keybinds
        .iter()
        .map(|kb| (kb.chord.as_str(), kb))
        .collect();

    let mut changes = Vec::new();

    // Every chord in old ∪ new, sorted by chord for determinism.
    let mut all_chords: BTreeSet<&str> = BTreeSet::new();
    all_chords.extend(old_map.keys().copied());
    all_chords.extend(new_map.keys().copied());

    for chord in all_chords {
        match (old_map.get(chord), new_map.get(chord)) {
            (None, Some(_)) => changes.push(KeybindChange {
                chord: chord.to_string(),
                kind: KeybindChangeKind::Added,
            }),
            (Some(_), None) => changes.push(KeybindChange {
                chord: chord.to_string(),
                kind: KeybindChangeKind::Removed,
            }),
            (Some(old_kb), Some(new_kb)) => {
                if keybind_body_hash(old_kb) != keybind_body_hash(new_kb) {
                    changes.push(KeybindChange {
                        chord: chord.to_string(),
                        kind: KeybindChangeKind::Changed,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }

    changes
}

// ---------------------------------------------------------------------------
// Plugin lifecycle diff (T-11.4)
// ---------------------------------------------------------------------------

/// Per-plugin change classification.
///
/// The four cases correspond directly to the T-11.4 spec:
///
/// * `Added` — plugin didn't exist; supervisor runs the equivalent of
///   `start-or-reload-plugin`.
/// * `Removed` — plugin exists in old scene, not in new; close it.
/// * `SourceChanged` — same name, different `source`. Restart cartridge;
///   wasm state is lost.
/// * `MountChanged` — same source, different mount target / geometry.
///   Close and relaunch at new target.
/// * `ConfigChanged` — same source and mount, different `config { }`
///   body. Try `reconfigure_plugin` (zellij-tile 0.44+); fall back to
///   close + relaunch if the zellij API isn't available.
/// * `LifecycleChanged` — `summon` <-> always / event-mount transition.
///   Supervisor updates its tracking (possibly dismisses if the new
///   mode is dormant).
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PluginChangeKind {
    /// New plugin declaration.
    Added,
    /// Plugin removed from the scene.
    Removed,
    /// `source` URI differs.
    SourceChanged,
    /// `mount` target or geometry differs.
    MountChanged,
    /// `config { }` body differs (same source + mount).
    ConfigChanged,
    /// Lifecycle (always / summon / event-mount) differs.
    LifecycleChanged,
}

/// One entry in the plugin diff.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PluginChange {
    /// Plugin name (the primary key — plugin names must be unique
    /// within a scene).
    pub name: String,

    /// Kind of change.
    pub kind: PluginChangeKind,
}

fn plugin_source_hash(p: &PluginNode) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    if let Some(src) = &p.source {
        write_len_prefixed(&mut h, src.uri.as_bytes());
    } else {
        h.update(&[0u8]);
    }
    h.finalize()
}

fn plugin_mount_hash(p: &PluginNode) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    if let Some(mount) = &p.mount {
        write_len_prefixed(&mut h, mount.target.as_bytes());
        for field in [
            &mount.into,
            &mount.split,
            &mount.size,
            &mount.x,
            &mount.y,
            &mount.width,
            &mount.height,
        ] {
            match field {
                Some(v) => {
                    h.update(&[1u8]);
                    write_len_prefixed(&mut h, v.as_bytes());
                }
                None => {
                    h.update(&[0u8]);
                }
            }
        }
    } else {
        h.update(&[0u8]);
    }
    h.finalize()
}

fn plugin_config_hash(p: &PluginNode) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    match &p.config {
        Some(cfg) => {
            h.update(&[1u8]);
            let n = cfg.args.len() as u32;
            h.update(&n.to_le_bytes());
            for a in &cfg.args {
                write_len_prefixed(&mut h, a.as_bytes());
            }
        }
        None => {
            h.update(&[0u8]);
        }
    }
    h.finalize()
}

/// Stable lifecycle summary string — we avoid `crate::plugin::lower_plugin`
/// here so the diff layer works on the raw AST (the lowering pass may
/// fail on an ambiguous plugin, and a failed lowering shouldn't prevent
/// the diff from surfacing a "lifecycle changed" signal). The summary is
/// `summon=<bool>|on=<count>` — textually distinct for each of the three
/// R6 lifecycle cases + the ambiguous case.
fn plugin_lifecycle_summary(p: &PluginNode) -> String {
    format!(
        "summon={}|on={}",
        p.summon.is_some() as u8,
        p.on.len(),
    )
}

/// Diff the plugin sections of two scenes by name.
pub fn diff_plugins(old: &SceneDoc, new: &SceneDoc) -> Vec<PluginChange> {
    let old_map: BTreeMap<&str, &PluginNode> = old
        .scene
        .plugins
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let new_map: BTreeMap<&str, &PluginNode> = new
        .scene
        .plugins
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(old_map.keys().copied());
    all_names.extend(new_map.keys().copied());

    let mut changes = Vec::new();
    for name in all_names {
        match (old_map.get(name), new_map.get(name)) {
            (None, Some(_)) => changes.push(PluginChange {
                name: name.to_string(),
                kind: PluginChangeKind::Added,
            }),
            (Some(_), None) => changes.push(PluginChange {
                name: name.to_string(),
                kind: PluginChangeKind::Removed,
            }),
            (Some(old_p), Some(new_p)) => {
                // Classify in priority order (most severe first so one
                // plugin emits one PluginChange — the supervisor picks
                // the heaviest operation).
                let kind = if plugin_source_hash(old_p) != plugin_source_hash(new_p) {
                    Some(PluginChangeKind::SourceChanged)
                } else if plugin_mount_hash(old_p) != plugin_mount_hash(new_p) {
                    Some(PluginChangeKind::MountChanged)
                } else if plugin_lifecycle_summary(old_p)
                    != plugin_lifecycle_summary(new_p)
                {
                    Some(PluginChangeKind::LifecycleChanged)
                } else if plugin_config_hash(old_p) != plugin_config_hash(new_p) {
                    Some(PluginChangeKind::ConfigChanged)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    changes.push(PluginChange {
                        name: name.to_string(),
                        kind,
                    });
                }
            }
            (None, None) => unreachable!(),
        }
    }

    changes
}

// ---------------------------------------------------------------------------
// Layout diff (T-11.5)
// ---------------------------------------------------------------------------

/// Coarse structural hash of a pane. For v0.1 we compare the entire
/// subtree; any nested change bubbles up.
fn pane_hash(p: &PaneNode) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    for field in [
        &p.when,
        &p.name,
        &p.command,
        &p.size,
        &p.split_direction,
        &p.cwd,
    ] {
        match field {
            Some(v) => {
                h.update(&[1u8]);
                write_len_prefixed(&mut h, v.as_bytes());
            }
            None => {
                h.update(&[0u8]);
            }
        }
    }
    // focus
    match p.focus {
        Some(true) => {
            h.update(&[2u8]);
        }
        Some(false) => {
            h.update(&[1u8]);
        }
        None => {
            h.update(&[0u8]);
        }
    };
    // nested panes
    let n = p.panes.len() as u32;
    h.update(&n.to_le_bytes());
    for nested in &p.panes {
        let nh = pane_hash(nested);
        h.update(nh.as_bytes());
    }
    h.finalize()
}

fn tab_hash(t: &TabNode) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    match &t.name {
        Some(n) => {
            h.update(&[1u8]);
            write_len_prefixed(&mut h, n.as_bytes());
        }
        None => {
            h.update(&[0u8]);
        }
    }
    match &t.when {
        Some(w) => {
            h.update(&[1u8]);
            write_len_prefixed(&mut h, w.as_bytes());
        }
        None => {
            h.update(&[0u8]);
        }
    }
    match t.focus {
        Some(true) => {
            h.update(&[2u8]);
        }
        Some(false) => {
            h.update(&[1u8]);
        }
        None => {
            h.update(&[0u8]);
        }
    };
    let n = t.panes.len() as u32;
    h.update(&n.to_le_bytes());
    for p in &t.panes {
        h.update(pane_hash(p).as_bytes());
    }
    h.finalize()
}

fn layout_hash(l: Option<&LayoutNode>) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    match l {
        None => {
            h.update(&[0u8]);
        }
        Some(layout) => {
            h.update(&[1u8]);
            let n = layout.tabs.len() as u32;
            h.update(&n.to_le_bytes());
            for t in &layout.tabs {
                h.update(tab_hash(t).as_bytes());
            }
            let np = layout.panes.len() as u32;
            h.update(&np.to_le_bytes());
            for p in &layout.panes {
                h.update(pane_hash(p).as_bytes());
            }
        }
    }
    h.finalize()
}

/// Whether the layout changed structurally between two scenes.
///
/// v0.1 is deliberately coarse: any edit inside the `layout { }` block
/// flips this to `true`. Fine-grained pane diffs are v0.2+ work
/// (tracked in the T-11.5 spec).
pub fn diff_layout_changed(old: &SceneDoc, new: &SceneDoc) -> bool {
    layout_hash(old.scene.layout.as_ref()) != layout_hash(new.scene.layout.as_ref())
}

// ---------------------------------------------------------------------------
// SceneDiff + telemetry
// ---------------------------------------------------------------------------

/// Summary of what changed between two compiled scenes.
///
/// The coarse `*_count` fields (carried over from T-11.1) stay for
/// backwards-compat with existing tests + callers. The finer diff
/// information from T-11.2 — T-11.5 lives on the `reactions_added`,
/// `reactions_removed`, `keybind_changes`, `plugin_changes`, and
/// `layout_changed` fields.
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

    /// T-11.2: reactions added (structural hash present in new but not
    /// old).
    pub reactions_added: usize,
    /// T-11.2: reactions removed (structural hash present in old but
    /// not new).
    pub reactions_removed: usize,
    /// T-11.3: per-chord change list.
    pub keybind_changes: Vec<KeybindChange>,
    /// T-11.4: per-plugin change list.
    pub plugin_changes: Vec<PluginChange>,
    /// T-11.5: any structural layout change.
    pub layout_changed: bool,
}

impl SceneDiff {
    /// Whether the diff indicates any semantic change.
    ///
    /// Uses the hash-based signals (T-11.2 — T-11.5), not the raw
    /// counts — because a reaction can change without the count
    /// changing.
    pub fn has_changes(&self) -> bool {
        self.reactions_added > 0
            || self.reactions_removed > 0
            || !self.keybind_changes.is_empty()
            || !self.plugin_changes.is_empty()
            || self.layout_changed
    }
}

/// Status of a completed reload attempt — surfaced on
/// [`ReloadTelemetry`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReloadStatus {
    /// Every stage applied cleanly.
    Ok,
    /// Some stages applied, then a stage aborted (T-11.5 layout-skip
    /// path also surfaces as Partial), or a later stage failed — prior
    /// stages remain applied.
    Partial,
    /// The reload failed BEFORE any stage ran (parse / compile
    /// failure). Old config retained.
    Failed,
}

impl ReloadStatus {
    /// Wire-format string for the `status` field of the
    /// `UserEvent:ark.scene.reloaded` payload.
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadStatus::Ok => "ok",
            ReloadStatus::Partial => "partial",
            ReloadStatus::Failed => "failed",
        }
    }
}

/// Telemetry record emitted for every completed reload (T-11.8).
///
/// The caller (supervisor) turns this into a
/// `UserEvent:ark.scene.reloaded { … }` broadcast + logs it against
/// the `scene::reload` tracing target.
#[derive(Debug, Clone)]
pub struct ReloadTelemetry {
    /// Wall-clock duration of the reload (parse → swap → diff →
    /// stages). Measured in milliseconds; callers that need
    /// sub-millisecond accuracy should re-instrument.
    pub duration_ms: u64,
    /// T-11.2: reaction hashes present in new but not old.
    pub reactions_added: usize,
    /// T-11.2: reaction hashes present in old but not new.
    pub reactions_removed: usize,
    /// T-11.3: total count of Added + Removed + Changed keybinds.
    pub keybinds_changed: usize,
    /// T-11.4: total count of plugin changes (any kind).
    pub plugins_changed: usize,
    /// Overall status: ok / partial / failed.
    pub status: ReloadStatus,
    /// Stage name where the reload aborted, when `status == Partial`
    /// or `Failed`. `None` on clean `Ok`.
    pub failed_stage: Option<String>,
}

// ---------------------------------------------------------------------------
// ReloadOutcome
// ---------------------------------------------------------------------------

/// Result of a reload attempt.
#[derive(Debug)]
pub enum ReloadOutcome {
    /// Reload completed; deltas were applied (possibly partially).
    Applied {
        /// What changed between old and new.
        diff: SceneDiff,
        /// Telemetry for the `UserEvent:ark.scene.reloaded` emit.
        telemetry: ReloadTelemetry,
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
        /// Telemetry for the `UserEvent:ark.scene.reloaded` emit
        /// (status = failed, stage = parse|compile).
        telemetry: ReloadTelemetry,
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
    ///
    /// Implements the T-11.7 staged recovery:
    ///
    /// 1. re-parse + validate the scene (fails BEFORE any stage → keep
    ///    old config fully, status = Failed, stage = "parse" or
    ///    "compile").
    /// 2. build a new registry (compile reactions) — same bucket as
    ///    stage 1 failure-mode.
    /// 3. compute the diff (pure; cannot fail).
    /// 4. apply stages in safety order: reactions → keybinds → plugins
    ///    → layout. Per stage, the registry / ast swap is atomic; a
    ///    per-stage failure ABORTS later stages but leaves earlier
    ///    stages applied.
    ///
    /// At T-11.x tier the per-stage apply step is the atomic
    /// `Arc`-swap of the registry and document. Each stage is
    /// intrinsically infallible at this tier (the work is memory-only
    /// — the I/O-heavy lifecycle ops live in the supervisor, which
    /// consumes the returned diff). The staged abort machinery is
    /// therefore a skeleton that future stages (supervisor-side
    /// plugin relaunch, ark-bus rebind) will populate.
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

        let started = Instant::now();

        // --- Stage 0a: re-parse scene file from disk ---
        let new_doc = match self.reparse_scene() {
            Ok(doc) => doc,
            Err(e) => {
                tracing::error!(
                    target: "scene::reload",
                    path = %self.scene_path.display(),
                    error = %e,
                    "reload_scene failed: parse/validate error; keeping old config"
                );
                let telemetry = ReloadTelemetry {
                    duration_ms: started.elapsed().as_millis() as u64,
                    reactions_added: 0,
                    reactions_removed: 0,
                    keybinds_changed: 0,
                    plugins_changed: 0,
                    status: ReloadStatus::Failed,
                    failed_stage: Some("parse".to_string()),
                };
                return ReloadOutcome::Failed {
                    error: e.to_string(),
                    telemetry,
                };
            }
        };

        // --- Stage 0b: build new registry ---
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
                let telemetry = ReloadTelemetry {
                    duration_ms: started.elapsed().as_millis() as u64,
                    reactions_added: 0,
                    reactions_removed: 0,
                    keybinds_changed: 0,
                    plugins_changed: 0,
                    status: ReloadStatus::Failed,
                    failed_stage: Some("compile".to_string()),
                };
                return ReloadOutcome::Failed {
                    error: joined,
                    telemetry,
                };
            }
        };

        // --- Compute diff (pure; cannot fail) ---
        let diff = self.compute_diff(&new_doc, &new_registry);

        if !diff.has_changes() {
            tracing::debug!(
                target: "scene::reload",
                path = %self.scene_path.display(),
                "reload_scene: no changes detected, skipping swap"
            );
        }

        // --- Stages 1-4 apply ---
        //
        // Each stage is wrapped so we can short-circuit on failure
        // while retaining earlier stages' application. At this tier
        // the only fallible operation is the mutex acquire (poisoning
        // is a bug we surface as Partial), and the layout stage is
        // intentionally SKIPPED per T-11.5 — layout changes require a
        // session restart in v0.1.

        let mut status = ReloadStatus::Ok;
        let mut failed_stage: Option<String> = None;

        // Stage 1: reactions (atomic registry swap).
        match self.apply_reactions_stage(new_registry) {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(
                    target: "scene::reload",
                    stage = "reactions",
                    error = %e,
                    "reload_scene: reactions stage failed; later stages aborted"
                );
                status = ReloadStatus::Partial;
                failed_stage = Some("reactions".to_string());
            }
        }

        // Stage 2: keybinds. The diff is surfaced; the ark-bus rebind
        // lives in the supervisor. Within-crate this is a noop — the
        // `KeybindChange` vec on the returned `SceneDiff` is the
        // contract.
        if status == ReloadStatus::Ok {
            // Nothing to apply locally; the supervisor consumes
            // `diff.keybind_changes` and emits `rebind_keys`.
        }

        // Stage 3: plugins. Same contract — `diff.plugin_changes` is
        // consumed by the supervisor for `start-or-reload-plugin` /
        // close + relaunch / reconfigure.
        if status == ReloadStatus::Ok {
            // No-op locally.
        }

        // Stage 4: layout — conservative skip (T-11.5).
        if status == ReloadStatus::Ok && diff.layout_changed {
            tracing::info!(
                target: "scene::reload",
                path = %self.scene_path.display(),
                "reload_scene: layout changed — skipping layout update (v0.1); session restart required"
            );
            status = ReloadStatus::Partial;
            failed_stage = Some("layout".to_string());
        }

        // Atomic doc swap — the reload commits the new AST so future
        // reloads diff against it. Done after the reactions stage so
        // the diff consistency (registry + doc) holds.
        //
        // NOTE: we swap the doc even when the layout stage "failed"
        // because layout failure is a conservative skip, not an
        // abort — the next reload should still diff against the now-
        // current-on-disk AST, since the session will eventually
        // restart and pick up the layout then. If we retained the old
        // doc, the next reload would re-diff the layout change and
        // re-emit the partial signal on every cycle.
        if status != ReloadStatus::Failed {
            let mut guard = self.current_doc.lock().expect("doc mutex poisoned");
            *guard = new_doc;
        }

        let telemetry = ReloadTelemetry {
            duration_ms: started.elapsed().as_millis() as u64,
            reactions_added: diff.reactions_added,
            reactions_removed: diff.reactions_removed,
            keybinds_changed: diff.keybind_changes.len(),
            plugins_changed: diff.plugin_changes.len(),
            status,
            failed_stage: failed_stage.clone(),
        };

        tracing::info!(
            target: "scene::reload",
            path = %self.scene_path.display(),
            duration_ms = telemetry.duration_ms,
            reactions_added = telemetry.reactions_added,
            reactions_removed = telemetry.reactions_removed,
            keybinds_changed = telemetry.keybinds_changed,
            plugins_changed = telemetry.plugins_changed,
            status = status.as_str(),
            failed_stage = ?failed_stage,
            "reload_scene: telemetry"
        );

        ReloadOutcome::Applied { diff, telemetry }
    }

    /// Stage 1 — atomic registry swap.
    ///
    /// Infallible today (the only failure mode is mutex poisoning,
    /// which we surface as an `Err` so the staged-recovery machinery
    /// in `reload_inner` has a real failure path to exercise).
    fn apply_reactions_stage(
        &self,
        new_registry: ReactionRegistry,
    ) -> Result<(), String> {
        let mut guard = self
            .current_registry
            .lock()
            .map_err(|e| format!("registry mutex poisoned: {e}"))?;
        *guard = Arc::new(new_registry);
        Ok(())
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

    /// Compute the T-11.2 — T-11.5 diff between the current live state
    /// and the new parsed scene. Pure function of the old registry /
    /// old doc / new registry / new doc.
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

        let (reactions_added, reactions_removed) =
            diff_reactions(&old_registry, new_registry);
        let keybind_changes = diff_keybinds(&old_doc, new_doc);
        let plugin_changes = diff_plugins(&old_doc, new_doc);
        let layout_changed = diff_layout_changed(&old_doc, new_doc);

        SceneDiff {
            old_reaction_count: old_registry.len(),
            new_reaction_count: new_registry.len(),
            old_keybind_count: old_doc.scene.keybinds.len(),
            new_keybind_count: new_doc.scene.keybinds.len(),
            old_plugin_count: old_doc.scene.plugins.len(),
            new_plugin_count: new_doc.scene.plugins.len(),
            reactions_added,
            reactions_removed,
            keybind_changes,
            plugin_changes,
            layout_changed,
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
            ReloadOutcome::Applied { diff, telemetry } => {
                assert_eq!(diff.old_reaction_count, initial_count);
                assert!(
                    diff.new_reaction_count > initial_count,
                    "new registry should have more reactions: old={}, new={}",
                    initial_count,
                    diff.new_reaction_count
                );
                // T-11.8: telemetry is populated.
                assert_eq!(telemetry.status, ReloadStatus::Ok);
                assert!(telemetry.reactions_added >= 1);
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
        match &outcome {
            ReloadOutcome::Failed { telemetry, .. } => {
                assert_eq!(telemetry.status, ReloadStatus::Failed);
                assert_eq!(telemetry.failed_stage.as_deref(), Some("parse"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }

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

    // -- T-11.2: reaction hashing + diff ---------------------------------

    #[test]
    fn reaction_hash_is_stable_for_comment_only_edits() {
        let v_no_comment = r#"scene "demo" {
    on "Started" { }
}"#;
        let v_with_comment = r#"scene "demo" {
    // leading comment
    on "Started" { } // trailing
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v_no_comment).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v_with_comment).expect("parse");
        let r1 = populate_registry(&d1).expect("populate");
        let r2 = populate_registry(&d2).expect("populate");

        let (added, removed) = diff_reactions(&r1, &r2);
        assert_eq!(added, 0, "comment-only edits should not add reactions");
        assert_eq!(removed, 0, "comment-only edits should not remove reactions");
    }

    #[test]
    fn reaction_hash_is_stable_for_whitespace_only_edits() {
        let v_tight = r#"scene "demo" { on "Started" { } }"#;
        let v_loose = r#"scene "demo" {

    on       "Started"    {
    }

}"#;
        let d1: SceneDoc = facet_kdl::from_str(v_tight).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v_loose).expect("parse");
        let r1 = populate_registry(&d1).expect("populate");
        let r2 = populate_registry(&d2).expect("populate");

        let (added, removed) = diff_reactions(&r1, &r2);
        assert_eq!(added, 0, "whitespace-only edits should not add reactions");
        assert_eq!(removed, 0, "whitespace-only edits should not remove reactions");
    }

    #[test]
    fn reaction_hash_detects_added_reaction() {
        let v1 = r#"scene "demo" {
    on "Started" { }
}"#;
        let v2 = r#"scene "demo" {
    on "Started" { }
    on "Done" { }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let r1 = populate_registry(&d1).expect("populate");
        let r2 = populate_registry(&d2).expect("populate");

        let (added, removed) = diff_reactions(&r1, &r2);
        assert_eq!(added, 1);
        assert_eq!(removed, 0);
    }

    #[test]
    fn reaction_hash_detects_removed_reaction() {
        let v1 = r#"scene "demo" {
    on "Started" { }
    on "Done" { }
}"#;
        let v2 = r#"scene "demo" {
    on "Started" { }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let r1 = populate_registry(&d1).expect("populate");
        let r2 = populate_registry(&d2).expect("populate");

        let (added, removed) = diff_reactions(&r1, &r2);
        assert_eq!(added, 0);
        assert_eq!(removed, 1);
    }

    #[test]
    fn reaction_hash_detects_predicate_change() {
        let v1 = r#"scene "demo" {
    on "Started" if="true" { }
}"#;
        let v2 = r#"scene "demo" {
    on "Started" if="false" { }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let r1 = populate_registry(&d1).expect("populate");
        let r2 = populate_registry(&d2).expect("populate");

        let (added, removed) = diff_reactions(&r1, &r2);
        // Exactly one reaction was "changed" — surfaced as one add + one remove.
        assert_eq!(added, 1);
        assert_eq!(removed, 1);
    }

    // -- T-11.3: keybind diff --------------------------------------------

    #[test]
    fn keybind_diff_detects_add_remove_change() {
        let v1 = r#"scene "demo" {
    keybind "Alt p" intent="picker.show"
    keybind "Ctrl g" intent="git.status"
}"#;
        let v2 = r#"scene "demo" {
    keybind "Alt p" intent="picker.show"
    keybind "Ctrl h" intent="hello.world"
    keybind "Ctrl g" intent="git.log"
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_keybinds(&d1, &d2);

        // Expect: Alt p unchanged, Ctrl g Changed, Ctrl h Added.
        let by_chord: BTreeMap<&str, &KeybindChangeKind> = changes
            .iter()
            .map(|c| (c.chord.as_str(), &c.kind))
            .collect();
        assert_eq!(changes.len(), 2, "got {changes:?}");
        assert_eq!(by_chord.get("Ctrl g"), Some(&&KeybindChangeKind::Changed));
        assert_eq!(by_chord.get("Ctrl h"), Some(&&KeybindChangeKind::Added));
    }

    #[test]
    fn keybind_diff_detects_remove() {
        let v1 = r#"scene "demo" {
    keybind "Alt p" intent="picker.show"
    keybind "Ctrl g" intent="git.status"
}"#;
        let v2 = r#"scene "demo" {
    keybind "Alt p" intent="picker.show"
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_keybinds(&d1, &d2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].chord, "Ctrl g");
        assert_eq!(changes[0].kind, KeybindChangeKind::Removed);
    }

    // -- T-11.4: plugin lifecycle diff -----------------------------------

    #[test]
    fn plugin_diff_detects_added_and_removed() {
        let v1 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}"#;
        let v2 = r#"scene "demo" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_plugins(&d1, &d2);
        let by_name: BTreeMap<&str, &PluginChangeKind> =
            changes.iter().map(|c| (c.name.as_str(), &c.kind)).collect();

        assert_eq!(by_name.get("picker"), Some(&&PluginChangeKind::Removed));
        assert_eq!(by_name.get("status"), Some(&&PluginChangeKind::Added));
    }

    #[test]
    fn plugin_diff_detects_source_changed() {
        let v1 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}"#;
        let v2 = r#"scene "demo" {
    plugin "picker" {
        source "file:/tmp/picker.wasm"
        mount "floating"
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_plugins(&d1, &d2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "picker");
        assert_eq!(changes[0].kind, PluginChangeKind::SourceChanged);
    }

    #[test]
    fn plugin_diff_detects_mount_changed() {
        let v1 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}"#;
        let v2 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating" width="80"
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_plugins(&d1, &d2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "picker");
        assert_eq!(changes[0].kind, PluginChangeKind::MountChanged);
    }

    #[test]
    fn plugin_diff_detects_lifecycle_changed() {
        let v1 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}"#;
        let v2 = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        let changes = diff_plugins(&d1, &d2);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "picker");
        assert_eq!(changes[0].kind, PluginChangeKind::LifecycleChanged);
    }

    // -- T-11.5: layout diff ---------------------------------------------

    #[test]
    fn layout_diff_flags_tab_added() {
        let v1 = r#"scene "demo" {
    layout {
        tab "work"
    }
}"#;
        let v2 = r#"scene "demo" {
    layout {
        tab "work"
        tab "logs"
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v1).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v2).expect("parse");
        assert!(diff_layout_changed(&d1, &d2));
    }

    #[test]
    fn layout_diff_stable_for_identical_layouts() {
        let v = r#"scene "demo" {
    layout {
        tab "work" {
            pane name="editor"
        }
    }
}"#;
        let d1: SceneDoc = facet_kdl::from_str(v).expect("parse");
        let d2: SceneDoc = facet_kdl::from_str(v).expect("parse");
        assert!(!diff_layout_changed(&d1, &d2));
    }

    #[test]
    fn layout_diff_flags_partial_on_reload() {
        let v1 = r#"scene "demo" {
    layout {
        tab "work"
    }
}"#;
        let v2 = r#"scene "demo" {
    layout {
        tab "work"
        tab "logs"
    }
}"#;
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", v1);
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        std::fs::write(&path, v2).expect("overwrite");
        let outcome = reloader.reload(|| None);
        match &outcome {
            ReloadOutcome::Applied { diff, telemetry } => {
                assert!(diff.layout_changed);
                assert_eq!(telemetry.status, ReloadStatus::Partial);
                assert_eq!(telemetry.failed_stage.as_deref(), Some("layout"));
            }
            other => panic!("expected Applied (partial), got {other:?}"),
        }
    }

    // -- T-11.8: telemetry -----------------------------------------------

    #[test]
    fn telemetry_counts_reactions_added() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        std::fs::write(&path, SCENE_V2).expect("overwrite");
        let outcome = reloader.reload(|| None);
        match outcome {
            ReloadOutcome::Applied { telemetry, .. } => {
                assert_eq!(telemetry.status, ReloadStatus::Ok);
                assert_eq!(telemetry.reactions_added, 1);
                assert_eq!(telemetry.reactions_removed, 0);
                assert_eq!(telemetry.keybinds_changed, 0);
                assert_eq!(telemetry.plugins_changed, 0);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_records_failed_stage_for_parse_error() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        std::fs::write(&path, SCENE_BROKEN).expect("overwrite");
        match reloader.reload(|| None) {
            ReloadOutcome::Failed { telemetry, .. } => {
                assert_eq!(telemetry.status, ReloadStatus::Failed);
                assert_eq!(telemetry.failed_stage.as_deref(), Some("parse"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // -- T-11.7: reload failure recovery ---------------------------------

    #[test]
    fn reload_failure_keeps_old_doc_and_registry() {
        let tmp = tempdir();
        let (path, doc, registry) = write_scene(tmp.path(), "scene.kdl", SCENE_V1);
        let initial_len = registry.len();
        let reloader = SceneReloader::new(path.clone(), Arc::new(registry), doc);

        // Break the file.
        std::fs::write(&path, SCENE_BROKEN).expect("overwrite");
        let _ = reloader.reload(|| None);

        // Registry is still the old one.
        assert_eq!(reloader.current_registry().len(), initial_len);

        // Fix the file → reload should work and pick up v2.
        std::fs::write(&path, SCENE_V2).expect("overwrite");
        match reloader.reload(|| None) {
            ReloadOutcome::Applied { telemetry, .. } => {
                assert_eq!(telemetry.status, ReloadStatus::Ok);
            }
            other => panic!("expected Applied after fix, got {other:?}"),
        }
    }
}
