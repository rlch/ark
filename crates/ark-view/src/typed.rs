//! Typed handle wrappers. `Pane<V>` and `Stack<V>` wrap an opaque
//! [`HandleId`] with a `PhantomData<V>` marker so the compiler can
//! route typed affordance methods without bloating the wire payload
//! (see R5: V is compile-time-only, never on the wire).
//!
//! `TabHandle` has no view type — tabs carry panes, not renderers.
//!
//! Per cavekit-soul-phase-2-ark-view.md R4 + R5 and scene R17.

use crate::handle::HandleId;
use crate::view::{CommandView, View, ZellijView};
use std::marker::PhantomData;

/// Common surface shared by [`Pane<V>`] and [`Stack<V>`]. Extensions
/// hold typed wrappers for static guarantees; `&dyn PaneLike` lets
/// internal helper code iterate mixed containers polymorphically
/// without reflection.
///
/// v0.1 pins only the `handle()` accessor — the `emit<E: Event>`
/// surface mentioned in kit R4 is deferred until the typed `Event`
/// trait lands with the RPC wiring in T-018+. Additions to this trait
/// are MINOR-compatible because extensions consume `&dyn PaneLike`
/// through inherent methods, not through explicit impls.
pub trait PaneLike {
    /// Opaque handle id — same bytes that appear on the wire.
    fn handle(&self) -> &HandleId;
}

impl<V: View> PaneLike for Pane<V> {
    fn handle(&self) -> &HandleId {
        self.handle()
    }
}

impl<V: View> PaneLike for Stack<V> {
    fn handle(&self) -> &HandleId {
        self.handle()
    }
}

/// **Internal.** Doc-hidden constructor used EXCLUSIVELY by the
/// `tests/ui/` trybuild compile-fail fixtures to assemble a `Pane<V>`
/// from outside the crate — `Pane::from_handle` is `pub(crate)` and
/// the compile-fail fixtures live in an integration test crate that
/// cannot name it. Not part of the public surface; MUST NOT be called
/// from extension code. Unstable across any version.
#[doc(hidden)]
pub fn __trybuild_pane_ctor<V: View>(h: HandleId) -> Pane<V> {
    Pane::from_handle(h)
}

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
        f.debug_struct("Pane")
            .field("handle", &self.handle)
            .finish()
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
        f.debug_struct("Stack")
            .field("handle", &self.handle)
            .finish()
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

// ---- Marker-gated affordances on `Pane<V>` (T-010) ---------------------
//
// Kit R4 requires that `env`, `write_stdin`, `pid` are ONLY in scope
// when `V: CommandView`, and that `pipe` is ONLY in scope when
// `V: ZellijView`. Inherent-impl marker-gating is how Rust encodes
// this at the type system: a `Pane<Z>` where `Z: ZellijView` but not
// `CommandView` cannot name `env(...)` — the method is literally not
// a candidate during method resolution.
//
// Bodies are stubs — the actual RPC invocation is wired in T-018+
// (ext→host method bodies, see cavekit-soul-phase-2-ark-view.md R6
// pane/* + stack/* methods). Stubs are acceptable here because R4's
// acceptance criteria are about TYPE-LEVEL visibility, which the
// signatures lock.

impl<V: CommandView> Pane<V> {
    /// Set an environment variable for the subprocess renderer.
    ///
    /// Maps to the `pane/env` RPC method at T-018+. v0.1 body is a
    /// stub — callers get the correct type-level visibility now so
    /// scene/ext code can compile against the surface before the
    /// dispatcher lands.
    pub fn env(&self, _key: &str, _value: &str) {
        // RPC wiring lands in T-018+ (ext→host method bodies).
    }

    /// Write bytes to the subprocess's stdin stream.
    ///
    /// Maps to `pane/write_stdin` at T-018+. Stub body per T-010.
    pub fn write_stdin(&self, _bytes: &[u8]) {
        // RPC wiring lands in T-018+.
    }

    /// Process id of the subprocess renderer, if alive.
    ///
    /// Maps to `pane/pid` at T-018+. Stub returns `None` until the
    /// dispatcher can ask the supervisor.
    pub fn pid(&self) -> Option<u32> {
        // RPC wiring lands in T-018+; stub returns None.
        None
    }
}

impl<V: ZellijView> Pane<V> {
    /// Pipe a message to the zellij plugin renderer.
    ///
    /// Maps to `pane/pipe` at T-018+. Stub body per T-010.
    pub fn pipe(&self, _message: &[u8]) {
        // RPC wiring lands in T-018+.
    }
}

// ---- `Stack<V>` methods (T-011) ----------------------------------------

/// Attributes passed to [`Stack::spawn_pane`].
///
/// v0.1 is intentionally empty — later tiers (scene wiring, ext
/// authoring) may extend this with `env` overrides, initial view-body
/// payload, etc. Kept as a dedicated struct so additions are
/// MINOR-compatible (per phase-2 decision #4c): appending fields with
/// `Default` values is source-compatible for call sites that build
/// via `PaneAttrs::default()` / struct-literal-with-`..default()`.
#[derive(Clone, Debug, Default)]
pub struct PaneAttrs {
    // Intentionally empty for v0.1 — see doc-comment.
}

impl<V: View> Stack<V> {
    /// Spawn a new pane into this stack.
    ///
    /// Maps to `stack/spawn_pane` at T-018+. T-011 pins the
    /// Rust-side signature; the returned `Pane<V>` carries a synthetic
    /// placeholder handle until the RPC dispatcher is wired — callers
    /// that exercise the returned handle against the host will get
    /// a `HandleGone`-shaped error (R7). This is INTENTIONAL for v0.1
    /// so scene/ext code can compile and unit-test against the surface.
    pub fn spawn_pane(&self, _attrs: PaneAttrs) -> Pane<V> {
        // Placeholder: distinct synthetic handles per call so callers
        // can still distinguish two spawned children by handle before
        // RPC wiring lands. `<stack-handle>-<monotonic>` mirrors R-7's
        // production format (stack-handle-ulid) in spirit. Replaced
        // with real RPC invocation in T-018+.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        Pane::from_handle(HandleId::new(format!(
            "__unwired_spawn__-{}-{n}",
            self.handle.as_str()
        )))
    }

    /// Close a specific child pane without tearing down the stack
    /// itself. Subsequent ops against the closed child produce
    /// `HandleGone` (R7) on the dispatcher side.
    ///
    /// Per R9, stack-children are NEVER entered into the user-close
    /// suppression set — a re-invocation of `spawn_pane` after a
    /// `close_child` is always honoured.
    ///
    /// Maps to `stack/close_child` at T-018+. Stub body per T-011.
    pub fn close_child(&self, _child: &Pane<V>) {
        // RPC wiring lands in T-018+.
    }

    /// Enumerate current child panes.
    ///
    /// Stub returns empty until the name-indexed lookup surface (R10)
    /// and the RPC dispatcher land. Callers depending on this should
    /// flag a follow-up against T-018+.
    pub fn children(&self) -> Vec<Pane<V>> {
        Vec::new()
    }

    /// Close every child pane, leaving the stack itself intact.
    ///
    /// Maps to `stack/clear` at T-018+. Stub body per T-011.
    pub fn clear(&self) {
        // RPC wiring lands in T-018+.
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

    // ---- PaneLike (T-009) --------------------------------------------------

    #[test]
    fn pane_like_polymorphic_over_pane_and_stack() {
        let p: Pane<VX> = Pane::from_handle(HandleId::new("a"));
        let s: Stack<VX> = Stack::from_handle(HandleId::new("b"));
        let vec: Vec<&dyn PaneLike> = vec![&p, &s];
        let ids: Vec<&str> = vec.iter().map(|x| x.handle().as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    // ---- Marker-gated affordances on Pane<V> (T-010) -----------------------

    #[test]
    fn pane_commandview_affordances_exist_on_command_view() {
        struct CV;
        impl crate::view::View for CV {}
        impl crate::view::CommandView for CV {}
        let p: Pane<CV> = Pane::from_handle(HandleId::new("x"));
        p.env("KEY", "VALUE");
        p.write_stdin(b"hello");
        let _ = p.pid();
    }

    #[test]
    fn pane_zellijview_affordances_exist_on_zellij_view() {
        struct ZV;
        impl crate::view::View for ZV {}
        impl crate::view::ZellijView for ZV {}
        let p: Pane<ZV> = Pane::from_handle(HandleId::new("x"));
        p.pipe(b"msg");
    }

    // ---- Stack<V> methods (T-011) ------------------------------------------

    #[test]
    fn stack_spawn_pane_returns_pane_of_same_view_type() {
        let s: Stack<VX> = Stack::from_handle(HandleId::new("s"));
        let _p: Pane<VX> = s.spawn_pane(PaneAttrs::default());
    }

    #[test]
    fn stack_close_child_accepts_pane_of_same_view_type() {
        let s: Stack<VX> = Stack::from_handle(HandleId::new("s"));
        let child: Pane<VX> = Pane::from_handle(HandleId::new("c"));
        s.close_child(&child);
    }

    #[test]
    fn stack_children_returns_vec_pane_v() {
        let s: Stack<VX> = Stack::from_handle(HandleId::new("s"));
        let v: Vec<Pane<VX>> = s.children();
        assert!(v.is_empty());
    }

    #[test]
    fn stack_clear_compiles() {
        let s: Stack<VX> = Stack::from_handle(HandleId::new("s"));
        s.clear();
    }

    #[test]
    fn pane_attrs_is_default_clone_debug() {
        let a = PaneAttrs::default();
        let _b = a.clone();
        let _dbg = format!("{:?}", PaneAttrs::default());
    }
}
