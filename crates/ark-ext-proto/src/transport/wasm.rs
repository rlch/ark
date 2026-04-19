//! Wasm-component transport scaffold (T-084 / scene-v3 S-H).
//!
//! This module is the **third delivery mode** for ark extensions listed
//! in `cavekit-scene.md` R6 — alongside the compiled-in [`super::in_proc`]
//! trait-object dispatcher and the subprocess [`super::ndjson`]
//! JSON-RPC 2.0 transport. It pins the *type-level* surface of the
//! `WasmExtensionClient` so ark's supervisor can dispatch against
//! `Arc<dyn ExtensionClient>` without knowing which of the three
//! transports backs it.
//!
//! # Status — scaffold
//!
//! The acceptance criterion for T-084 is that **three delivery modes
//! exist as types** so the rest of the ark tree (supervisor, CLI ext
//! helpers, scene's `wasm_meta` reader) can refer to the wasm transport
//! by name and we don't rewrite the `ExtensionClient` trait when the
//! full `wasmtime::component::Component` integration lands in v0.2.
//!
//! This file therefore ships:
//!
//! * A concrete [`WasmExtensionClient`] struct with the configuration
//!   knobs the future runtime will need ([`WasmClientConfig`]), held
//!   behind a feature-gate-free surface so scaffold callers don't pay
//!   the `wasmtime` compile cost on a default build.
//! * A full [`super::ExtensionClient`] trait impl whose every method
//!   returns [`crate::ExtensionError::method_not_found`] — the opt-out
//!   path that the NDJSON server-side shim + in-process trait-object
//!   clients already use for unimplemented methods (F-015 / R16
//!   "missing methods return JSON-RPC `-32601`"). This means callers
//!   can wire a `WasmExtensionClient` into the supervisor TODAY and
//!   the behaviour is "transport exists, no methods implemented yet"
//!   — identical to an extension that implements zero trait methods.
//! * A v0.2 wiring plan documented on [`WasmExtensionClient::load`]
//!   that enumerates the wasmtime component-model steps (feature-gated
//!   behind a `wasm-transport` cargo feature when the dep is added).
//!
//! # Why scaffold, not full integration (2026-04-18 close-out note)
//!
//! Full `wasmtime::component::Component` integration pulls in the
//! `wasmtime` crate (~10 MB of deps: `cranelift-*`, `regalloc2`,
//! `wasm-encoder`, `wasmparser`, …) as an unconditional dependency of
//! `ark-ext-proto`. Per the packet constraint we keep `ark-ext-proto`
//! lean — it's the pure-contract crate every other ark crate transits
//! through. The concrete dispatcher will arrive in v0.2 behind a
//! cargo feature (`wasm-transport`) so default `cargo build` stays
//! under the current compile-time budget.
//!
//! # Three-mode type-proof
//!
//! ```no_run
//! use std::sync::Arc;
//! use ark_ext_proto::transport::{
//!     ExtensionClient, InProcClient, NdjsonClient,
//!     wasm::{WasmExtensionClient, WasmClientConfig},
//! };
//!
//! fn pick_transport(kind: &str) -> Arc<dyn ExtensionClient> {
//!     match kind {
//!         // Mode 1: compiled-in trait-object dispatch.
//!         "in-process" => Arc::new(InProcClient::from_ext(
//!             unimplemented_ext(),
//!         )),
//!         // Mode 2: subprocess NDJSON / JSON-RPC 2.0.
//!         "subprocess" => Arc::new(unimplemented_ndjson()),
//!         // Mode 3: wasm component (scaffold — methods return
//!         // `method_not_found` until v0.2 wasmtime integration).
//!         "wasm" => Arc::new(WasmExtensionClient::scaffold(
//!             WasmClientConfig::default(),
//!         )),
//!         _ => panic!("unknown extension kind"),
//!     }
//! }
//! # fn unimplemented_ext() -> impl ark_ext_proto::ArkExtension { struct E; #[async_trait::async_trait] impl ark_ext_proto::ArkExtension for E {} E }
//! # fn unimplemented_ndjson() -> NdjsonClient { unimplemented!() }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use super::{ExtensionClient, RequestOptions};
use crate::*;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Run-time configuration for a [`WasmExtensionClient`].
///
/// Fields correspond to knobs the v0.2 wasmtime integration will need
/// when it lands the concrete `wasmtime::Engine` + `wasmtime::Store`
/// wiring. Today these are descriptive only — the scaffold path does
/// not read them.
#[derive(Debug, Clone)]
pub struct WasmClientConfig {
    /// Path to the `.wasm` component file on disk. The scene metadata
    /// reader (`crates/scene/src/wasm_meta.rs`) returns this path in
    /// its `ExtensionMetadata` output; `WasmExtensionClient::load`
    /// will consume it verbatim in v0.2.
    pub component_path: PathBuf,
    /// Maximum memory (bytes) the component's linear memory may grow
    /// to. The v0.2 wasmtime wiring will translate this into a
    /// `wasmtime::ResourceLimiter`. Default: 64 MiB.
    pub memory_limit_bytes: u64,
    /// Fuel budget per host-to-guest call. Zero = fuel disabled. The
    /// v0.2 wiring will install a `Store::set_fuel` ceiling around
    /// every exported-function invocation. Default: `0` (disabled —
    /// the v0.2 dispatcher decides).
    pub fuel_per_call: u64,
    /// Opt-in WASI preview2 context. When `true` the v0.2 wiring
    /// installs a `wasmtime_wasi::WasiCtx` with the HOST-decided
    /// capability set. Extensions still go through the reverse-request
    /// gate ([`super::ReverseRequestGate`]) for any `host/*` or
    /// `workspace/*` call regardless of WASI context.
    pub wasi_preview2: bool,
}

impl Default for WasmClientConfig {
    fn default() -> Self {
        Self {
            component_path: PathBuf::new(),
            memory_limit_bytes: 64 * 1024 * 1024,
            fuel_per_call: 0,
            wasi_preview2: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Wasm-component [`ExtensionClient`] implementation (T-084 scaffold).
///
/// One instance per loaded `.wasm` component. The scaffold holds only
/// the configuration blob; the v0.2 wasmtime integration will grow
/// fields for the engine, compiled component, pre-instantiated store,
/// and the per-export function handles (`handshake`, `ping`, …).
///
/// # Cloneability
///
/// The scaffold is `Clone` so the supervisor can hand out
/// `Arc<dyn ExtensionClient>` references without contention. The v0.2
/// wiring may tighten this to `Clone` only via an internal `Arc<Mutex>`
/// around the wasmtime `Store` — callers should not assume
/// independent-state semantics on clone.
#[derive(Debug, Clone)]
pub struct WasmExtensionClient {
    /// Shared configuration. `Arc` so multiple clones of the client
    /// share the same backing config without extra allocations.
    config: Arc<WasmClientConfig>,
}

impl WasmExtensionClient {
    /// Construct a scaffold client — every method returns
    /// [`ExtensionError::method_not_found`] until the v0.2 wasmtime
    /// integration lands.
    ///
    /// This entry point is intentionally named `scaffold` (not `new`)
    /// so downstream users of `ark-ext-proto` know they're wiring a
    /// stub transport. When the v0.2 wiring lands, an additional
    /// [`WasmExtensionClient::load`] constructor will return
    /// `Result<Self, ExtensionError>` after compiling the component.
    pub fn scaffold(config: WasmClientConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    /// Borrow the configuration the client was constructed with.
    pub fn config(&self) -> &WasmClientConfig {
        &self.config
    }

    /// Compile + instantiate a wasm component (v0.2 stub).
    ///
    /// # v0.2 wiring plan
    ///
    /// When the `wasm-transport` cargo feature lands this constructor
    /// will perform:
    ///
    /// 1. `let engine = wasmtime::Engine::new(&config)?;` with
    ///    component-model + async support enabled.
    /// 2. `let component = wasmtime::component::Component::from_file(
    ///    &engine, &cfg.component_path)?;`
    /// 3. Build a `wasmtime::Store<HostState>` whose `HostState`
    ///    carries the [`ReverseRequestGate`] + `WasiCtx` (when
    ///    [`WasmClientConfig::wasi_preview2`] is set).
    /// 4. `wasmtime::component::bindgen!` will generate typed
    ///    `handshake` / `ping` / … host and guest bindings from the
    ///    `share/extension-protocol.wit` (emitted by the existing
    ///    `gen-extension-spec` binary) — today the binary emits KDL
    ///    only; the WIT emitter is a sibling v0.2 task.
    /// 5. Call `bindings::ArkExtension::handshake(&mut store, …)` for
    ///    the real round-trip.
    ///
    /// Until that lands, this constructor returns
    /// [`ExtensionError::method_not_found`] wrapped around a
    /// diagnostic message pointing at this scaffold.
    pub fn load(_path: impl AsRef<Path>) -> ExtResult<Self> {
        Err(ExtensionError::method_not_found(
            "wasm-transport/load (v0.2 — wasmtime integration deferred)",
        ))
    }
}

// ---------------------------------------------------------------------------
// ExtensionClient impl — opt-out everywhere via method_not_found
// ---------------------------------------------------------------------------

/// Helper that constructs the wire-method-named error for a given
/// trait method. Extracted so every trait method forwards through one
/// consistent call site — when the v0.2 wasmtime dispatch lands we
/// replace this helper's callers one-at-a-time with the real wasm
/// exported-function invocation.
fn not_impl(method: &'static str) -> ExtensionError {
    ExtensionError::method_not_found(method)
}

#[async_trait]
impl ExtensionClient for WasmExtensionClient {
    // -- Lifecycle -----------------------------------------------------------

    async fn initialize(
        &self,
        _req: InitializeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<InitializeResponse> {
        Err(not_impl("initialize"))
    }

    async fn initialized(
        &self,
        _req: InitializedRequest,
        _opts: RequestOptions,
    ) -> ExtResult<InitializedResponse> {
        Err(not_impl("initialized"))
    }

    async fn shutdown(
        &self,
        _req: ShutdownRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ShutdownResponse> {
        Err(not_impl("shutdown"))
    }

    async fn ping(&self, _req: PingRequest, _opts: RequestOptions) -> ExtResult<PingResponse> {
        Err(not_impl("ping"))
    }

    // -- Async + cancel ------------------------------------------------------

    async fn cancel(
        &self,
        _req: CancelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<CancelResponse> {
        Err(not_impl("$/cancel"))
    }

    async fn progress(
        &self,
        _req: ProgressRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ProgressResponse> {
        Err(not_impl("$/progress"))
    }

    async fn task_create(
        &self,
        _req: TaskCreateRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskCreateResponse> {
        Err(not_impl("task/create"))
    }

    async fn task_get(
        &self,
        _req: TaskGetRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskGetResponse> {
        Err(not_impl("task/get"))
    }

    async fn task_cancel(
        &self,
        _req: TaskCancelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskCancelResponse> {
        Err(not_impl("task/cancel"))
    }

    // -- Event bus -----------------------------------------------------------

    async fn event_subscribe(
        &self,
        _req: EventSubscribeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventSubscribeResponse> {
        Err(not_impl("event/subscribe"))
    }

    async fn event_unsubscribe(
        &self,
        _req: EventUnsubscribeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventUnsubscribeResponse> {
        Err(not_impl("event/unsubscribe"))
    }

    async fn event_emit(
        &self,
        _req: EventEmitRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventEmitResponse> {
        Err(not_impl("event/emit"))
    }

    async fn event_notify(
        &self,
        _req: EventNotifyRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventNotifyResponse> {
        Err(not_impl("event/notify"))
    }

    // -- Intents -------------------------------------------------------------

    async fn intent_unregister(
        &self,
        _req: IntentUnregisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<IntentUnregisterResponse> {
        Err(not_impl("intent/unregister"))
    }

    async fn intent_dispatch(
        &self,
        _req: IntentDispatchRequest,
        _opts: RequestOptions,
    ) -> ExtResult<IntentDispatchResponse> {
        Err(not_impl("intent/dispatch"))
    }

    // -- UI: keybind / status ------------------------------------------------

    async fn ui_keybind_register(
        &self,
        _req: UiKeybindRegisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiKeybindRegisterResponse> {
        Err(not_impl("ui/keybind/register"))
    }

    async fn ui_keybind_unregister(
        &self,
        _req: UiKeybindUnregisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiKeybindUnregisterResponse> {
        Err(not_impl("ui/keybind/unregister"))
    }

    async fn ui_status_push(
        &self,
        _req: UiStatusPushRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiStatusPushResponse> {
        Err(not_impl("ui/status/push"))
    }

    // -- UI: panes -----------------------------------------------------------

    async fn ui_pane_request(
        &self,
        _req: UiPaneRequestRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiPaneRequestResponse> {
        Err(not_impl("ui/pane/request"))
    }

    async fn ui_pane_close(
        &self,
        _req: UiPaneCloseRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiPaneCloseResponse> {
        Err(not_impl("ui/pane/close"))
    }

    // -- Pane / Stack handle ops (Phase 2 R6) --------------------------------

    async fn pane_emit(
        &self,
        _req: PaneEmitRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneEmitResponse> {
        Err(not_impl("pane/emit"))
    }

    async fn pane_replace_view(
        &self,
        _req: PaneReplaceViewRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneReplaceViewResponse> {
        Err(not_impl("pane/replace_view"))
    }

    async fn pane_close(
        &self,
        _req: PaneCloseRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneCloseResponse> {
        Err(not_impl("pane/close"))
    }

    async fn stack_spawn_pane(
        &self,
        _req: StackSpawnPaneRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackSpawnPaneResponse> {
        Err(not_impl("stack/spawn_pane"))
    }

    async fn stack_close_child(
        &self,
        _req: StackCloseChildRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackCloseChildResponse> {
        Err(not_impl("stack/close_child"))
    }

    async fn stack_clear(
        &self,
        _req: StackClearRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackClearResponse> {
        Err(not_impl("stack/clear"))
    }

    // -- Session lifecycle hooks (Phase 2 ext-surface R1) --------------------

    async fn on_session_start(
        &self,
        _req: OnSessionStartRequest,
        _opts: RequestOptions,
    ) -> ExtResult<OnSessionStartResponse> {
        Err(not_impl("on_session_start"))
    }

    async fn on_session_end(
        &self,
        _req: OnSessionEndRequest,
        _opts: RequestOptions,
    ) -> ExtResult<OnSessionEndResponse> {
        Err(not_impl("on_session_end"))
    }

    // -- Feature-group hooks (Phase 2 ext-surface R2) ------------------------

    async fn scene_compile_hook(
        &self,
        _req: SceneCompileHookRequest,
        _opts: RequestOptions,
    ) -> ExtResult<SceneCompileHookResponse> {
        Err(not_impl("scene_compile_hook"))
    }

    async fn control_verbs(
        &self,
        _req: ControlVerbsRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ControlVerbsResponse> {
        Err(not_impl("control_verbs"))
    }

    async fn doctor_checks(
        &self,
        _req: DoctorChecksRequest,
        _opts: RequestOptions,
    ) -> ExtResult<DoctorChecksResponse> {
        Err(not_impl("doctor_checks"))
    }

    async fn list_columns(
        &self,
        _req: ListColumnsRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ListColumnsResponse> {
        Err(not_impl("list_columns"))
    }

    // -- Workspace -----------------------------------------------------------

    async fn workspace_apply_edit(
        &self,
        _req: WorkspaceApplyEditRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceApplyEditResponse> {
        Err(not_impl("workspace/applyEdit"))
    }

    async fn workspace_configuration(
        &self,
        _req: WorkspaceConfigurationRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceConfigurationResponse> {
        Err(not_impl("workspace/configuration"))
    }

    async fn workspace_show_document(
        &self,
        _req: WorkspaceShowDocumentRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowDocumentResponse> {
        Err(not_impl("workspace/showDocument"))
    }

    async fn workspace_show_message(
        &self,
        _req: WorkspaceShowMessageRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageResponse> {
        Err(not_impl("workspace/showMessage"))
    }

    async fn workspace_show_message_request(
        &self,
        _req: WorkspaceShowMessageRequestRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageRequestResponse> {
        Err(not_impl("workspace/showMessageRequest"))
    }

    // -- Scene ---------------------------------------------------------------

    async fn scene_get_root(
        &self,
        _req: SceneGetRootRequest,
        _opts: RequestOptions,
    ) -> ExtResult<SceneGetRootResponse> {
        Err(not_impl("scene/getRoot"))
    }

    // -- Host syscalls -------------------------------------------------------

    async fn host_fs_read(
        &self,
        _req: HostFsReadRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostFsReadResponse> {
        Err(not_impl("host/fs/read"))
    }

    async fn host_fs_write(
        &self,
        _req: HostFsWriteRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostFsWriteResponse> {
        Err(not_impl("host/fs/write"))
    }

    async fn host_proc_spawn(
        &self,
        _req: HostProcSpawnRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostProcSpawnResponse> {
        Err(not_impl("host/proc/spawn"))
    }

    async fn host_net_fetch(
        &self,
        _req: HostNetFetchRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostNetFetchResponse> {
        Err(not_impl("host/net/fetch"))
    }

    // -- Logging -------------------------------------------------------------

    async fn log_write(
        &self,
        _req: LogWriteRequest,
        _opts: RequestOptions,
    ) -> ExtResult<LogWriteResponse> {
        Err(not_impl("log/write"))
    }

    async fn log_set_level(
        &self,
        _req: LogSetLevelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<LogSetLevelResponse> {
        Err(not_impl("log/setLevel"))
    }
}

// ---------------------------------------------------------------------------
// Tests — scaffold type surface + method_not_found propagation
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The scaffold is constructible via `scaffold(config)` and the
    /// config round-trips through `config()` unchanged.
    #[test]
    fn scaffold_constructs_and_exposes_config() {
        let cfg = WasmClientConfig {
            component_path: PathBuf::from("/tmp/ark-test/fixture.wasm"),
            memory_limit_bytes: 1024,
            fuel_per_call: 500,
            wasi_preview2: false,
        };
        let client = WasmExtensionClient::scaffold(cfg.clone());
        assert_eq!(
            client.config().component_path,
            PathBuf::from("/tmp/ark-test/fixture.wasm"),
        );
        assert_eq!(client.config().memory_limit_bytes, 1024);
        assert_eq!(client.config().fuel_per_call, 500);
        assert!(!client.config().wasi_preview2);
    }

    /// The scaffold is `Clone` and both handles point at the same
    /// backing config (shared via `Arc`). This proves the supervisor
    /// can fan out `Arc<dyn ExtensionClient>` clones without per-clone
    /// allocation explosion.
    #[test]
    fn scaffold_is_clone_and_shares_config_via_arc() {
        let cfg = WasmClientConfig {
            component_path: PathBuf::from("/tmp/ark-test/fx.wasm"),
            ..WasmClientConfig::default()
        };
        let a = WasmExtensionClient::scaffold(cfg);
        let b = a.clone();
        // Same underlying allocation — `Arc::ptr_eq` proves it.
        assert!(Arc::ptr_eq(&a.config, &b.config));
    }

    /// The scaffold plugs into `Arc<dyn ExtensionClient>` — the whole
    /// point of the scene-v3 three-mode contract. If this line stops
    /// compiling, the trait surface and the scaffold drifted.
    #[test]
    fn scaffold_is_dyn_extension_client() {
        let _boxed: Arc<dyn ExtensionClient> =
            Arc::new(WasmExtensionClient::scaffold(WasmClientConfig::default()));
    }

    /// `load` is the v0.2-wasmtime entry point; until that wiring
    /// lands it must return `MethodNotFound` pointing at the
    /// scaffold, so callers get a self-describing error instead of a
    /// silent unimplemented panic.
    #[test]
    fn load_returns_method_not_found_until_v02() {
        let err = WasmExtensionClient::load("/dev/null").unwrap_err();
        match err {
            ExtensionError::MethodNotFound(m) => {
                assert!(m.contains("wasm-transport/load"), "{m}");
                assert!(m.contains("v0.2"), "{m}");
            }
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    /// Calling `initialize` on the scaffold returns
    /// `MethodNotFound("initialize")` — the same wire-name the default
    /// `ArkExtension::initialize` impl uses (see
    /// `initialize_default_returns_method_not_found` in `lib.rs`).
    /// This keeps the three transports in sync on the opt-out path
    /// per F-015 / R16.
    #[tokio::test]
    async fn initialize_returns_method_not_found_verbatim() {
        let client = WasmExtensionClient::scaffold(WasmClientConfig::default());
        let err = client
            .initialize(
                InitializeRequest {
                    protocol_version: "1.0".into(),
                    client_capabilities: "null".into(),
                    client_info: "ark-test".into(),
                },
                RequestOptions::default(),
            )
            .await
            .expect_err("scaffold must refuse until v0.2");
        match err {
            ExtensionError::MethodNotFound(m) => assert_eq!(m, "initialize"),
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    /// Every Phase-2 RPC surface (`ping`, `intent/dispatch`,
    /// `stack/spawn_pane`, `scene/getRoot`) also bottoms out at
    /// `MethodNotFound` with the correct wire name. This is the
    /// T-084 acceptance proof — the scaffold is a legal
    /// `ExtensionClient` that the supervisor can dispatch against
    /// without special-casing, and every failure carries a
    /// diagnostic wire-name so ark ops can grep which method the
    /// ext doesn't ship yet.
    #[tokio::test]
    async fn every_method_returns_named_method_not_found() {
        let client = WasmExtensionClient::scaffold(WasmClientConfig::default());

        // Spot-check one method per functional group. Exhaustive
        // coverage lives in the trait's own default-impl test (see
        // `phase_2_new_trait_methods_default_to_method_not_found`
        // in `lib.rs`); here we prove the wasm transport doesn't
        // short-circuit the opt-out path.
        let cases: &[(&str, ExtensionError)] = &[
            (
                "ping",
                client
                    .ping(PingRequest::default(), RequestOptions::default())
                    .await
                    .unwrap_err(),
            ),
            (
                "intent/dispatch",
                client
                    .intent_dispatch(
                        IntentDispatchRequest {
                            name: "ark.core.pane.move".into(),
                            args: "null".into(),
                        },
                        RequestOptions::default(),
                    )
                    .await
                    .unwrap_err(),
            ),
            (
                "stack/spawn_pane",
                client
                    .stack_spawn_pane(
                        StackSpawnPaneRequest {
                            stack: ark_view::HandleId::new("stack-test-1"),
                            attrs: "null".into(),
                        },
                        RequestOptions::default(),
                    )
                    .await
                    .unwrap_err(),
            ),
            (
                "scene/getRoot",
                client
                    .scene_get_root(SceneGetRootRequest {}, RequestOptions::default())
                    .await
                    .unwrap_err(),
            ),
        ];

        for (expected, err) in cases {
            match err {
                ExtensionError::MethodNotFound(m) => {
                    assert_eq!(m, expected, "wrong method name on wasm transport");
                }
                other => {
                    panic!("expected MethodNotFound({expected}), got {other:?}")
                }
            }
        }
    }

    /// The default handshake (on the trait) delegates to
    /// `initialize` — so on the scaffold it must surface the
    /// `initialize` `MethodNotFound`, not a bogus version error.
    /// This verifies the R16 opt-out path reaches all the way
    /// through the handshake wrapper.
    #[tokio::test]
    async fn handshake_default_wraps_initialize_method_not_found() {
        let client = WasmExtensionClient::scaffold(WasmClientConfig::default());
        let err = client
            .handshake_default(Capabilities::empty(), "ark-test".into())
            .await
            .expect_err("scaffold handshake must refuse");
        match err {
            ExtensionError::MethodNotFound(m) => assert_eq!(m, "initialize"),
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }
}
