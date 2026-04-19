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
/// v0.2 widened this struct (see v0.2-backlog #2) so it carries a
/// per-view JSON payload (`view_attrs`). The struct is OPAQUE to
/// `ark-view`: each view type defines its own shape via a typed
/// `{ViewName}Attrs` struct that the caller serialises into
/// `view_attrs` via [`PaneAttrs::from_attrs`]. On the wire this travels
/// as the `attrs` field of
/// [`ark_ext_proto::StackSpawnPaneRequest`][`StackSpawnPaneRequest`]
/// (also `serde_json::Value`).
///
/// ### Why `serde_json::Value` instead of a generic `V::Attrs`?
///
/// Threading an associated type onto `Stack<V>` would cascade a new
/// `V::Attrs: Serialize + DeserializeOwned` bound onto every call site
/// that names `Stack<V>` — including trait object iteration through
/// [`PaneLike`]. `serde_json::Value` keeps the bound-inference cheap at
/// the minor cost of an intermediate JSON traversal (the Value is
/// serialised straight through the RPC layer, not round-tripped).
///
/// Backwards compat: the default (empty) `view_attrs` is
/// `serde_json::Value::Null`. The previous struct literal `PaneAttrs {}`
/// no longer compiles verbatim, but `PaneAttrs::default()` and
/// `..PaneAttrs::default()` update syntax both continue to work — and
/// in fact ALL ark-internal call sites used the default path.
///
/// [`StackSpawnPaneRequest`]: https://docs.rs/ark-ext-proto
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PaneAttrs {
    /// Per-view JSON attrs. Opaque to ark-view; each view type defines
    /// its own shape (serde_json::Value used to avoid a generic
    /// parameter on `PaneAttrs` — a `V::Attrs` associated type would
    /// cascade V bounds everywhere `Stack<V>` appears).
    ///
    /// Default: [`serde_json::Value::Null`].
    #[serde(default)]
    pub view_attrs: serde_json::Value,
}

impl PaneAttrs {
    /// Construct from a serialisable attrs type.
    ///
    /// Used at fan-out sites that have a typed `{ViewName}Attrs`
    /// struct on hand (e.g. `ClaudeCodeSubagentAttrs` → a
    /// `Stack<ClaudeCodeSubagent>` spawn_pane call). The resulting
    /// `PaneAttrs` carries `view_attrs == serde_json::to_value(&attrs)`.
    ///
    /// Returns a [`serde_json::Error`] when the caller's attrs type
    /// can't be serialised (rare in practice — all ark view-attr types
    /// are plain structs of owned strings / primitives).
    pub fn from_attrs<A: serde::Serialize>(attrs: &A) -> Result<Self, serde_json::Error> {
        Ok(Self {
            view_attrs: serde_json::to_value(attrs)?,
        })
    }

    /// Borrow the per-view JSON attrs.
    pub fn view_attrs(&self) -> &serde_json::Value {
        &self.view_attrs
    }
}

/// Dispatcher the host (supervisor) plugs in at init time to let
/// [`Stack::spawn_pane`] actually reach the `stack/spawn_pane` RPC.
///
/// `ark-view` sits BELOW `ark-ext-proto` in the crate DAG (see
/// `ark-view/Cargo.toml` — the crate forbids an `ark-ext-proto` dep),
/// so the real RPC client can't be referenced here directly. Instead,
/// the supervisor registers a concrete implementer via
/// [`register_stack_dispatcher`] at startup.
///
/// Implementations receive the stack handle + the per-view attrs JSON
/// (pre-extracted from [`PaneAttrs::view_attrs`]) and return either a
/// concrete freshly-minted child pane handle, or `None` when the
/// dispatch failed (transport error, capability denied, etc.). On
/// `None` the caller falls back to the synthetic handle path so the
/// current user experience (at-most-once-per-`agent_id` fan-out) is
/// preserved — the child just ends up with an opaque placeholder the
/// next pane op will surface as `HandleGone`.
pub trait StackDispatcher: Send + Sync {
    /// Invoke the host's `stack/spawn_pane` RPC and return the newly
    /// minted child pane handle. See trait docs for the None-fallback
    /// semantics.
    fn spawn_pane(&self, stack: &HandleId, view_attrs: &serde_json::Value) -> Option<HandleId>;
}

/// Process-global [`StackDispatcher`] slot. Set once at supervisor init
/// via [`register_stack_dispatcher`]; consulted by every
/// `Stack::spawn_pane` call in the process.
///
/// Process-global (not per-`Stack`) because `Stack<V>` is serialised as
/// a plain string and reconstructed from wire frames all over the
/// place — threading a dispatcher handle through every call site would
/// cascade changes into every crate that iterates typed handles.
/// Mirrors the pattern `supervisor::ext_dispatch::CAP_REGISTRY` uses.
static STACK_DISPATCHER: std::sync::OnceLock<Box<dyn StackDispatcher>> = std::sync::OnceLock::new();

/// Register the process-global [`StackDispatcher`]. Called once by the
/// supervisor (or a test harness) at startup. Idempotent: subsequent
/// calls after the first are silently ignored — the first-writer-wins
/// contract is sufficient because the supervisor is the only legitimate
/// caller and it initialises before any extension can hold a `Stack<V>`.
///
/// Returns `true` if the dispatcher was installed; `false` if one was
/// already registered.
pub fn register_stack_dispatcher<D: StackDispatcher + 'static>(dispatcher: D) -> bool {
    STACK_DISPATCHER.set(Box::new(dispatcher)).is_ok()
}

/// Test-only: read back the current process-global dispatcher (if any).
/// Exposed so tests can assert registration happened; the hot path
/// inlines this check directly.
#[doc(hidden)]
pub fn stack_dispatcher() -> Option<&'static dyn StackDispatcher> {
    STACK_DISPATCHER.get().map(|b| b.as_ref())
}

impl<V: View> Stack<V> {
    /// Spawn a new pane into this stack.
    ///
    /// **v0.2 live RPC path** (see v0.2-backlog #2). When the host has
    /// registered a [`StackDispatcher`] via
    /// [`register_stack_dispatcher`] — which the supervisor does once
    /// at startup — `spawn_pane` forwards `attrs.view_attrs` to the
    /// host's `stack/spawn_pane` RPC and returns the concrete child
    /// pane handle the host minted.
    ///
    /// **Fallback** — when no dispatcher is registered (unit tests, the
    /// ext-crate-only test harness, any context where the supervisor
    /// isn't in the loop), OR when the registered dispatcher returned
    /// `None` (transport error / capability denied / etc.), the method
    /// falls back to a synthetic placeholder handle of the shape
    /// `__unwired_spawn__-<stack-handle>-<counter>`. Callers that
    /// exercise the returned handle against the host will get
    /// `HandleGone` on the next pane op, matching R7.
    ///
    /// The synthetic-handle counter is process-global + monotonic so
    /// two back-to-back `spawn_pane` calls never produce the same
    /// placeholder — unit tests that assert idempotency-by-handle
    /// continue to work.
    pub fn spawn_pane(&self, attrs: PaneAttrs) -> Pane<V> {
        if let Some(dispatcher) = STACK_DISPATCHER.get() {
            if let Some(child) = dispatcher.spawn_pane(&self.handle, &attrs.view_attrs) {
                return Pane::from_handle(child);
            }
        }
        // Fallback: synthetic placeholder. See doc-comment.
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

    // ---- v0.2 backlog #2: PaneAttrs widening ------------------------------

    #[test]
    fn pane_attrs_default_view_attrs_is_null() {
        // Default round-trip: empty `view_attrs` serialises as JSON null
        // (or a struct with `view_attrs: null`). The important invariant
        // is that `Default::default()` stays source-compatible with the
        // old empty-struct shape for every existing call site.
        let a = PaneAttrs::default();
        assert!(a.view_attrs.is_null());
    }

    #[test]
    fn pane_attrs_from_attrs_round_trip() {
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct SampleAttrs {
            id: String,
            transcript_path: String,
        }
        let sample = SampleAttrs {
            id: "sub-abc".into(),
            transcript_path: "/tmp/t.jsonl".into(),
        };
        let pa = PaneAttrs::from_attrs(&sample).expect("serialisable");
        // Round-trip back: the typed attrs survive the JSON cast.
        let back: SampleAttrs =
            serde_json::from_value(pa.view_attrs.clone()).expect("deserialisable");
        assert_eq!(back, sample);
    }

    #[test]
    fn pane_attrs_serializes_and_deserializes() {
        // Wire-level round-trip — exercised by the supervisor's
        // `stack/spawn_pane` handler when it receives the opaque JSON.
        let pa = PaneAttrs::from_attrs(&json!({"k": "v"})).unwrap();
        let wire = serde_json::to_value(&pa).unwrap();
        let back: PaneAttrs = serde_json::from_value(wire).unwrap();
        assert_eq!(back.view_attrs, json!({"k": "v"}));
    }

    #[test]
    fn pane_attrs_deserializes_from_empty_object_backcompat() {
        // v0.1 had an empty-struct PaneAttrs. The serde shape there was
        // `{}`. v0.2's default deserialises from `{}` with `view_attrs`
        // falling back to `null` via `#[serde(default)]` — preserves
        // wire compat for any frame a v0.1 peer sent.
        let pa: PaneAttrs = serde_json::from_str("{}").unwrap();
        assert!(pa.view_attrs.is_null());
    }

    // ---- v0.2 backlog #2: Stack::spawn_pane dispatcher path ---------------

    // NOTE: the process-global STACK_DISPATCHER is one-shot (OnceLock).
    // We therefore CANNOT register a dispatcher inside a unit test here
    // without poisoning every subsequent test in the process. The
    // fallback path (no dispatcher registered) IS exercised by the
    // existing `stack_spawn_pane_returns_pane_of_same_view_type` test.
    //
    // Dispatcher-registered behaviour is verified via a dedicated
    // integration test in `tests/stack_dispatcher.rs` — it runs in its
    // own process so the OnceLock doesn't leak across unrelated tests.

    #[test]
    fn stack_spawn_pane_synthetic_handle_shape() {
        // Fallback path — the synthetic handle contains the stack's
        // handle bytes and a monotonic counter. Pins the shape so any
        // future dispatcher-fallback tweak stays compatible.
        let s: Stack<VX> = Stack::from_handle(HandleId::new("my-stack"));
        let p = s.spawn_pane(PaneAttrs::default());
        let h = p.handle().as_str();
        assert!(h.starts_with("__unwired_spawn__-my-stack-"), "got {h}");
    }

    #[test]
    fn stack_spawn_pane_with_typed_attrs_fallback() {
        // Even with a non-null view_attrs payload, the fallback stays
        // in the synthetic handle shape — the dispatcher is the only
        // thing that translates `view_attrs` into a real child handle.
        let s: Stack<VX> = Stack::from_handle(HandleId::new("s"));
        let pa = PaneAttrs::from_attrs(&json!({"id": "child-1"})).unwrap();
        let p = s.spawn_pane(pa);
        assert!(p.handle().as_str().starts_with("__unwired_spawn__-s-"));
    }
}
