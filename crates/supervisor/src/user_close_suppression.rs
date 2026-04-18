//! Session-scoped user-close suppression storage (T-036).
//!
//! Stores `(SceneHandleName, ParamsHash)` pairs whenever the user
//! manually closes a scene-declared pane. Reconciler consults on
//! every tick: absent → spawn; present w/ equal hash → skip spawn;
//! present w/ differing hash → evict + spawn. Stack-children never
//! enter the map (SuppressionPolicy enforces via debug_assert).
//!
//! Per cavekit-soul-phase-2-ark-view.md R8 + R9 + host-dispatch R9.
//!
//! ## Scope of T-036
//!
//! This module owns the STORAGE and the pure `consult` API only.
//! The zellij-pane-close delta detection and the
//! `ark.handle.invalidated { cause: user_closed }` emission are
//! integration wiring details handled by a later tier alongside
//! real zellij eventing. Callers of this module compute the
//! required inputs (SceneHandleName, ParamsHash, is_stack_child)
//! upstream.

use ark_view::{ParamsHash, SceneHandleName, SuppressionPolicy};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Session-scoped map of closed-by-user scene handles.
///
/// Keyed by `SceneHandleName` (as String via its inner
/// representation). Values are the `ParamsHash` computed at close
/// time; reconciler compares against a fresh hash to decide if
/// params materially changed (differing hash → evict + respawn).
#[derive(Debug, Default)]
pub struct ClosedByUserMap {
    inner: Mutex<BTreeMap<String, ParamsHash>>,
}

impl ClosedByUserMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a user-close. Per kit invariant #5, stack-children are
    /// filtered out by the caller (or via `SuppressionPolicy::
    /// assert_not_stack_child` in debug builds). Passing
    /// `is_stack_child = true` here is a SuppressionPolicy invariant
    /// violation; debug_assert fires and release-builds skip the
    /// write.
    pub fn record(
        &self,
        handle_name: &SceneHandleName,
        params_hash: ParamsHash,
        is_stack_child: bool,
    ) {
        SuppressionPolicy::assert_not_stack_child(is_stack_child, handle_name);
        if is_stack_child {
            return; // release-build: skip the write per R9.
        }
        let mut g = self.inner.lock().expect("closed_by_user mutex poisoned");
        g.insert(handle_name.as_str().to_string(), params_hash);
    }

    /// Look up a suppression entry by scene handle name. Reconciler
    /// uses this: absent → spawn; equal hash → skip; differing →
    /// evict + spawn.
    pub fn lookup(&self, handle_name: &SceneHandleName) -> Option<ParamsHash> {
        let g = self.inner.lock().expect("closed_by_user mutex poisoned");
        g.get(handle_name.as_str()).copied()
    }

    /// Remove an entry (reconciler does this after detecting a
    /// differing hash — "evict then spawn" per invariant #2).
    pub fn evict(&self, handle_name: &SceneHandleName) -> bool {
        let mut g = self.inner.lock().expect("closed_by_user mutex poisoned");
        g.remove(handle_name.as_str()).is_some()
    }

    /// Clear all entries. Called on supervisor session boundary.
    pub fn clear(&self) {
        self.inner
            .lock()
            .expect("closed_by_user mutex poisoned")
            .clear();
    }

    /// Count of currently-suppressed handles. Informational.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("closed_by_user mutex poisoned")
            .len()
    }

    /// True when no suppressions recorded.
    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .expect("closed_by_user mutex poisoned")
            .is_empty()
    }
}

/// Reconciler decision after consulting the suppression map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnDecision {
    /// No suppression recorded — spawn the pane.
    Spawn,
    /// Suppression matches current params — skip the spawn.
    Skip,
    /// Suppression exists but params changed — evict + spawn.
    EvictAndSpawn,
}

/// Reconciler's consult helper. Given a handle name + current params
/// hash, return the decision. Per invariant #2.
///
/// Eviction is a side-effect callers apply after receiving
/// `SpawnDecision::EvictAndSpawn` — this function is pure.
pub fn consult(
    map: &ClosedByUserMap,
    handle_name: &SceneHandleName,
    current_hash: ParamsHash,
) -> SpawnDecision {
    match map.lookup(handle_name) {
        None => SpawnDecision::Spawn,
        Some(recorded) if recorded == current_hash => SpawnDecision::Skip,
        Some(_) => SpawnDecision::EvictAndSpawn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_view::hash_params;

    #[test]
    fn record_and_lookup_roundtrip() {
        let map = ClosedByUserMap::new();
        let name = SceneHandleName::new("editor");
        let hash = hash_params(&"params-v1");
        map.record(&name, hash, false);
        assert_eq!(map.lookup(&name), Some(hash));
    }

    #[test]
    fn lookup_absent_returns_none() {
        let map = ClosedByUserMap::new();
        assert!(map.lookup(&SceneHandleName::new("nope")).is_none());
    }

    #[test]
    fn consult_spawn_when_absent() {
        let map = ClosedByUserMap::new();
        let name = SceneHandleName::new("editor");
        let h = hash_params(&"p");
        assert_eq!(consult(&map, &name, h), SpawnDecision::Spawn);
    }

    #[test]
    fn consult_skip_when_hash_matches() {
        let map = ClosedByUserMap::new();
        let name = SceneHandleName::new("editor");
        let h = hash_params(&"same-params");
        map.record(&name, h, false);
        assert_eq!(consult(&map, &name, h), SpawnDecision::Skip);
    }

    #[test]
    fn consult_evict_when_hash_differs() {
        let map = ClosedByUserMap::new();
        let name = SceneHandleName::new("editor");
        let old = hash_params(&"old");
        let new = hash_params(&"new");
        assert_ne!(old, new);
        map.record(&name, old, false);
        assert_eq!(consult(&map, &name, new), SpawnDecision::EvictAndSpawn);
    }

    #[test]
    fn evict_removes_entry() {
        let map = ClosedByUserMap::new();
        let name = SceneHandleName::new("editor");
        map.record(&name, hash_params(&"p"), false);
        assert!(map.evict(&name));
        assert!(map.lookup(&name).is_none());
    }

    #[test]
    fn evict_returns_false_on_absent() {
        let map = ClosedByUserMap::new();
        assert!(!map.evict(&SceneHandleName::new("nope")));
    }

    #[test]
    fn clear_empties_map() {
        let map = ClosedByUserMap::new();
        map.record(&SceneHandleName::new("a"), hash_params(&1), false);
        map.record(&SceneHandleName::new("b"), hash_params(&2), false);
        assert_eq!(map.len(), 2);
        map.clear();
        assert!(map.is_empty());
    }

    #[test]
    fn session_scoped_fresh_map_empty() {
        // Per kit R9 invariant #3: supervisor restart = new session = empty map.
        // This is trivially satisfied by ClosedByUserMap::default().
        let map = ClosedByUserMap::default();
        assert!(map.is_empty());
    }

    // Debug-only test for SuppressionPolicy invariant #5 enforcement.
    // Under release builds the assertion no-ops (per SuppressionPolicy's
    // debug_assert semantics).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "stack child")]
    fn record_stack_child_debug_panics() {
        let map = ClosedByUserMap::new();
        map.record(&SceneHandleName::new("child"), hash_params(&"p"), true);
    }
}
