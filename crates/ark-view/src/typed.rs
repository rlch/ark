//! Typed handle wrappers. `Pane<V>` and `Stack<V>` wrap an opaque
//! [`HandleId`] with a `PhantomData<V>` marker so the compiler can
//! route typed affordance methods without bloating the wire payload
//! (see R5: V is compile-time-only, never on the wire).
//!
//! `TabHandle` has no view type — tabs carry panes, not renderers.
//!
//! Per cavekit-soul-phase-2-ark-view.md R4 + R5 and scene R17.

use crate::handle::HandleId;
use crate::view::View;
use std::marker::PhantomData;

/// Typed wrapper around a pane handle.
///
/// `V` is the view type rendered by this pane. The parameter is
/// compile-time-only: on the wire, `Pane<V>` serialises as the plain
/// `HandleId` string (R5). Construction is intentionally crate-private
/// — extensions receive `Pane<V>` values from `SessionHandles` /
/// `Stack::spawn_pane` return values, not by hand.
pub struct Pane<V: View> {
    handle: HandleId,
    // `PhantomData<fn() -> V>` is contravariant and always Send+Sync,
    // regardless of whether V itself is Send/Sync — the struct never
    // actually owns a V, so the marker must not impose ownership
    // variance. This is the standard idiom for compile-time-only type
    // parameters.
    _marker: PhantomData<fn() -> V>,
}

impl<V: View> Pane<V> {
    /// Crate-private constructor. Ark wiring (spawn paths, the
    /// name-lookup accessor, deserialisation) goes through this entry;
    /// extensions never construct directly.
    pub(crate) fn from_handle(handle: HandleId) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    /// Opaque handle id. Inherent accessor — same handle bytes that
    /// appear on the wire.
    pub fn handle(&self) -> &HandleId {
        &self.handle
    }
}

// Manual `Clone`/`Debug` impls (not `derive`): the derive macro would
// synthesise `where V: Clone` / `where V: Debug` bounds that are
// unnecessary — `PhantomData<fn() -> V>` is always Clone+Debug
// regardless of V, and we don't actually own a V.
impl<V: View> Clone for Pane<V> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            _marker: PhantomData,
        }
    }
}

impl<V: View> std::fmt::Debug for Pane<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane").field("handle", &self.handle).finish()
    }
}

impl<V: View> serde::Serialize for Pane<V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.handle.serialize(s)
    }
}

impl<'de, V: View> serde::Deserialize<'de> for Pane<V> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        HandleId::deserialize(d).map(Self::from_handle)
    }
}

/// Typed wrapper around a stack handle — a dynamic container of
/// same-view-type panes. See `impl<V: View> Stack<V>` (T-011) for
/// spawn/close/clear methods.
pub struct Stack<V: View> {
    handle: HandleId,
    _marker: PhantomData<fn() -> V>,
}

impl<V: View> Stack<V> {
    /// Crate-private constructor. See `Pane::from_handle` for rationale.
    pub(crate) fn from_handle(handle: HandleId) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    /// Opaque handle id.
    pub fn handle(&self) -> &HandleId {
        &self.handle
    }
}

// Manual Clone/Debug — see Pane<V> above for rationale.
impl<V: View> Clone for Stack<V> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            _marker: PhantomData,
        }
    }
}

impl<V: View> std::fmt::Debug for Stack<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stack").field("handle", &self.handle).finish()
    }
}

impl<V: View> serde::Serialize for Stack<V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.handle.serialize(s)
    }
}

impl<'de, V: View> serde::Deserialize<'de> for Stack<V> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        HandleId::deserialize(d).map(Self::from_handle)
    }
}

/// Non-parametric tab handle. Tabs hold panes; they have no view type.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TabHandle(HandleId);

impl TabHandle {
    /// Crate-private constructor. See `Pane::from_handle` for rationale.
    pub(crate) fn from_handle(handle: HandleId) -> Self {
        Self(handle)
    }

    /// Opaque handle id.
    pub fn handle(&self) -> &HandleId {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Test-only view types. `VX` and `VY` are distinct so we can prove
    // V is purely compile-time (different V's produce identical wire).
    struct VX;
    impl crate::view::View for VX {}

    struct VY;
    impl crate::view::View for VY {}

    // ---- Pane ---------------------------------------------------------------

    #[test]
    fn pane_serialises_as_plain_handle_string() {
        let p = Pane::<VX>::from_handle(HandleId::new("abc-123"));
        assert_eq!(serde_json::to_value(&p).unwrap(), json!("abc-123"));
    }

    #[test]
    fn pane_deserialises_from_plain_string() {
        let p: Pane<VX> = serde_json::from_str("\"abc-123\"").unwrap();
        assert_eq!(p.handle().as_str(), "abc-123");
    }

    // ---- Stack --------------------------------------------------------------

    #[test]
    fn stack_serialises_as_plain_handle_string() {
        let s = Stack::<VX>::from_handle(HandleId::new("stack-42"));
        assert_eq!(serde_json::to_value(&s).unwrap(), json!("stack-42"));
    }

    #[test]
    fn stack_deserialises_from_plain_string() {
        let s: Stack<VX> = serde_json::from_str("\"stack-42\"").unwrap();
        assert_eq!(s.handle().as_str(), "stack-42");
    }

    // ---- TabHandle ----------------------------------------------------------

    #[test]
    fn tab_handle_serialises_as_plain_handle_string() {
        let t = TabHandle::from_handle(HandleId::new("tab-7"));
        assert_eq!(serde_json::to_value(&t).unwrap(), json!("tab-7"));
    }

    #[test]
    fn tab_handle_deserialises_from_plain_string() {
        let t: TabHandle = serde_json::from_str("\"tab-7\"").unwrap();
        assert_eq!(t.handle().as_str(), "tab-7");
    }

    // ---- R5 invariant: V is not on the wire --------------------------------

    #[test]
    fn pane_roundtrip_different_view_types_interchangeable_wire() {
        // Serialise Pane<VX>, deserialise as Pane<VY>. If V were on the
        // wire this would fail; R5 says V is compile-time-only, so both
        // round-trips succeed with identical handle bytes.
        let p_x = Pane::<VX>::from_handle(HandleId::new("shared-id"));
        let wire = serde_json::to_string(&p_x).unwrap();
        let p_y: Pane<VY> = serde_json::from_str(&wire).unwrap();
        assert_eq!(p_x.handle().as_str(), "shared-id");
        assert_eq!(p_y.handle().as_str(), "shared-id");
    }

    // ---- Derives / auto-traits ---------------------------------------------

    #[test]
    fn pane_is_clone_and_debug() {
        let p = Pane::<VX>::from_handle(HandleId::new("clone-me"));
        let p2 = p.clone();
        assert_eq!(p.handle().as_str(), p2.handle().as_str());
        // Debug compile-check.
        let _ = format!("{:?}", p);
    }

    #[test]
    fn stack_is_clone_and_debug() {
        let s = Stack::<VX>::from_handle(HandleId::new("clone-me"));
        let s2 = s.clone();
        assert_eq!(s.handle().as_str(), s2.handle().as_str());
        let _ = format!("{:?}", s);
    }

    #[test]
    fn tab_handle_is_clone_and_debug() {
        let t = TabHandle::from_handle(HandleId::new("clone-me"));
        let t2 = t.clone();
        assert_eq!(t.handle().as_str(), t2.handle().as_str());
        let _ = format!("{:?}", t);
    }

    #[test]
    fn pane_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Pane<VX>>();
    }

    #[test]
    fn stack_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Stack<VX>>();
    }

    #[test]
    fn tab_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TabHandle>();
    }
}
