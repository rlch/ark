//! Intent + op registry — R7 runtime surface.
//!
//! Scene reactions and keybinds both dispatch through a single
//! [`IntentRegistry`]. Each op is an implementation of the [`Intent`]
//! trait with:
//!
//! * a `NAME` (e.g. `"ark.core.open_tab"`);
//! * a typed, facet-derived [`Intent::Args`] struct (the argument shape is
//!   parsed directly from the reaction's KDL node at scene parse time via
//!   `facet-kdl`, giving each op static type-checking + miette-rendered
//!   span diagnostics for free);
//! * an async `dispatch(&self, args, ctx)` that returns an optional
//!   [`IntentValue`] — a `serde_json::Value` handed back to the
//!   reaction cascade so CEL / follow-up ops can read the result
//!   (see R7 + R8).
//!
//! The registry itself stores ops behind an object-safe trait
//! ([`DynIntent`], implemented automatically for every `T: Intent`) so a
//! single `HashMap<&'static str, Arc<dyn DynIntent>>` can hold the whole
//! core + extension vocabulary. Dispatch through the registry
//! ([`IntentRegistry::dispatch_dyn`]) accepts the raw `KdlNode` the
//! reaction pass pulled out of the scene AST; the adapter
//! re-serialises that node (`KdlNode: Display`) and feeds the single-line
//! rendering back into `facet_kdl::from_str::<I::Args>`. facet-kdl does
//! not expose a per-node deserialiser (only `from_str` / `from_slice`),
//! so the round-trip is the shortest legal path today. Spans are lost
//! across the trip — good enough for v1; a zero-copy per-node path is
//! tracked as TODO below.
//!
//! Thread-safety: the inner map is an `Arc<tokio::sync::RwLock<_>>`.
//! `dispatch_dyn` is async and `.await`s `I::dispatch`, so a `tokio`
//! async lock is required — holding a `std::sync::RwLock` across an
//! `.await` would starve the executor. The lock is released as soon
//! as the per-op `Arc<dyn DynIntent>` is cloned out; `I::dispatch`
//! runs without holding any registry state.
//!
//! Cross-crate handles ([`IntentContext::mux`], `.bus`, `.supervisor`)
//! are PLACEHOLDER types at this tier. Tier-4 and Tier-5 tasks replace
//! them with the real handles once `crates/mux/`, `crates/core/` and
//! `crates/supervisor/` export the needed public surface. See per-type
//! TODOs.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use facet::Facet;
use kdl::KdlNode;
use miette::Diagnostic;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::id::SceneId;

// ---------------------------------------------------------------------------
// Return + error types
// ---------------------------------------------------------------------------

/// Value an [`Intent`] returns after dispatch.
///
/// v1 models op results as `serde_json::Value` so CEL (R8) and follow-up
/// reactions (R10 cascade) can inspect the result uniformly. Typed
/// per-op return structs could be layered on later by having the op
/// implementation call `serde_json::to_value(...)` internally; the
/// registry does not constrain the shape.
pub type IntentValue = serde_json::Value;

/// Error surface for the op dispatch pipeline.
///
/// Variants align with the `op/*` family in cavekit-scene R12:
///
/// * `op/unknown`      — registry lookup miss
/// * `op/args-invalid` — facet-kdl rejected the reaction's KDL args
/// * `op/failed`       — the op's own `dispatch` returned `Err`
///
/// The `Unknown` variant carries the optional "did you mean …?" hint
/// that the scene compile pipeline (scope pass) computes via
/// [`crate::suggest::suggest_similar`]; the registry itself does not
/// suggest, so callers attach the suggestion before surfacing.
#[derive(Debug, Error, Diagnostic)]
pub enum IntentError {
    /// The requested op name is not registered. Typically surfaces at
    /// `ark scene check` rather than runtime; at runtime it means a
    /// namespaced extension op went missing between scene compile and
    /// dispatch (extension unloaded? hot-reload race?).
    #[error("unknown op `{name}`")]
    #[diagnostic(
        code = "op/unknown",
        help("This op is not in the intent registry. Check the scene's `use` list, or run `ark scene check` to surface this at compile time with a \"did you mean …?\" hint.")
    )]
    Unknown {
        /// The op name the caller asked for (as declared in the scene).
        name: String,
    },

    /// facet-kdl rejected the node's arguments against the op's typed
    /// `Args` schema. The inner error carries miette spans into the
    /// offending KDL source, so surfacing through `miette` renders a
    /// caret at the bad value.
    #[error("invalid args for op `{name}`")]
    #[diagnostic(
        code = "op/args-invalid",
        help("Run `ark scene check` to see the expected shape for this op (facet SHAPE reflection surfaces field docs + types).")
    )]
    ArgsInvalid {
        /// The op that failed to parse args.
        name: String,

        /// facet-kdl's rendered error text. We keep it as a string
        /// (rather than embedding the full `KdlDeserializeError`
        /// with its own source span) so this crate's `intent.rs`
        /// stays decoupled from the scene-file source that produced
        /// the node — the caller (compile pipeline) already owns
        /// the `NamedSource`.
        message: String,
    },

    /// The op ran to completion but returned its own error. The inner
    /// `Box<dyn Diagnostic>` lets op implementations surface their
    /// own rich, typed errors (e.g. `mux/no-such-tab`, `plugin/
    /// mount-failed`) without this crate having to import them.
    #[error("op `{name}` failed: {message}")]
    #[diagnostic(code = "op/failed")]
    Failed {
        /// The op that failed.
        name: String,

        /// Human-readable summary. For anything richer, the op
        /// returned a `Diagnostic`; we hold a copy of its `Display`
        /// here so the top-level error is self-describing without
        /// needing the reviewer to drill into `source`.
        message: String,

        /// Underlying op-specific diagnostic, typed as a trait object
        /// so every op can supply its own error type without
        /// cross-crate cycles. `miette` walks `source()` when
        /// rendering, so the inner diagnostic still surfaces.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl IntentError {
    /// Construct an `Unknown` error from the attempted op name.
    pub fn unknown(name: impl Into<String>) -> Self {
        Self::Unknown { name: name.into() }
    }

    /// Construct an `ArgsInvalid` error, capturing the parse message.
    pub fn args_invalid(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ArgsInvalid {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Construct a `Failed` error from a boxed diagnostic.
    pub fn failed(
        name: impl Into<String>,
        err: Box<dyn std::error::Error + Send + Sync + 'static>,
    ) -> Self {
        let name = name.into();
        let message = err.to_string();
        Self::Failed {
            name,
            message,
            source: err,
        }
    }
}

// ---------------------------------------------------------------------------
// Placeholder handles
// ---------------------------------------------------------------------------
//
// The scene crate is a leaf in the workspace dep graph (mux, core,
// supervisor depend on it — not the other way round). Real handles for
// the mux, event bus, and supervisor therefore can't live here without
// introducing a cycle. Later tiers either:
//
// * flip this crate to depend on narrow trait crates (e.g. `ark-mux-api`
//   that exposes only an async-trait `MuxHandle`), or
// * move `IntentContext` to a sibling crate that sits above scene in the
//   DAG.
//
// For T-4.1 we keep opaque placeholders so the Intent trait + registry
// can land without waiting on that decomposition.

/// Placeholder for the zellij mux handle.
///
/// Real type: `ark_mux::ZellijMux` (or an `Arc<dyn MuxHandle>` extracted
/// from it). Core + plugin ops that touch tabs / panes (R7 ops 1–8)
/// take this handle to invoke `launch-or-focus-plugin`, `new-tab`,
/// `close-pane`, etc.
///
/// TODO(T-4.2): replace with the real mux handle (or a narrow trait
/// object from an `ark-mux-api` crate, to avoid pulling the full mux
/// implementation into the scene dep graph).
#[derive(Debug, Default)]
pub struct MuxPlaceholder;

/// Placeholder for the ark event bus.
///
/// Real type: `ark_core::EventBus`. Ops that `emit` synthetic
/// `UserEvent`s (R7 op 10) and reactions that fan out broadcast through
/// this handle.
///
/// The placeholder implementation captures every emitted
/// [`AgentEvent::UserEvent`] in an inner `Mutex<Vec<_>>` so tests (T-4.2)
/// can assert on the payload without a real bus. Production callers
/// drain the queue via [`EventBus::drain_user_events`]; the real bus
/// will replace this surface entirely.
///
/// TODO(T-5.x): replace with `Arc<ark_core::EventBus>` (or an
/// `EmitHandle` trait object) once the bus API is pinned. Capture-queue
/// behavior goes away at that point; emit fans out via broadcast.
#[derive(Debug, Default)]
pub struct EventBus {
    /// In-memory capture of every `emit`-produced `UserEvent`. Drained by
    /// tests; production callers will not observe this field once the
    /// real bus lands.
    captured: std::sync::Mutex<Vec<ark_types::event::AgentEvent>>,
}

impl EventBus {
    /// Append a synthetic `UserEvent` to the capture queue.
    ///
    /// Called by the `emit` op ([`crate::ops::messaging::EmitOp`]); no-op
    /// otherwise. Holds a `std::sync::Mutex`, not a `tokio::sync::Mutex`,
    /// because the lock is only ever held across the `.push()` call —
    /// never across an `.await`.
    pub fn record_user_event(&self, event: ark_types::event::AgentEvent) {
        if let Ok(mut q) = self.captured.lock() {
            q.push(event);
        }
    }

    /// Drain the capture queue, returning every recorded event in push
    /// order and leaving the queue empty. Intended for tests.
    pub fn drain_user_events(&self) -> Vec<ark_types::event::AgentEvent> {
        match self.captured.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(_) => Vec::new(),
        }
    }
}

/// Placeholder for the supervisor handle.
///
/// Real type: `ark_supervisor::SupervisorHandle`. Ops that reach into
/// supervisor state (`reload_scene`, lifecycle probes, ACP routing)
/// take this handle.
///
/// T-11.1: adds [`any_turn_inflight`](SupervisorHandle::any_turn_inflight)
/// and [`scene_reloader`](SupervisorHandle::scene_reloader) for the
/// hot-reload gate. The real supervisor will replace the placeholder
/// inner fields with live ACP session state and a wired
/// [`crate::reload::SceneReloader`].
///
/// TODO(T-5.x): replace with the real supervisor handle once
/// `crates/supervisor/` exposes a public `SupervisorHandle` facade.
#[derive(Debug, Default)]
pub struct SupervisorHandle {
    /// T-ACP.2c stub: number of ACP sessions with in-flight
    /// `session/prompt` turns. `None` = ACP not active / not wired.
    /// The real supervisor sets this from the ACP client's session
    /// tracker.
    inflight_count: std::sync::Mutex<Option<usize>>,

    /// T-11.1: optional scene reloader. Wired by the supervisor at
    /// boot when a scene path is configured.
    reloader: std::sync::Mutex<Option<Arc<crate::reload::SceneReloader>>>,
}

impl SupervisorHandle {
    /// Construct a new placeholder handle (no ACP sessions, no
    /// reloader).
    pub fn new() -> Self {
        Self::default()
    }

    /// T-ACP.2c: check whether any ACP session has a `session/prompt`
    /// awaiting response. Returns `Some(n)` where `n` is the count
    /// of sessions with in-flight turns, or `None` when the ACP
    /// subsystem is not wired.
    ///
    /// Used by the turn-inflight gate in
    /// [`crate::reload::SceneReloader::reload`].
    pub fn any_turn_inflight(&self) -> Option<usize> {
        self.inflight_count
            .lock()
            .expect("inflight mutex poisoned")
            .clone()
    }

    /// Test helper: set the in-flight turn count.
    pub fn set_inflight_count(&self, count: Option<usize>) {
        let mut guard = self.inflight_count.lock().expect("inflight mutex poisoned");
        *guard = count;
    }

    /// T-11.1: install the scene reloader. Called by the supervisor
    /// once the scene is compiled and the reloader is constructed.
    pub fn set_reloader(&self, reloader: Arc<crate::reload::SceneReloader>) {
        let mut guard = self.reloader.lock().expect("reloader mutex poisoned");
        *guard = Some(reloader);
    }

    /// T-11.1: access the installed scene reloader. Returns `None`
    /// when no reloader has been installed (scene-less agents, test
    /// stubs).
    pub fn reloader(&self) -> Option<Arc<crate::reload::SceneReloader>> {
        self.reloader
            .lock()
            .expect("reloader mutex poisoned")
            .clone()
    }
}

/// Handle used by the ACP-interaction core ops (T-ACP.2b) to drive
/// `session/prompt`, `session/cancel`, `session/set_mode`, and
/// `session/request_permission` responses against the engine agent.
///
/// Wraps an optional [`acp_client::AcpClient`]: the supervisor (T-ACP.4a)
/// will replace the `None` with a live client after it spawns the
/// engine subprocess + drives the ACP handshake. Until then the
/// ops return `op/failed` with a clear "ACP client not wired" error —
/// this is deliberately a runtime error rather than a panic so scenes
/// authored against the R7 surface still parse + compile cleanly.
///
/// Wrapped in [`std::sync::Mutex`] so a cross-thread supervisor path
/// can swap the inner handle at runtime (reload, reconnect) without
/// tearing down the whole `IntentContext`. Taking the lock is
/// contention-free in steady state — only the install / swap paths
/// need the write side.
///
/// TODO(T-ACP.4a): replace with a lock-free shared handle once the
/// supervisor wiring is in place.
#[derive(Debug, Default)]
pub struct AcpClientHandle {
    /// Live ACP client, installed by the supervisor once the engine is
    /// up and through `initialize` + `new_session`. `None` before that
    /// point (and in unit tests that don't wire a real engine).
    inner: std::sync::Mutex<Option<Arc<acp_client::AcpClient>>>,
}

impl AcpClientHandle {
    /// Construct an empty handle (no live client yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Swap in a freshly-spawned ACP client. Called by the supervisor
    /// at session boot. Returns the previous client, if any — callers
    /// that take ownership are responsible for draining + dropping it.
    pub fn install(
        &self,
        client: Arc<acp_client::AcpClient>,
    ) -> Option<Arc<acp_client::AcpClient>> {
        let mut guard = self.inner.lock().expect("acp handle mutex poisoned");
        guard.replace(client)
    }

    /// Current ACP client, cloned out by ref-count. `None` until the
    /// supervisor has installed one.
    pub fn get(&self) -> Option<Arc<acp_client::AcpClient>> {
        let guard = self.inner.lock().expect("acp handle mutex poisoned");
        guard.clone()
    }
}

/// Provenance tag for a dispatched intent.
///
/// Scene graph (R11) renders each reaction / keybind with its origin
/// (user scene vs. extension name vs. legacy hook config). `ReactionOrigin`
/// carries that attribution from the compile pipeline through to ops that
/// want to log or gate on it; the dispatcher's telemetry record (T-5.6)
/// renders the `Debug` form into `reaction_origin="…"` so users filtering
/// the `scene::reactions` tracing target can attribute every fired
/// reaction back to its source layer.
///
/// Variants:
///
/// * [`ReactionOrigin::UserScene`] — reaction parsed from the user's
///   scene KDL file. Default for any reaction whose origin isn't
///   explicitly set by a downstream rewriter.
/// * [`ReactionOrigin::HookConfig`] — reaction synthesised by the
///   T-5.7 hook-compat layer from a legacy `[[hooks]]` TOML entry.
///   See [`crate::hook_compat`] for the shape of the synthesised
///   fragment.
///
/// TODO(post-v1): grow richer variants — `Extension { name: String }`,
/// `Keybind { chord: String }` — once the extension merge pass and
/// keybind compile path need to attribute distinctly. The current
/// two-variant enum keeps the dispatcher-visible Debug rendering
/// stable while we add the legacy compat path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionOrigin {
    /// Reaction parsed straight from the user's scene KDL.
    UserScene,
    /// Reaction synthesised from a legacy `[[hooks]]` TOML entry by the
    /// T-5.7 hook-compat layer ([`crate::hook_compat`]).
    HookConfig,
    /// Reaction synthesised from a `plugin { }` block's lifecycle markers
    /// (summon / dismiss / plugin-body `on`) or `subscribes` selectors
    /// by the T-7.3 / T-7.4 / T-7.5 reaction-synthesis pass in
    /// [`crate::plugin_reactions`].
    PluginLifecycle,
}

impl Default for ReactionOrigin {
    fn default() -> Self {
        ReactionOrigin::UserScene
    }
}

// ---------------------------------------------------------------------------
// IntentContext
// ---------------------------------------------------------------------------

/// Runtime context passed to every [`Intent::dispatch`] call.
///
/// Bundles the handles an op might need: the mux for tab/pane ops, the
/// event bus for `emit`, the supervisor for scene lifecycle, plus the
/// identity of the scene that fired the op and its provenance for scene
/// graph attribution. Each handle is `Arc`-wrapped so cloning the
/// context (e.g. to dispatch ops concurrently from a single reaction) is
/// cheap.
///
/// Fields are `pub` rather than accessor-gated — ops are the primary
/// consumers of this struct and need all of it. A private-field + getter
/// layer adds ceremony without adding isolation.
#[derive(Debug, Clone)]
pub struct IntentContext {
    /// Mux handle. See [`MuxPlaceholder`] for the real-type migration.
    pub mux: Arc<MuxPlaceholder>,

    /// Event bus handle. See [`EventBus`] for the real-type migration.
    pub bus: Arc<EventBus>,

    /// Supervisor handle. See [`SupervisorHandle`] for the real-type
    /// migration.
    pub supervisor: Arc<SupervisorHandle>,

    /// ACP client handle (T-ACP.2b). Used by the ACP-interaction ops
    /// (`prompt`, `acp/cancel`, `acp/permit`, `set_mode`). `None`-
    /// wrapped until the supervisor installs a live client at
    /// T-ACP.4a; see [`AcpClientHandle`].
    pub acp: Arc<AcpClientHandle>,

    /// Identity of the scene whose reaction / keybind fired this op
    /// (R11 attribution, R14 hot-reload delta detection). Real type;
    /// not a placeholder.
    pub scene_id: SceneId,

    /// Where the reaction came from (user scene vs. extension). See
    /// [`ReactionOrigin`] for the real-type migration.
    pub origin: ReactionOrigin,

    /// Cascade depth of the current emit chain (T-5.4).
    ///
    /// `0` at the top-level dispatch (triggered by a broadcast
    /// `AgentEvent`); every reaction's `emit` op increments this when
    /// building the context for the child dispatch. The registry-
    /// wrapping dispatcher compares this against
    /// [`IntentContext::max_cascade_depth`] and refuses to re-dispatch
    /// when the next hop would exceed the bound.
    pub cascade_depth: u32,

    /// Per-scene cascade-depth bound (R4 `max-cascade-depth=<N>`;
    /// default 4 when the scene attribute is absent).
    ///
    /// Stored on every context so child emits that construct a new
    /// context inherit the same cap. This is a value, not a handle —
    /// it's a scene-file-wide config and doesn't change across
    /// dispatches within one scene instance.
    pub max_cascade_depth: u32,
}

impl IntentContext {
    /// Construct an `IntentContext` with the supplied scene identity
    /// and default-initialised placeholder handles. Tests and early
    /// wiring use this; real production call sites build the context
    /// from live handles directly.
    pub fn placeholder(scene_id: SceneId) -> Self {
        Self {
            mux: Arc::new(MuxPlaceholder),
            bus: Arc::new(EventBus::default()),
            supervisor: Arc::new(SupervisorHandle::new()),
            acp: Arc::new(AcpClientHandle::new()),
            scene_id,
            origin: ReactionOrigin::default(),
            cascade_depth: 0,
            max_cascade_depth: DEFAULT_MAX_CASCADE_DEPTH,
        }
    }

    /// Produce a child context for a cascaded dispatch — the one
    /// created when an `emit` op's synthetic `UserEvent` feeds back
    /// into the reaction dispatcher. Increments [`cascade_depth`] by
    /// one; all other fields (including [`max_cascade_depth`]) are
    /// preserved.
    ///
    /// Returns `None` when the increment would exceed
    /// [`max_cascade_depth`] — the caller (the reactions dispatcher,
    /// T-5.3) logs an error and drops the cascade at that point.
    pub fn cascade_child(&self) -> Option<Self> {
        let next_depth = self.cascade_depth.saturating_add(1);
        if next_depth > self.max_cascade_depth {
            return None;
        }
        Some(Self {
            cascade_depth: next_depth,
            ..self.clone()
        })
    }
}

/// Default cascade-depth bound per R4 acceptance criterion.
///
/// Scenes override via `scene "<name>" max-cascade-depth=<N>`.
pub const DEFAULT_MAX_CASCADE_DEPTH: u32 = 4;

// ---------------------------------------------------------------------------
// Intent trait + object-safe wrapper
// ---------------------------------------------------------------------------

/// A single registered op.
///
/// Implementations declare the op's static [`NAME`](Intent::NAME) and
/// the facet-derived [`Args`](Intent::Args) struct; the registry routes
/// `dispatch_dyn(name, kdl_args, ctx)` through a matching
/// implementation.
///
/// `Args` is required to impl `Facet<'static>` (facet-kdl's derive
/// constraint); it is the wrapper type fed directly to
/// `facet_kdl::from_str`, i.e. the "document root" for that op's KDL
/// fragment. For op `foo` with args `foo name="bar"`, the `Args`
/// struct typically looks like:
///
/// ```ignore
/// #[derive(Facet, Debug)]
/// struct FooDoc {
///     #[facet(kdl::child)]
///     foo: FooArgs,
/// }
///
/// #[derive(Facet, Debug)]
/// struct FooArgs {
///     #[facet(kdl::property)]
///     name: String,
/// }
/// ```
///
/// … and the op's `Args` is `FooDoc`. This matches the facet-kdl pattern
/// `crates/scene/src/ast.rs` already uses for `SceneDoc`/`SceneNode`.
#[async_trait]
pub trait Intent: Send + Sync + 'static {
    /// Facet-derived arg struct. MUST be a facet-kdl "document wrapper"
    /// whose single `#[facet(kdl::child)]` field matches the op's KDL
    /// node name.
    type Args: Facet<'static> + Send + 'static;

    /// Stable op identifier. Convention: `"ark.core.<verb>"` for
    /// built-ins (R7), `"<ext>.<verb>"` for extension ops.
    const NAME: &'static str;

    /// Run the op. Return `Some(value)` to feed the result back into
    /// the reaction cascade (CEL `$ret`, R7 pipe / emit chaining) or
    /// `None` for void ops.
    async fn dispatch(
        &self,
        args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError>;
}

/// Object-safe erased shim behind [`Intent`].
///
/// `Intent::Args` is an associated type, so `dyn Intent` is not
/// constructible. `DynIntent` flattens the interface: it exposes the op
/// name + an `async fn dispatch_dyn` that takes the raw `KdlNode` and
/// performs the args deserialisation internally, producing only the
/// erased return type. Every `T: Intent` gets a blanket
/// `impl DynIntent for T` below.
///
/// This trait is an implementation detail — scene authors impl
/// [`Intent`] directly.
#[async_trait]
pub trait DynIntent: Send + Sync {
    /// Op name (mirrors `Intent::NAME`). The registry stores ops under
    /// this key.
    fn name(&self) -> &'static str;

    /// Deserialize the KDL node into `Self::Args`, then dispatch.
    async fn dispatch_dyn(
        &self,
        kdl_args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError>;
}

#[async_trait]
impl<T: Intent> DynIntent for T {
    fn name(&self) -> &'static str {
        T::NAME
    }

    async fn dispatch_dyn(
        &self,
        kdl_args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        // facet-kdl only exposes `from_str` / `from_slice`; there is no
        // per-`KdlNode` entry point. We round-trip through the KDL 2.0
        // string rendering (`KdlNode: Display`) so the op's
        // facet-derived `Args` sees a well-formed single-node document.
        //
        // Cost:
        //   * one extra allocation per dispatch (the rendered string);
        //   * source spans in the produced `KdlDeserializeError`
        //     point into this rendered string, not the original
        //     scene file. For v1 that's fine — `ark scene check`
        //     catches args-parse errors at compile time, so runtime
        //     dispatch rarely hits `ArgsInvalid`. When it does, the
        //     compile pipeline wraps the error with the real
        //     `NamedSource` before surfacing.
        //
        // TODO(T-4.x optimisation): when `facet-kdl` exposes a
        // `from_node` / `from_kdl_node` API we drop the round-trip;
        // track upstream.
        let rendered = kdl_args.to_string();
        let args = facet_kdl::from_str::<T::Args>(&rendered)
            .map_err(|e| IntentError::args_invalid(T::NAME, e.to_string()))?;
        T::dispatch(self, args, ctx).await
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Thread-safe registry of ops.
///
/// Cloning is cheap (`Arc` handle + inner `RwLock`). The compile
/// pipeline registers core ops + extension-contributed ops at session
/// spawn; dispatch paths hold the registry read-locked only long
/// enough to clone out the `Arc<dyn DynIntent>` for the matched op,
/// then release the lock before awaiting the op's own future.
#[derive(Clone, Default)]
pub struct IntentRegistry {
    inner: Arc<RwLock<HashMap<&'static str, Arc<dyn DynIntent>>>>,
}

impl std::fmt::Debug for IntentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // We can't lock in Debug (sync context, might block async
        // executors), so just name the type + note the trait-object
        // store. Tests that need the count use `IntentRegistry::len`.
        f.debug_struct("IntentRegistry")
            .field("inner", &"Arc<RwLock<HashMap<&'static str, Arc<dyn DynIntent>>>>")
            .finish()
    }
}

impl IntentRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an op. Subsequent calls to `dispatch_dyn(I::NAME, …)`
    /// route to this implementation.
    ///
    /// Re-registering under the same `NAME` replaces the previous
    /// implementation. The v1 spec treats name collisions as a compile
    /// error at the scope pass; the registry itself is tolerant so
    /// tests + hot-reload paths can swap ops freely.
    pub async fn register<I: Intent>(&self, op: I) {
        let mut guard = self.inner.write().await;
        guard.insert(I::NAME, Arc::new(op));
    }

    /// Current number of registered ops. Useful for tests.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Whether the registry is empty. Useful for tests.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// Dispatch by name with a raw `KdlNode` of args.
    ///
    /// Returns `Ok(Some(value))` when the op ran and produced a value,
    /// `Ok(None)` when the op ran and produced no value, or an error
    /// per [`IntentError`]. The registry's read lock is released
    /// before the op's `dispatch` future is awaited so long-running
    /// ops don't block concurrent dispatches.
    pub async fn dispatch_dyn(
        &self,
        name: &str,
        kdl_args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let op = {
            let guard = self.inner.read().await;
            guard
                .get(name)
                .cloned()
                .ok_or_else(|| IntentError::unknown(name))?
        };
        op.dispatch_dyn(kdl_args, ctx).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -- Test op: `foo arg="bar"` -------------------------------------------

    /// Args-wrapper struct: the actual node body.
    #[derive(Facet, Debug)]
    struct FooArgs {
        /// `arg=` property on the `foo` node.
        #[facet(facet_kdl::property)]
        arg: String,
    }

    /// facet-kdl requires the deserialiser input to be a "document"
    /// with one or more `#[facet(kdl::child)]` fields. `FooDoc` is
    /// that wrapper for the `foo` op.
    #[derive(Facet, Debug)]
    struct FooDoc {
        /// The `foo` node itself.
        #[facet(facet_kdl::child)]
        foo: FooArgs,
    }

    /// Test op: records how many times it fired + echoes its arg
    /// back as a JSON string.
    struct FooOp {
        fired: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Intent for FooOp {
        type Args = FooDoc;
        const NAME: &'static str = "test.foo";

        async fn dispatch(
            &self,
            args: Self::Args,
            _ctx: &IntentContext,
        ) -> Result<Option<IntentValue>, IntentError> {
            self.fired.fetch_add(1, Ordering::SeqCst);
            Ok(Some(serde_json::Value::String(args.foo.arg)))
        }
    }

    // -- Test op: always-fails --------------------------------------------

    /// Args for the always-failing op — no properties, just a name.
    #[derive(Facet, Debug)]
    struct BombArgs {}

    #[derive(Facet, Debug)]
    struct BombDoc {
        #[facet(facet_kdl::child)]
        #[allow(dead_code)]
        bomb: BombArgs,
    }

    struct BombOp;

    #[derive(Debug, Error, Diagnostic)]
    #[error("bomb went off")]
    #[diagnostic(code = "test/bomb")]
    struct BombError;

    #[async_trait]
    impl Intent for BombOp {
        type Args = BombDoc;
        const NAME: &'static str = "test.bomb";

        async fn dispatch(
            &self,
            _args: Self::Args,
            _ctx: &IntentContext,
        ) -> Result<Option<IntentValue>, IntentError> {
            Err(IntentError::failed(Self::NAME, Box::new(BombError)))
        }
    }

    // -- helpers ----------------------------------------------------------

    fn test_ctx() -> IntentContext {
        let scene_id =
            SceneId::from_bytes(PathBuf::from("/tmp/scene.kdl"), b"scene \"test\" { }");
        IntentContext::placeholder(scene_id)
    }

    /// Build a fresh `KdlNode` by parsing a single-line KDL fragment
    /// and extracting the first (only) node. Used in tests because
    /// hand-building KdlNode via its builder API for every shape is
    /// noisier than the string round-trip.
    fn node_from(src: &str) -> KdlNode {
        let doc: kdl::KdlDocument = src.parse().expect("test KDL parses");
        doc.nodes().first().expect("at least one node").clone()
    }

    // -- dispatch_dyn happy path ------------------------------------------

    #[tokio::test]
    async fn register_and_dispatch_parses_args_and_returns_value() {
        let registry = IntentRegistry::new();
        let fired = Arc::new(AtomicUsize::new(0));
        registry
            .register(FooOp {
                fired: fired.clone(),
            })
            .await;
        assert_eq!(registry.len().await, 1);

        let node = node_from(r#"foo arg="bar""#);
        let ctx = test_ctx();

        let ret = registry
            .dispatch_dyn("test.foo", &node, &ctx)
            .await
            .expect("dispatch ok");

        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(ret, Some(serde_json::Value::String("bar".to_string())));
    }

    // -- unknown op --------------------------------------------------------

    #[tokio::test]
    async fn unknown_op_returns_unknown_error() {
        let registry = IntentRegistry::new();
        let node = node_from(r#"nope"#);
        let ctx = test_ctx();

        let err = registry
            .dispatch_dyn("test.missing", &node, &ctx)
            .await
            .expect_err("must error on unknown op");

        match err {
            IntentError::Unknown { name } => assert_eq!(name, "test.missing"),
            other => panic!("expected Unknown, got {other:?}"),
        }

        // Error code matches the `op/unknown` spec in R12.
        let err = IntentError::unknown("x");
        let code = err.code().map(|c| c.to_string()).unwrap_or_default();
        assert_eq!(code, "op/unknown");
    }

    // -- args-parse failure ------------------------------------------------

    #[tokio::test]
    async fn args_parse_failure_returns_args_invalid() {
        let registry = IntentRegistry::new();
        let fired = Arc::new(AtomicUsize::new(0));
        registry
            .register(FooOp {
                fired: fired.clone(),
            })
            .await;

        // `foo` requires `arg=<str>`; this node is missing it.
        let node = node_from(r#"foo"#);
        let ctx = test_ctx();

        let err = registry
            .dispatch_dyn("test.foo", &node, &ctx)
            .await
            .expect_err("must error on missing arg");

        match err {
            IntentError::ArgsInvalid { name, .. } => assert_eq!(name, "test.foo"),
            other => panic!("expected ArgsInvalid, got {other:?}"),
        }
        // The op's dispatch must NOT have fired — args failed first.
        assert_eq!(fired.load(Ordering::SeqCst), 0);

        // Error code matches the `op/args-invalid` spec in R12.
        let err = IntentError::args_invalid("x", "y");
        let code = err.code().map(|c| c.to_string()).unwrap_or_default();
        assert_eq!(code, "op/args-invalid");
    }

    // -- op-failed surfaces as op/failed -----------------------------------

    #[tokio::test]
    async fn op_failure_surfaces_as_failed() {
        let registry = IntentRegistry::new();
        registry.register(BombOp).await;

        let node = node_from(r#"bomb"#);
        let ctx = test_ctx();

        let err = registry
            .dispatch_dyn("test.bomb", &node, &ctx)
            .await
            .expect_err("must error");

        match &err {
            IntentError::Failed { name, .. } => assert_eq!(name, "test.bomb"),
            other => panic!("expected Failed, got {other:?}"),
        }
        let code = err.code().map(|c| c.to_string()).unwrap_or_default();
        assert_eq!(code, "op/failed");
    }

    // -- thread-safety: concurrent dispatches --------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_dispatches_do_not_deadlock() {
        let registry = IntentRegistry::new();
        let fired = Arc::new(AtomicUsize::new(0));
        registry
            .register(FooOp {
                fired: fired.clone(),
            })
            .await;

        let mut handles = Vec::new();
        for i in 0..32 {
            let reg = registry.clone();
            let ctx = test_ctx();
            let node = node_from(&format!(r#"foo arg="n{i}""#));
            handles.push(tokio::spawn(async move {
                reg.dispatch_dyn("test.foo", &node, &ctx).await
            }));
        }

        for h in handles {
            let r = h.await.expect("task joined");
            assert!(r.is_ok(), "each dispatch must succeed: {r:?}");
        }
        assert_eq!(fired.load(Ordering::SeqCst), 32);
    }

    // -- re-register overwrites --------------------------------------------

    #[tokio::test]
    async fn reregister_replaces_previous() {
        let registry = IntentRegistry::new();
        let fired_a = Arc::new(AtomicUsize::new(0));
        let fired_b = Arc::new(AtomicUsize::new(0));
        registry
            .register(FooOp {
                fired: fired_a.clone(),
            })
            .await;
        registry
            .register(FooOp {
                fired: fired_b.clone(),
            })
            .await;
        assert_eq!(registry.len().await, 1, "same NAME collapses to one slot");

        let node = node_from(r#"foo arg="x""#);
        let ctx = test_ctx();
        registry
            .dispatch_dyn("test.foo", &node, &ctx)
            .await
            .expect("ok");

        assert_eq!(fired_a.load(Ordering::SeqCst), 0, "first op replaced");
        assert_eq!(fired_b.load(Ordering::SeqCst), 1, "second op fired");
    }
}
