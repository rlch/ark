//! Intent registry + op dispatch surface — T-047..T-055 / R7.
//!
//! Scene reactions (`on` blocks) and keybinds (`bind` blocks) both
//! dispatch their op list through a single [`IntentRegistry`]. Each op
//! implements the object-safe [`Intent`] trait, which parses its
//! arguments straight from the reaction's [`kdl::KdlNode`] and drives
//! the side-effect.
//!
//! ## Deliberate differences vs the v2 archive
//!
//! The v2 archive's [`intent.rs`][v2] exposed a generic `Intent<Args>`
//! trait with an associated type that fed `facet_kdl::from_str` through
//! a string round-trip per dispatch. v3 sheds the generics — ops parse
//! their own args off the [`kdl::KdlNode`] using the node's property /
//! argument accessors — so the object-safe trait stays flat and the
//! registry holds one `Arc<dyn Intent>` entry per verb. See R7 for the
//! canonical op vocabulary.
//!
//! ## Runtime handles
//!
//! Real mux, event-bus, and supervisor wiring lands in Tier-10+ when
//! the concrete types in `crates/mux/`, `crates/core/`, and
//! `crates/supervisor/` are exposed. For Tier-5 we carry narrow trait
//! objects ([`MuxHandle`], [`EventBus`]) so tests can substitute mocks
//! and downstream tiers can swap in real implementations without
//! churning op code.
//!
//! ## Idempotency + fail-fast (T-055)
//!
//! * `focus` / `close` / `rename` / `resize` / `move` / `pin` / `unpin`
//!   / `reload_scene`: catch "handle not found" as a noop-success.
//! * `spawn` / `new_tab`: check-then-create; if the handle exists the
//!   op focuses the existing target instead of re-creating it.
//! * `pipe` / `emit` / `exec` / `set_status`: always side-effect.
//!
//! Dispatch is fail-fast at the registry layer: the first error bubbles
//! out of [`IntentRegistry::dispatch`]. The caller (the reactions
//! dispatcher in Tier 6) is responsible for logging + skipping the
//! remaining ops per R4.9.
//!
//! [v2]: ../../../scene-v2-archive/src/intent.rs

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use kdl::KdlNode;

use crate::ast::layout::Handle;
use crate::error::SceneError;
use crate::id::SceneId;

// scene-2026-04-18 T-009: retire scene-local `HandleKind` enum — every
// reference now points at the re-exported `ark_view::HandleKind` which
// carries only the three runtime-significant variants `{Tab, Pane,
// Stack}`. The retired `Command` / `Plugin` variants (view-type info)
// moved to `ark_view::Pane<V>` / `ark_view::Stack<V>` per soul Phase 2
// R3/R4.
pub use ark_view::HandleKind;

// ---------------------------------------------------------------------------
// Return value
// ---------------------------------------------------------------------------

/// Value an [`Intent`] returns after dispatch.
///
/// Kept deliberately small — side-effect ops return [`IntentValue::None`],
/// query-shaped ops (future) can surface scalars. The full
/// `serde_json::Value` surface the v2 archive exposed is deferred to v0.2
/// when op→op result chaining (R7.cascade) pulls it back in.
#[derive(Debug, Clone, PartialEq)]
pub enum IntentValue {
    /// The op ran and produced no value (the common case — all Tier 5 ops).
    None,
    /// A string scalar (e.g. `exec` stdout when trimmed to a line).
    String(String),
    /// A signed integer (e.g. `exec` exit code).
    Integer(i64),
    /// A boolean flag.
    Boolean(bool),
}

// ---------------------------------------------------------------------------
// Runtime handle traits
// ---------------------------------------------------------------------------

/// Narrow trait surface for the zellij mux handle.
///
/// The concrete implementation lives in `crates/mux/` (Tier 10+).
/// Keeping the trait here means Tier-5 ops compile + test against a
/// mock without pulling the full mux crate into the scene dep graph.
///
/// Methods return `Result<(), String>` so mux-side errors surface as
/// opaque strings — callers don't need to depend on the mux's own error
/// types. Idempotent ops (T-055) treat `Err(msg)` whose body contains
/// `"not found"` as a noop-success.
pub trait MuxHandle: Send + Sync + std::fmt::Debug {
    /// Close the pane addressed by `handle`. Idempotent per T-055 when
    /// the caller's op classification is `NoopOnAbsent`.
    fn close_pane(&self, handle: &Handle) -> Result<(), String>;

    /// Close the tab addressed by `handle`. Idempotent when absent.
    fn close_tab(&self, handle: &Handle) -> Result<(), String>;

    /// Transfer focus to the referenced pane.
    fn focus_pane(&self, handle: &Handle) -> Result<(), String>;

    /// Transfer focus to the referenced tab.
    fn focus_tab(&self, handle: &Handle) -> Result<(), String>;

    /// Rename a tab (tab-only op per R7).
    fn rename_tab(&self, handle: &Handle, name: &str) -> Result<(), String>;

    /// Resize a pane along `direction` ("up"/"down"/"left"/"right") by
    /// `by` ("inc"/"dec"). Pane-only.
    fn resize_pane(&self, handle: &Handle, direction: &str, by: &str) -> Result<(), String>;

    /// Move a pane to a named anchor (e.g. "top-right"). Pane-only.
    fn move_pane(&self, handle: &Handle, to: &str) -> Result<(), String>;

    /// Pin an overlay pane so it survives tab switch.
    fn pin_pane(&self, handle: &Handle) -> Result<(), String>;

    /// Unpin a previously pinned overlay pane.
    fn unpin_pane(&self, handle: &Handle) -> Result<(), String>;

    /// Check whether a pane or tab with `handle` exists in the running mux.
    /// Used by `spawn` / `new_tab` for check-then-create-else-focus
    /// semantics (T-055).
    fn handle_exists(&self, handle: &Handle) -> bool;

    /// Spawn a tiled pane (or overlay when `overlay` is `true`) with the
    /// given handle. Called by the `spawn` op after [`Self::handle_exists`]
    /// reports `false`.
    fn spawn_pane(
        &self,
        handle: &Handle,
        overlay: bool,
        view_body: Option<&str>,
    ) -> Result<(), String>;

    /// Create a new tab with the given handle, optional display name,
    /// and optional working directory.
    fn new_tab(&self, handle: &Handle, name: Option<&str>, cwd: Option<&str>)
    -> Result<(), String>;

    /// Send `payload` from one pane to another. Both source and target
    /// must exist.
    fn pipe(&self, from: &Handle, to: &Handle, payload: &str) -> Result<(), String>;
}

/// Event-bus trait surface used by `emit` + `set_status`.
///
/// Tier-7's `ark-bus` plugin implements the real bus; Tier-5 tests pass
/// in a mock that captures events to a `Vec` for inspection.
pub trait EventBus: Send + Sync + std::fmt::Debug {
    /// Emit a synthetic `UserEvent` on the bus.
    ///
    /// * `name`: fully-qualified event name (e.g. `"user.my_event"`,
    ///   `"ark.scene.reloaded"`).
    /// * `source`: attribution tag (e.g. `"scene"` / `"ext:<name>"`).
    /// * `payload`: arbitrary JSON payload.
    fn emit_user_event(&self, name: &str, source: &str, payload: serde_json::Value);

    /// Push a status-bar message — sugar over [`Self::emit_user_event`]
    /// routing to the status extension. Default impl emits a
    /// `ark.status.push` user event carrying the status fields in the
    /// payload.
    fn push_status(&self, text: &str, severity: Option<&str>, ttl_ms: Option<u64>) {
        let mut payload = serde_json::Map::new();
        payload.insert("text".into(), serde_json::Value::String(text.to_string()));
        if let Some(sev) = severity {
            payload.insert(
                "severity".into(),
                serde_json::Value::String(sev.to_string()),
            );
        }
        if let Some(ttl) = ttl_ms {
            payload.insert("ttl_ms".into(), serde_json::Value::Number(ttl.into()));
        }
        self.emit_user_event(
            "ark.status.push",
            "scene",
            serde_json::Value::Object(payload),
        );
    }
}

// ---------------------------------------------------------------------------
// IntentContext
// ---------------------------------------------------------------------------

/// Runtime context passed to every [`Intent::dispatch`] call.
///
/// Bundles the scene identity, origin attribution, and runtime handles
/// the op may touch. Handles are wrapped in `Arc<dyn Trait>` so cloning
/// the context across concurrent dispatches is cheap.
#[derive(Debug, Clone)]
pub struct IntentContext {
    /// Identity of the scene whose reaction / keybind fired this op.
    /// Used for hot-reload delta detection (R14) and attribution in
    /// `ark scene explain` (R11).
    pub scene_id: SceneId,

    /// Origin of the dispatch — e.g. `"scene"`, `"ext:<name>"`, `"bind"`.
    /// Rendered into the `tracing` span so users can filter by source.
    pub origin: String,

    /// Optional hint about the declared type of the handle the op is
    /// acting on, set by the compile pipeline when it resolves the
    /// `@handle` reference. `None` when the op carries no handle.
    pub handle_type_hint: Option<HandleKind>,

    /// Mux handle. `None` in tests / scene-less agents; ops that need
    /// the mux return `Err` with a clear "mux not wired" message in
    /// that case.
    pub mux: Option<Arc<dyn MuxHandle>>,

    /// Event bus handle. `None` in tests; `emit` + `set_status` are
    /// noops with a `tracing::warn!` when absent.
    pub bus: Option<Arc<dyn EventBus>>,
}

impl IntentContext {
    /// Construct an `IntentContext` with the supplied scene identity
    /// and no live handles. Used by tests + early wiring.
    pub fn new(scene_id: SceneId, origin: impl Into<String>) -> Self {
        Self {
            scene_id,
            origin: origin.into(),
            handle_type_hint: None,
            mux: None,
            bus: None,
        }
    }

    /// Builder: attach a mux handle.
    pub fn with_mux(mut self, mux: Arc<dyn MuxHandle>) -> Self {
        self.mux = Some(mux);
        self
    }

    /// Builder: attach an event bus handle.
    pub fn with_bus(mut self, bus: Arc<dyn EventBus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Builder: attach a handle-type hint.
    pub fn with_handle_type_hint(mut self, hint: HandleKind) -> Self {
        self.handle_type_hint = Some(hint);
        self
    }
}

// ---------------------------------------------------------------------------
// Intent trait + registry
// ---------------------------------------------------------------------------

/// A single registered op.
///
/// Implementations parse their own arguments off the incoming
/// [`KdlNode`] (typically by walking `node.entries()`) and drive the
/// side-effect against [`IntentContext`]. The trait is object-safe so a
/// single `HashMap<String, Arc<dyn Intent>>` can hold the whole
/// core + extension op vocabulary.
///
/// Returning `Ok(IntentValue::None)` is the common case for Tier-5 ops
/// since they side-effect rather than producing values.
#[async_trait]
pub trait Intent: Send + Sync {
    /// Run the op.
    ///
    /// Arguments are read directly off `args` (a reference to the op's
    /// KDL node in the scene AST). `ctx` carries runtime handles + the
    /// dispatch origin.
    ///
    /// Idempotent ops (T-055) treat "handle not found" errors as
    /// `Ok(IntentValue::None)` internally; the caller doesn't see the
    /// distinction.
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError>;
}

/// Thread-safe registry of ops, keyed by fully-qualified name.
///
/// Cloning is cheap (one `Arc` clone). `register` takes `&mut self` for
/// construction-time population; once built, the registry is typically
/// wrapped in an `Arc<IntentRegistry>` so concurrent dispatches share a
/// single read-only table. No interior mutability — the contract is
/// "build once at scene compile, consult many times at runtime".
#[derive(Default)]
pub struct IntentRegistry {
    entries: HashMap<String, Arc<dyn Intent>>,
}

impl std::fmt::Debug for IntentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentRegistry")
            .field("ops", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl IntentRegistry {
    /// Construct an empty registry — extension-only scenes or tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a registry pre-populated with every `ark.core.*` op
    /// (R7 #1–13). Extensions register additional namespaced ops on top
    /// of this baseline.
    pub fn with_core_ops() -> Self {
        let mut reg = Self::new();
        crate::ops::register_core_ops(&mut reg);
        reg
    }

    /// Register an op under `name`. Re-registering the same name
    /// replaces the previous implementation.
    pub fn register(&mut self, name: &str, intent: Arc<dyn Intent>) {
        self.entries.insert(name.to_string(), intent);
    }

    /// Current number of registered ops.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All registered op names, in undefined order. Useful for
    /// "did you mean?" suggestions and `ark scene explain`.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(|s| s.as_str()).collect()
    }

    /// Dispatch an op by fully-qualified name.
    ///
    /// Returns the first error from the matched op's `dispatch` call.
    /// The caller (reactions dispatcher, Tier 6) decides what to do
    /// with the remaining ops — per R4.9 the policy is "log + skip
    /// remaining; event loop continues".
    ///
    /// Unknown op names surface as [`SceneError::UnknownOp`] with an
    /// empty help field (callers wrap the error with a suggestion
    /// rendered via [`crate::suggest`] before surfacing).
    pub async fn dispatch(
        &self,
        name: &str,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let op = self.entries.get(name).cloned().ok_or_else(|| {
            // Placeholder source context; the reactions dispatcher wraps
            // the error with the real `NamedSource` before surfacing.
            SceneError::UnknownOp {
                op: name.to_string(),
                help: String::new(),
                src: miette::NamedSource::new("<runtime>", String::new()),
                span: miette::SourceSpan::new(0.into(), 0),
            }
        })?;
        op.dispatch(args, ctx).await
    }
}

// ---------------------------------------------------------------------------
// Helpers used by op implementations
// ---------------------------------------------------------------------------

/// Read the first positional argument off `node` as a string.
///
/// Used by ops whose shape is `<verb> @handle`. Returns `None` when the
/// node has no positional arguments or the first entry isn't a string.
pub(crate) fn first_argument(node: &KdlNode) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(|s| s.to_string())
}

/// Read a property `key=` off `node` as a string.
pub(crate) fn property_str(node: &KdlNode, key: &str) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().map(|n| n.value() == key).unwrap_or(false))
        .and_then(|e| e.value().as_string())
        .map(|s| s.to_string())
}

/// Read a property `key=` off `node` as a u64.
pub(crate) fn property_u64(node: &KdlNode, key: &str) -> Option<u64> {
    node.entries()
        .iter()
        .find(|e| e.name().map(|n| n.value() == key).unwrap_or(false))
        .and_then(|e| e.value().as_integer())
        .and_then(|i| u64::try_from(i).ok())
}

/// Parse a raw handle string into a typed [`Handle`], surfacing grammar
/// errors as [`SceneError::OpUnresolvedRef`] with a "did you mean?"
/// help body built by the caller.
pub(crate) fn parse_handle(raw: &str, op: &str) -> Result<Handle, SceneError> {
    Handle::new(raw).map_err(|_| SceneError::OpUnresolvedRef {
        op: op.to_string(),
        kind: "handle".to_string(),
        name: raw.to_string(),
        help: String::new(),
        src: miette::NamedSource::new("<runtime>", String::new()),
        span: miette::SourceSpan::new(0.into(), 0),
    })
}

/// Classify whether a mux error should be treated as a noop per the
/// T-055 idempotency policy.
///
/// Any mux error whose body contains the literal `"not found"` is
/// considered a benign "handle absent" case. Anything else surfaces as
/// a genuine [`SceneError::OpFailed`].
pub(crate) fn is_noop_absent_error(msg: &str) -> bool {
    msg.contains("not found") || msg.contains("no such")
}

/// Render a mux-side error string into a [`SceneError::OpFailed`],
/// applying the T-055 idempotency policy: absent-handle errors map to
/// `Ok(IntentValue::None)` so callers can use the short-circuit
/// `?`-return pattern uniformly.
pub(crate) fn idempotent_map(
    op: &'static str,
    result: Result<(), String>,
) -> Result<IntentValue, SceneError> {
    match result {
        Ok(()) => Ok(IntentValue::None),
        Err(msg) if is_noop_absent_error(&msg) => {
            tracing::debug!(
                target: "scene::ops",
                op,
                reason = %msg,
                "idempotent noop: target absent"
            );
            Ok(IntentValue::None)
        }
        Err(msg) => Err(SceneError::OpFailed {
            op: op.to_string(),
            message: msg,
        }),
    }
}

/// Non-idempotent counterpart of [`idempotent_map`] — any mux-side
/// error surfaces as [`SceneError::OpFailed`] verbatim.
pub(crate) fn strict_map(
    op: &'static str,
    result: Result<(), String>,
) -> Result<IntentValue, SceneError> {
    match result {
        Ok(()) => Ok(IntentValue::None),
        Err(msg) => Err(SceneError::OpFailed {
            op: op.to_string(),
            message: msg,
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // -- Mock handles ------------------------------------------------------

    /// Mock mux that records every call into a shared `Vec<String>` so
    /// tests can assert on the dispatched sequence. Optionally fails
    /// with a configured error message to exercise fail-fast + idempotency.
    #[derive(Debug, Default)]
    pub(crate) struct MockMux {
        pub calls: Mutex<Vec<String>>,
        pub fail_with: Mutex<Option<String>>,
        pub existing: Mutex<Vec<String>>,
    }

    impl MockMux {
        fn record(&self, call: String) {
            self.calls.lock().expect("poisoned").push(call);
        }
        fn check(&self, call: String) -> Result<(), String> {
            self.record(call);
            if let Some(msg) = self.fail_with.lock().expect("poisoned").clone() {
                Err(msg)
            } else {
                Ok(())
            }
        }
        pub(crate) fn take_calls(&self) -> Vec<String> {
            std::mem::take(&mut *self.calls.lock().expect("poisoned"))
        }
        pub(crate) fn set_fail(&self, msg: impl Into<String>) {
            *self.fail_with.lock().expect("poisoned") = Some(msg.into());
        }
        pub(crate) fn mark_existing(&self, raw: &str) {
            self.existing
                .lock()
                .expect("poisoned")
                .push(raw.to_string());
        }
    }

    impl MuxHandle for MockMux {
        fn close_pane(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("close_pane({})", h.raw()))
        }
        fn close_tab(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("close_tab({})", h.raw()))
        }
        fn focus_pane(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("focus_pane({})", h.raw()))
        }
        fn focus_tab(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("focus_tab({})", h.raw()))
        }
        fn rename_tab(&self, h: &Handle, name: &str) -> Result<(), String> {
            self.check(format!("rename_tab({},{name})", h.raw()))
        }
        fn resize_pane(&self, h: &Handle, d: &str, by: &str) -> Result<(), String> {
            self.check(format!("resize_pane({},{d},{by})", h.raw()))
        }
        fn move_pane(&self, h: &Handle, to: &str) -> Result<(), String> {
            self.check(format!("move_pane({},{to})", h.raw()))
        }
        fn pin_pane(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("pin_pane({})", h.raw()))
        }
        fn unpin_pane(&self, h: &Handle) -> Result<(), String> {
            self.check(format!("unpin_pane({})", h.raw()))
        }
        fn handle_exists(&self, h: &Handle) -> bool {
            self.existing
                .lock()
                .expect("poisoned")
                .iter()
                .any(|r| r == h.raw())
        }
        fn spawn_pane(
            &self,
            h: &Handle,
            overlay: bool,
            view_body: Option<&str>,
        ) -> Result<(), String> {
            self.check(format!(
                "spawn_pane({},overlay={overlay},view={:?})",
                h.raw(),
                view_body
            ))
        }
        fn new_tab(&self, h: &Handle, name: Option<&str>, cwd: Option<&str>) -> Result<(), String> {
            self.check(format!(
                "new_tab({},name={:?},cwd={:?})",
                h.raw(),
                name,
                cwd
            ))
        }
        fn pipe(&self, f: &Handle, t: &Handle, payload: &str) -> Result<(), String> {
            self.check(format!("pipe({},{},{payload})", f.raw(), t.raw()))
        }
    }

    /// Mock event bus capturing emitted events.
    #[derive(Debug, Default)]
    pub(crate) struct MockBus {
        pub events: Mutex<Vec<(String, String, serde_json::Value)>>,
    }
    impl MockBus {
        pub(crate) fn take_events(&self) -> Vec<(String, String, serde_json::Value)> {
            std::mem::take(&mut *self.events.lock().expect("poisoned"))
        }
    }
    impl EventBus for MockBus {
        fn emit_user_event(&self, name: &str, source: &str, payload: serde_json::Value) {
            self.events.lock().expect("poisoned").push((
                name.to_string(),
                source.to_string(),
                payload,
            ));
        }
    }

    // -- Fixtures ----------------------------------------------------------

    pub(crate) fn test_scene_id() -> SceneId {
        SceneId::new(PathBuf::from("/tmp/scene.kdl"), b"scene \"t\" { }")
    }

    pub(crate) fn node_from(src: &str) -> KdlNode {
        let doc: kdl::KdlDocument = src.parse().expect("test KDL parses");
        doc.nodes().first().expect("at least one node").clone()
    }

    pub(crate) fn ctx_with(mux: Arc<MockMux>, bus: Arc<MockBus>) -> IntentContext {
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
    }

    // -- Trivial op used for registry tests --------------------------------

    struct Noop;
    #[async_trait]
    impl Intent for Noop {
        async fn dispatch(
            &self,
            _args: &KdlNode,
            _ctx: &IntentContext,
        ) -> Result<IntentValue, SceneError> {
            Ok(IntentValue::None)
        }
    }

    struct Bomb;
    #[async_trait]
    impl Intent for Bomb {
        async fn dispatch(
            &self,
            _args: &KdlNode,
            _ctx: &IntentContext,
        ) -> Result<IntentValue, SceneError> {
            Err(SceneError::OpFailed {
                op: "test.bomb".into(),
                message: "boom".into(),
            })
        }
    }

    // -- Registry tests ----------------------------------------------------

    #[tokio::test]
    async fn register_and_dispatch() {
        let mut reg = IntentRegistry::new();
        reg.register("test.noop", Arc::new(Noop));
        assert_eq!(reg.len(), 1);
        let node = node_from("test.noop");
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux, bus);
        let v = reg.dispatch("test.noop", &node, &ctx).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }

    #[tokio::test]
    async fn unknown_op_errors() {
        let reg = IntentRegistry::new();
        let node = node_from("whatever");
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux, bus);
        let err = reg
            .dispatch("test.missing", &node, &ctx)
            .await
            .expect_err("must error");
        assert!(matches!(err, SceneError::UnknownOp { op, .. } if op == "test.missing"));
    }

    #[tokio::test]
    async fn fail_fast_returns_first_error() {
        let mut reg = IntentRegistry::new();
        reg.register("test.bomb", Arc::new(Bomb));
        let node = node_from("test.bomb");
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        let ctx = ctx_with(mux, bus);
        let err = reg
            .dispatch("test.bomb", &node, &ctx)
            .await
            .expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "test.bomb"));
    }

    #[test]
    fn with_core_ops_populates_registry() {
        let reg = IntentRegistry::with_core_ops();
        assert!(
            reg.names().iter().any(|n| n.starts_with("ark.core.")),
            "expected at least one ark.core.* op"
        );
    }

    #[test]
    fn idempotent_map_absent_becomes_ok() {
        let r = idempotent_map("test.focus", Err("handle not found".into())).expect("absent -> ok");
        assert_eq!(r, IntentValue::None);
    }

    #[test]
    fn idempotent_map_other_errors_surface() {
        let err = idempotent_map("test.focus", Err("mux disconnected".into()))
            .expect_err("non-absent must surface");
        assert!(matches!(err, SceneError::OpFailed { op, .. } if op == "test.focus"));
    }

    #[test]
    fn strict_map_all_errors_surface() {
        let err = strict_map("test.spawn", Err("handle not found".into()))
            .expect_err("strict must surface even absent errors");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }
}
