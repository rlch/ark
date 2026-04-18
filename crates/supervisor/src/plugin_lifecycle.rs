//! Plugin lifecycle manager — T-7.2.
//!
//! Implements the supervisor-side state machine that tracks every scene-
//! declared plugin's mount status across its session. Responsible for
//!
//! * enumerating the set of [`Lifecycle::Always`] plugins at session spawn
//!   and requesting mount via the scene's `mount_plugin` op surface (never
//!   bypassing the intent registry);
//! * maintaining a `{name -> MountState}` map the rest of the supervisor
//!   can read/observe for audit/telemetry purposes;
//! * surfacing mount failures through two channels:
//!   * a structured `tracing::error!` line tagged `plugin/mount-failed`,
//!   * a synthetic [`AgentEvent::UserEvent`] (`ark.plugin.failed`) broadcast
//!     on the supervisor event bus so scene reactions can observe the
//!     failure and react (e.g. set status, alt-mount).
//!
//! The manager itself is **transport-agnostic**: it does not invoke
//! `zellij launch-or-focus-plugin` directly. Instead it walks the plugin
//! declarations and dispatches the corresponding `mount_plugin` op through
//! the scene's [`IntentRegistry`]. Today those ops are stubs
//! (`crates/scene/src/ops/plugins.rs` logs + returns `Ok(None)`); when the
//! real mux handle arrives on [`IntentContext`] the manager inherits the
//! real behaviour automatically. The lifecycle/state tracking machinery is
//! real regardless — it is what enables the reaction synthesis path
//! (T-7.3 / T-7.4 / T-7.5) to know whether a plugin is already up.
//!
//! ## Mount outcome surface
//!
//! Every mount attempt returns a [`MountOutcome`] describing the post-hoc
//! state. Successful mounts transition `Dormant -> Mounted { pane_id }`;
//! failures transition to `Failed { reason }` and fan out both the
//! tracing log and the `ark.plugin.failed` bus event.
//!
//! `pane_id` is **synthesised** at this tier because the mux stub has no
//! real pane identifier to hand back. The manager assigns a monotonic
//! `"placeholder:<n>"` id per successful mount so downstream observers
//! (T-7.3 / T-7.4) can distinguish "some pane" from "no pane" without
//! needing the real zellij reply channel — the contract for real pane
//! ids lands alongside the real mux handle (TODO below).
//!
//! TODO(post-v0.1): when `IntentContext::mux` is replaced with the real
//! mux handle, replace the placeholder pane-id synthesis with the actual
//! pane id returned by `launch-or-focus-plugin`. The `MountState` variant
//! shape does not change — only the value in `pane_id` becomes authoritative.

use std::collections::BTreeMap;
use std::sync::Arc;

use ark_scene::intent::{IntentContext, IntentRegistry};

/// Plugin lifecycle classification.
///
/// V3 migration: v3 models plugins as extensions with bindings.
/// This enum is retained as a supervisor-local type for the lifecycle
/// manager's state machine. Once the extension binding system is
/// fully wired, this will be replaced by the binding's protocol/render
/// mode combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Plugin is always mounted at session spawn.
    Always,
    /// Plugin is mounted on demand via a user event.
    Summon,
    /// Plugin is mounted when a specific event fires.
    EventMount,
}

/// V3-compatible plugin declaration.
///
/// Minimal shim carrying the fields the lifecycle manager needs.
/// V2's `PluginDecl<'a>` was borrowed from the AST; this version owns
/// its strings since v3 doesn't have `PluginNode` in the scene AST.
#[derive(Debug, Clone)]
pub struct PluginDecl {
    /// Plugin name.
    pub name: String,
    /// Plugin lifecycle classification.
    pub lifecycle: Lifecycle,
    /// Plugin source URI.
    pub source: String,
    /// Mount target (e.g. `"floating"`, `"status-bar"`).
    pub mount: Option<String>,
}
use ark_types::EventSink;
use ark_types::event::{CoreEvent, ExtEvent};
#[cfg(test)]
use kdl::KdlDocument;
use kdl::{KdlEntry, KdlNode, KdlValue};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Provenance tag used in the synthetic `ark.plugin.failed` event so
/// scene reactions can filter on source without string parsing.
///
/// Mirrors the pattern used by other supervisor-synthesised events
/// (`source: "core"`).
pub const FAILURE_EVENT_SOURCE: &str = "core";

/// Synthetic `UserEvent` name emitted when a plugin mount fails.
///
/// Scene reactions subscribe via `UserEvent:ark.plugin.failed` to react
/// to mount failures — e.g. `set_status text="picker unavailable"` or
/// `mount_plugin name="fallback-picker"`.
pub const PLUGIN_FAILED_EVENT: &str = "ark.plugin.failed";

/// Runtime mount status for a single plugin.
///
/// Transitions:
///
/// ```text
/// Dormant ── mount_plugin succeeds ──▶ Mounted { pane_id }
/// Dormant ── mount_plugin errors ───▶ Failed { reason }
/// Mounted ── unmount_plugin ok ──────▶ Dormant
/// Failed  ── retry via reaction ────▶ Dormant → Mounted | Failed
/// ```
///
/// A plugin with `Lifecycle::Always` starts `Dormant` and is expected to
/// transition to `Mounted` during session spawn (see
/// [`PluginLifecycleManager::mount_always_on`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountState {
    /// Plugin is known to the scene but has no live pane. The default
    /// starting state for every summon / event-mount plugin, and the
    /// state an always-on plugin occupies between supervisor startup
    /// and the first `mount_always_on` call.
    Dormant,

    /// Plugin has been mounted through the intent registry's
    /// `mount_plugin` op. `pane_id` identifies the zellij pane (or a
    /// synthetic placeholder while the stub op is in place).
    Mounted {
        /// Zellij pane id — stringly-typed because zellij returns
        /// `PaneId::Plugin(u32)` / `PaneId::Terminal(u32)` and the
        /// supervisor preserves the distinction verbatim. Placeholder
        /// form is `"placeholder:<n>"` under the stub.
        pane_id: String,
    },

    /// Mount failed. `reason` carries the error message surfaced via
    /// the `plugin/mount-failed` log line and the `ark.plugin.failed`
    /// UserEvent.
    Failed {
        /// Human-readable error message. Preserved verbatim so scene
        /// reactions (and the picker UI) can display the original
        /// failure cause.
        reason: String,
    },
}

impl MountState {
    /// Short stable slug for structured logging + telemetry attribution.
    pub const fn as_str(&self) -> &'static str {
        match self {
            MountState::Dormant => "dormant",
            MountState::Mounted { .. } => "mounted",
            MountState::Failed { .. } => "failed",
        }
    }

    /// Whether the plugin is currently mounted (has a live pane).
    pub const fn is_mounted(&self) -> bool {
        matches!(self, MountState::Mounted { .. })
    }
}

/// Outcome of a single mount attempt.
///
/// `mount_always_on` returns one per always-on plugin so the caller can
/// count failures / report telemetry without re-walking the state map.
#[derive(Debug, Clone)]
pub enum MountOutcome {
    /// The plugin was successfully mounted. `pane_id` matches the value
    /// stored on the new [`MountState::Mounted`].
    Mounted {
        /// Plugin name.
        name: String,
        /// Pane id (or placeholder) used for the new mount.
        pane_id: String,
    },

    /// The mount attempt failed. The plugin's state is now
    /// [`MountState::Failed`] and an `ark.plugin.failed` event has been
    /// broadcast on the event bus.
    Failed {
        /// Plugin name.
        name: String,
        /// Error reason surfaced via the failure event.
        reason: String,
    },

    /// The plugin was already mounted before this call — no work done.
    /// `launch-or-focus-plugin` is idempotent on the zellij side so we
    /// treat a repeated request as a successful focus and return the
    /// existing pane id unchanged.
    AlreadyMounted {
        /// Plugin name.
        name: String,
        /// Existing pane id.
        pane_id: String,
    },
}

impl MountOutcome {
    /// Whether the outcome represents a state where the plugin is live.
    pub const fn is_mounted(&self) -> bool {
        matches!(
            self,
            MountOutcome::Mounted { .. } | MountOutcome::AlreadyMounted { .. }
        )
    }
}

/// Supervisor-owned plugin lifecycle tracker.
///
/// Clone-cheap (all state lives behind an `Arc<Mutex<..>>`). The clone
/// handle is what gets threaded into the reaction-synthesis layer so
/// T-7.3 / T-7.4 reactions can query the current state before deciding
/// whether a `mount_plugin` is a genuine mount or a focus-only no-op.
#[derive(Clone, Debug)]
pub struct PluginLifecycleManager {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    /// Deterministic map for reproducible iteration order in tests and
    /// `ark scene graph` listings.
    state: BTreeMap<String, MountState>,
    /// Monotonic counter for synthesising placeholder pane ids under
    /// the stub mux surface. Removed when the real mux handle lands.
    pane_counter: u64,
}

impl PluginLifecycleManager {
    /// Construct an empty manager.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state: BTreeMap::new(),
                pane_counter: 0,
            })),
        }
    }

    /// Current [`MountState`] for `name`. Returns `None` when the plugin
    /// has never been seeded into the map.
    pub async fn state(&self, name: &str) -> Option<MountState> {
        self.inner.lock().await.state.get(name).cloned()
    }

    /// Snapshot the full state map for inspection (ark scene graph,
    /// tests). Order is deterministic (`BTreeMap`).
    pub async fn snapshot(&self) -> Vec<(String, MountState)> {
        self.inner
            .lock()
            .await
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Record a plugin with an initial `Dormant` state.
    ///
    /// Idempotent — a second call with the same name leaves the
    /// existing state untouched. Used by the compile-time wiring layer
    /// to ensure every scene-declared plugin has a starting slot even
    /// before its lifecycle runs.
    pub async fn seed_dormant(&self, name: &str) {
        let mut inner = self.inner.lock().await;
        inner
            .state
            .entry(name.to_string())
            .or_insert(MountState::Dormant);
    }

    /// Force the recorded state for `name` to `Failed { reason }` and
    /// fan out the `ark.plugin.failed` event.
    ///
    /// Useful for wasm load / version-mismatch failures that surface
    /// outside the `mount_plugin` op path (e.g. the zellij layout
    /// renderer rejects the cartridge before the op fires).
    pub async fn record_failure(
        &self,
        name: &str,
        reason: impl Into<String>,
        event_bus: &EventSink,
    ) {
        let reason = reason.into();
        {
            let mut inner = self.inner.lock().await;
            inner.state.insert(
                name.to_string(),
                MountState::Failed {
                    reason: reason.clone(),
                },
            );
        }
        emit_failure_event(event_bus, name, &reason);
        error!(
            target = "plugin",
            code = "plugin/mount-failed",
            plugin = name,
            reason = %reason,
            "plugin mount failed",
        );
    }

    /// Record a successful mount for `name` and stash the pane id.
    ///
    /// Used both by [`mount_always_on`] and by the reaction-synthesis
    /// layer (T-7.3 / T-7.4) when a summon/event-mount op resolves.
    pub async fn record_mounted(&self, name: &str, pane_id: impl Into<String>) {
        let pane_id = pane_id.into();
        let mut inner = self.inner.lock().await;
        inner
            .state
            .insert(name.to_string(), MountState::Mounted { pane_id });
    }

    /// Record `name` as dormant again — used after `unmount_plugin` /
    /// `close-pane` succeeds.
    pub async fn record_dormant(&self, name: &str) {
        let mut inner = self.inner.lock().await;
        inner.state.insert(name.to_string(), MountState::Dormant);
    }

    /// Walk `decls` and mount every plugin whose lifecycle is
    /// [`Lifecycle::Always`].
    ///
    /// Non-always plugins are seeded as `Dormant` without firing an op
    /// — they mount lazily via their own reaction paths. For every
    /// always-on plugin the manager dispatches `ark.core.mount_plugin`
    /// through the intent registry with the plugin's configured `at` /
    /// `into` overrides drawn from the decl's `mount` child.
    ///
    /// Returns one [`MountOutcome`] per always-on plugin, in iteration
    /// order (source order of the scene's plugin declarations). The
    /// supervisor can tally failures without re-walking the state map.
    pub async fn mount_always_on(
        &self,
        decls: &[PluginDecl],
        registry: &IntentRegistry,
        ctx: &IntentContext,
        event_bus: &EventSink,
    ) -> Vec<MountOutcome> {
        let mut outcomes = Vec::new();
        for decl in decls {
            // Seed every plugin so downstream lookups have an entry.
            self.seed_dormant(&decl.name).await;
            if decl.lifecycle != Lifecycle::Always {
                continue;
            }

            // Idempotency: if someone else already mounted the plugin
            // (hot-reload race, re-entry), skip and surface AlreadyMounted.
            if let Some(MountState::Mounted { pane_id }) = self.state(&decl.name).await {
                debug!(
                    target = "plugin",
                    plugin = %decl.name,
                    pane_id = %pane_id,
                    "mount_always_on: already mounted, skipping",
                );
                outcomes.push(MountOutcome::AlreadyMounted {
                    name: decl.name.to_string(),
                    pane_id,
                });
                continue;
            }

            let node = build_mount_plugin_node(decl);
            match registry.dispatch("ark.core.mount_plugin", &node, ctx).await {
                Ok(_value) => {
                    // The stub op does not hand back a pane id. Synthesise
                    // a placeholder so downstream observers can still
                    // distinguish mounts — replaced with the real id when
                    // the mux handle lands.
                    let pane_id = {
                        let mut inner = self.inner.lock().await;
                        inner.pane_counter = inner.pane_counter.saturating_add(1);
                        format!("placeholder:{}", inner.pane_counter)
                    };
                    self.record_mounted(&decl.name, pane_id.clone()).await;
                    info!(
                        target = "plugin",
                        plugin = %decl.name,
                        pane_id = %pane_id,
                        lifecycle = "always",
                        "plugin mounted",
                    );
                    outcomes.push(MountOutcome::Mounted {
                        name: decl.name.to_string(),
                        pane_id,
                    });
                }
                Err(err) => {
                    let reason = err.to_string();
                    self.record_failure(&decl.name, reason.clone(), event_bus)
                        .await;
                    outcomes.push(MountOutcome::Failed {
                        name: decl.name.to_string(),
                        reason,
                    });
                }
            }
        }
        outcomes
    }
}

impl Default for PluginLifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a synthetic `mount_plugin name="<n>" at="<target>" into="<slot>"`
/// KDL node matching the `MountPluginArgs` facet schema.
///
/// `at` / `into` are only added when the decl's `mount` child specifies
/// them — the op's facet derive treats absent properties as `None`.
fn build_mount_plugin_node(decl: &PluginDecl) -> KdlNode {
    let mut node = KdlNode::new("mount_plugin");
    node.push(KdlEntry::new_prop(
        "name",
        KdlValue::String(decl.name.clone()),
    ));
    if let Some(target) = &decl.mount {
        node.push(KdlEntry::new_prop("at", KdlValue::String(target.clone())));
    }
    // `into` is not captured in the lowered PluginDecl — the scene-root
    // `plugin { }` node stores it on `MountNode::into`, but that field
    // is not lifted into `PluginDecl` at this tier. Wiring it through
    // is a mechanical follow-up once the PluginDecl lifting grows the
    // `into` slot. For v0.1 the op receives only `name` + `at`.
    node
}

/// Broadcast a synthetic `ark.plugin.failed` UserEvent on the bus.
///
/// Payload shape:
///
/// ```json
/// { "plugin": "<name>", "reason": "<message>" }
/// ```
///
/// Best-effort: if the bus has no subscribers the send returns Err, which
/// we log-and-swallow — the tracing error line above already carried the
/// failure out-of-band.
fn emit_failure_event(event_bus: &EventSink, plugin: &str, reason: &str) {
    let payload = json!({
        "plugin": plugin,
        "reason": reason,
    });
    let event = CoreEvent::Ext(ExtEvent {
        ext: FAILURE_EVENT_SOURCE.to_string(),
        kind: PLUGIN_FAILED_EVENT.to_string(),
        payload,
    });
    if let Err(err) = event_bus.send(event) {
        warn!(
            target = "plugin",
            plugin,
            error = %err,
            "plugin.failed event had no subscribers",
        );
    }
}

/// Best-effort KDL-node builder used by tests that need to synthesise
/// `plugin { }` fragments without round-tripping through facet-kdl.
#[cfg(test)]
fn node_from_source(src: &str) -> KdlNode {
    let doc: KdlDocument = src.parse().expect("test KDL parses");
    doc.nodes().first().cloned().expect("at least one node")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_scene::id::SceneId;
    use ark_scene::ops::register_core_ops;
    use ark_types::channel;
    use std::path::PathBuf;

    fn test_ctx() -> IntentContext {
        IntentContext::new(
            SceneId::new(&PathBuf::from("/tmp/scene.kdl"), b"scene \"test\" { }"),
            "scene",
        )
    }

    fn make_plugin_decl(name: &str, lifecycle: Lifecycle) -> PluginDecl {
        PluginDecl {
            name: name.to_string(),
            lifecycle,
            source: format!("shipped:{name}"),
            mount: Some("floating".to_string()),
        }
    }

    #[tokio::test]
    async fn seed_dormant_is_idempotent() {
        let mgr = PluginLifecycleManager::new();
        mgr.seed_dormant("foo").await;
        mgr.seed_dormant("foo").await;
        assert_eq!(mgr.state("foo").await, Some(MountState::Dormant));
    }

    #[tokio::test]
    async fn record_mounted_transitions_state() {
        let mgr = PluginLifecycleManager::new();
        mgr.seed_dormant("foo").await;
        mgr.record_mounted("foo", "pane:1").await;
        assert_eq!(
            mgr.state("foo").await,
            Some(MountState::Mounted {
                pane_id: "pane:1".to_string()
            })
        );
    }

    #[tokio::test]
    async fn record_dormant_clears_pane_id() {
        let mgr = PluginLifecycleManager::new();
        mgr.record_mounted("foo", "pane:1").await;
        mgr.record_dormant("foo").await;
        assert_eq!(mgr.state("foo").await, Some(MountState::Dormant));
    }

    #[tokio::test]
    async fn record_failure_fans_out_user_event() {
        let mgr = PluginLifecycleManager::new();
        let (tx, mut rx) = channel(8);
        mgr.record_failure("foo", "wasm load error", &tx).await;
        assert!(matches!(
            mgr.state("foo").await,
            Some(MountState::Failed { .. })
        ));
        let event = rx.try_recv().expect("one event");
        match event {
            CoreEvent::Ext(ext) => {
                assert_eq!(ext.kind, PLUGIN_FAILED_EVENT);
                assert_eq!(ext.ext, FAILURE_EVENT_SOURCE);
                assert_eq!(ext.payload["plugin"], "foo");
                assert_eq!(ext.payload["reason"], "wasm load error");
            }
            other => panic!("expected CoreEvent::Ext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mount_always_on_mounts_only_always_lifecycle() {
        let mut registry = IntentRegistry::new();
        register_core_ops(&mut registry);
        let ctx = test_ctx();
        let (tx, _rx) = channel(8);
        let mgr = PluginLifecycleManager::new();

        let always_decl = make_plugin_decl("always-1", Lifecycle::Always);
        let summon_decl = make_plugin_decl("summon-1", Lifecycle::Summon);
        let event_mount_decl = make_plugin_decl("event-mount-1", Lifecycle::EventMount);

        let outcomes = mgr
            .mount_always_on(
                &[always_decl, summon_decl, event_mount_decl],
                &registry,
                &ctx,
                &tx,
            )
            .await;

        // V3 migration: `ark.core.mount_plugin` is not registered in v3's
        // `register_core_ops` (plugins are modelled as extensions in v3).
        // The dispatch of `ark.core.mount_plugin` therefore returns
        // op/unknown, producing a Failed outcome. In production
        // `CompiledScene::plugin_decls()` returns empty so mount_always_on
        // is a no-op; this test exercises the lifecycle manager's
        // failure-surface directly.
        //
        // Only the always-on plugin produced an outcome (summon + event-mount
        // are seeded as Dormant without dispatching any op).
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            MountOutcome::Failed { name, .. } => {
                assert_eq!(name, "always-1");
            }
            other => panic!("expected Failed (mount_plugin not in v3 registry), got {other:?}"),
        }

        // Every plugin should be seeded in the map.
        // always-1 ended in Failed state (op unknown), non-always are Dormant.
        assert!(matches!(
            mgr.state("always-1").await,
            Some(MountState::Failed { .. })
        ));
        assert_eq!(mgr.state("summon-1").await, Some(MountState::Dormant));
        assert_eq!(mgr.state("event-mount-1").await, Some(MountState::Dormant));
    }

    #[tokio::test]
    async fn mount_always_on_returns_already_mounted_when_state_is_already_mounted() {
        let mut registry = IntentRegistry::new();
        register_core_ops(&mut registry);
        let ctx = test_ctx();
        let (tx, _rx) = channel(8);
        let mgr = PluginLifecycleManager::new();

        let decl = make_plugin_decl("already-up", Lifecycle::Always);

        // Pre-seed as mounted — subsequent mount_always_on should
        // short-circuit and return AlreadyMounted.
        mgr.record_mounted("already-up", "pane:42").await;

        let outcomes = mgr.mount_always_on(&[decl], &registry, &ctx, &tx).await;
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            MountOutcome::AlreadyMounted { name, pane_id } => {
                assert_eq!(name, "already-up");
                assert_eq!(pane_id, "pane:42");
            }
            other => panic!("expected AlreadyMounted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mount_failure_transitions_to_failed_and_emits_event() {
        // Use a registry with NO mount_plugin registration so the
        // dispatch returns op/unknown — driving the Failed path.
        let empty_registry = IntentRegistry::new();
        let ctx = test_ctx();
        let (tx, mut rx) = channel(8);
        let mgr = PluginLifecycleManager::new();

        let decl = make_plugin_decl("failing", Lifecycle::Always);

        let outcomes = mgr
            .mount_always_on(&[decl], &empty_registry, &ctx, &tx)
            .await;
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            MountOutcome::Failed { name, reason } => {
                assert_eq!(name, "failing");
                assert!(!reason.is_empty());
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        // State records the failure.
        assert!(matches!(
            mgr.state("failing").await,
            Some(MountState::Failed { .. })
        ));

        // Bus received an ark.plugin.failed event.
        let event = rx.try_recv().expect("failure event");
        match event {
            CoreEvent::Ext(ext) => {
                assert_eq!(ext.kind, PLUGIN_FAILED_EVENT);
                assert_eq!(ext.payload["plugin"], "failing");
            }
            other => panic!("expected CoreEvent::Ext, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_returns_deterministic_order() {
        let mgr = PluginLifecycleManager::new();
        mgr.seed_dormant("zeta").await;
        mgr.seed_dormant("alpha").await;
        mgr.seed_dormant("mu").await;
        let snap = mgr.snapshot().await;
        let names: Vec<_> = snap.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn mount_state_slugs_are_stable() {
        assert_eq!(MountState::Dormant.as_str(), "dormant");
        assert_eq!(
            MountState::Mounted {
                pane_id: "x".to_string()
            }
            .as_str(),
            "mounted"
        );
        assert_eq!(
            MountState::Failed {
                reason: "x".to_string()
            }
            .as_str(),
            "failed"
        );
    }

    #[test]
    fn build_mount_plugin_node_emits_name_and_at() {
        let decl = make_plugin_decl("picker", Lifecycle::Always);
        let synthesised = build_mount_plugin_node(&decl);
        // Inspect entries directly — KDL 2.0's Display form adds
        // layout whitespace that varies by version; assertions against
        // the entry map are more stable.
        let name_entry = synthesised
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("name"))
            .expect("name entry present");
        assert_eq!(name_entry.value().as_string(), Some("picker"));
        let at_entry = synthesised
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("at"))
            .expect("at entry present");
        assert_eq!(at_entry.value().as_string(), Some("floating"));
    }

    #[test]
    fn node_from_source_is_utility_only() {
        // Smoke test: the helper is used as a fixture escape hatch.
        let node = node_from_source(r#"mount_plugin name="foo""#);
        assert_eq!(node.name().value(), "mount_plugin");
    }
}
