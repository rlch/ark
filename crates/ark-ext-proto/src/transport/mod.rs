//! Transport layer for [`crate::ArkExtension`] RPC.
//!
//! The extension-protocol has three wire transports, all speaking the
//! same method surface (`cavekit-scene.md` R16 "protocol-first, trait-
//! derived"):
//!
//! 1. **NDJSON / JSON-RPC 2.0** ([`ndjson`]) — newline-delimited JSON
//!    over a child process's stdin/stdout. This is the subprocess-
//!    extension transport.
//! 2. **WIT-bindgen / wasm-component** (future T-9.5.5+) — typed RPC
//!    over the wasm-component-model.
//! 3. **In-process trait-object** ([`in_proc`]) — direct dispatch on an
//!    `Arc<dyn ArkExtension>` with zero serialization cost.
//!
//! All three share the [`ExtensionClient`] trait — ark's higher layers
//! code to the trait and swap the concrete client by extension manifest
//! `kind` without touching call sites.

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    CancelRequest, CancelResponse, EventEmitRequest, EventEmitResponse, EventNotifyRequest,
    EventNotifyResponse, EventSubscribeRequest, EventSubscribeResponse, EventUnsubscribeRequest,
    EventUnsubscribeResponse, ExtResult, HostFsReadRequest, HostFsReadResponse, HostFsWriteRequest,
    HostFsWriteResponse, HostNetFetchRequest, HostNetFetchResponse, HostProcSpawnRequest,
    HostProcSpawnResponse, InitializeRequest, InitializeResponse, InitializedRequest,
    InitializedResponse, IntentDispatchRequest, IntentDispatchResponse, IntentRegisterRequest,
    IntentRegisterResponse, IntentUnregisterRequest, IntentUnregisterResponse, LogSetLevelRequest,
    LogSetLevelResponse, LogWriteRequest, LogWriteResponse, PingRequest, PingResponse,
    ProgressRequest, ProgressResponse, SceneGetRootRequest, SceneGetRootResponse, ShutdownRequest,
    ShutdownResponse, TaskCancelRequest, TaskCancelResponse, TaskCreateRequest, TaskCreateResponse,
    TaskGetRequest, TaskGetResponse, UiKeybindRegisterRequest, UiKeybindRegisterResponse,
    UiKeybindUnregisterRequest, UiKeybindUnregisterResponse, UiPaneCloseRequest,
    UiPaneCloseResponse, UiPaneRequestRequest, UiPaneRequestResponse, UiStatusPushRequest,
    UiStatusPushResponse, WorkspaceApplyEditRequest, WorkspaceApplyEditResponse,
    WorkspaceConfigurationRequest, WorkspaceConfigurationResponse, WorkspaceShowDocumentRequest,
    WorkspaceShowDocumentResponse, WorkspaceShowMessageRequest, WorkspaceShowMessageRequestRequest,
    WorkspaceShowMessageRequestResponse, WorkspaceShowMessageResponse,
};

pub mod in_proc;
pub mod ndjson;

pub use in_proc::InProcClient;
pub use ndjson::{NdjsonClient, NdjsonServer, Notification, Request, Response, ResponseError};

/// Per-call dispatch options.
///
/// All fields default to their `const`-friendly values (the struct
/// itself is `Default`). Callers tweak individual knobs for long-running
/// operations — e.g. a 30s `task/create` probe — without re-creating a
/// builder.
#[derive(Debug, Clone)]
pub struct RequestOptions {
    /// Hard deadline for the round-trip. Timer starts when the request
    /// hits the wire and stops when the matching `Response` is routed
    /// back. On expiry the client emits a `$/cancel` notification with
    /// the request id and returns
    /// [`crate::ExtensionError::Internal`] carrying a `timeout` message.
    ///
    /// Default: 5 seconds.
    pub timeout: Duration,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            // R16 says the HOST decides transport policy — 5s is the
            // generic UI-responsiveness budget (long-running ops use
            // `task/create` + `task/get` instead).
            timeout: Duration::from_secs(5),
        }
    }
}

/// Client-side view of the extension RPC surface.
///
/// One method per JSON-RPC method on the `ArkExtension` trait. Both the
/// NDJSON subprocess client ([`NdjsonClient`]) and the in-process
/// trait-object client ([`InProcClient`]) implement this — ark's
/// supervisor holds an `Arc<dyn ExtensionClient>` and dispatches against
/// it without knowing the transport.
///
/// Methods take a typed request struct and return `ExtResult<Response>`.
/// The default implementation for each method passes options via the
/// companion `*_with` method; implementors override the `*_with` form
/// and the `*` forms fall through with `RequestOptions::default()`.
///
/// # Why one method per RPC (vs. a generic `call<M>`)?
///
/// * Enum dispatch would defer the type-check to runtime — a client
///   calling `cancel` when the extension only has `task/cancel` would
///   fail at runtime instead of compile time.
/// * Facet-based bindings generate per-method code anyway, so the
///   explicit surface is no more boilerplate than the alternative.
/// * The caller site reads as ordinary async method calls
///   (`client.initialize(req).await?`) with full rustdoc on each one.
#[async_trait]
pub trait ExtensionClient: Send + Sync {
    // -- Lifecycle -----------------------------------------------------------

    /// `initialize` — opens the session. First call on any client.
    async fn initialize(
        &self,
        req: InitializeRequest,
        opts: RequestOptions,
    ) -> ExtResult<InitializeResponse>;

    /// `initialized` notification — ark → ext confirms the client is
    /// ready. No response.
    async fn initialized(
        &self,
        req: InitializedRequest,
        opts: RequestOptions,
    ) -> ExtResult<InitializedResponse>;

    /// `shutdown` — graceful teardown request. Transports follow up
    /// with stdin-close → SIGTERM → SIGKILL per R16.
    async fn shutdown(
        &self,
        req: ShutdownRequest,
        opts: RequestOptions,
    ) -> ExtResult<ShutdownResponse>;

    /// `ping` liveness probe.
    async fn ping(&self, req: PingRequest, opts: RequestOptions) -> ExtResult<PingResponse>;

    // -- Async + cancel ------------------------------------------------------

    /// `$/cancel` notification — request cancellation by id.
    async fn cancel(
        &self,
        req: CancelRequest,
        opts: RequestOptions,
    ) -> ExtResult<CancelResponse>;

    /// `$/progress` notification.
    async fn progress(
        &self,
        req: ProgressRequest,
        opts: RequestOptions,
    ) -> ExtResult<ProgressResponse>;

    /// `task/create` — start a long-running op.
    async fn task_create(
        &self,
        req: TaskCreateRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskCreateResponse>;

    /// `task/get` — poll the state of a task.
    async fn task_get(
        &self,
        req: TaskGetRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskGetResponse>;

    /// `task/cancel` — cooperative cancel by handle.
    async fn task_cancel(
        &self,
        req: TaskCancelRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskCancelResponse>;

    // -- Event bus -----------------------------------------------------------

    /// `event/subscribe`.
    async fn event_subscribe(
        &self,
        req: EventSubscribeRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventSubscribeResponse>;

    /// `event/unsubscribe`.
    async fn event_unsubscribe(
        &self,
        req: EventUnsubscribeRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventUnsubscribeResponse>;

    /// `event/emit`.
    async fn event_emit(
        &self,
        req: EventEmitRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventEmitResponse>;

    /// `event/notify` — host-to-extension delivery.
    async fn event_notify(
        &self,
        req: EventNotifyRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventNotifyResponse>;

    // -- Intents -------------------------------------------------------------

    /// `intent/register`.
    async fn intent_register(
        &self,
        req: IntentRegisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentRegisterResponse>;

    /// `intent/unregister`.
    async fn intent_unregister(
        &self,
        req: IntentUnregisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentUnregisterResponse>;

    /// `intent/dispatch`.
    async fn intent_dispatch(
        &self,
        req: IntentDispatchRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentDispatchResponse>;

    // -- UI: keybind / status ------------------------------------------------

    /// `ui/keybind/register`.
    async fn ui_keybind_register(
        &self,
        req: UiKeybindRegisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiKeybindRegisterResponse>;

    /// `ui/keybind/unregister`.
    async fn ui_keybind_unregister(
        &self,
        req: UiKeybindUnregisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiKeybindUnregisterResponse>;

    /// `ui/status/push` notification.
    async fn ui_status_push(
        &self,
        req: UiStatusPushRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiStatusPushResponse>;

    // -- UI: panes -----------------------------------------------------------

    /// `ui/pane/request`.
    async fn ui_pane_request(
        &self,
        req: UiPaneRequestRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiPaneRequestResponse>;

    /// `ui/pane/close`.
    async fn ui_pane_close(
        &self,
        req: UiPaneCloseRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiPaneCloseResponse>;

    // -- Workspace -----------------------------------------------------------

    /// `workspace/applyEdit`.
    async fn workspace_apply_edit(
        &self,
        req: WorkspaceApplyEditRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceApplyEditResponse>;

    /// `workspace/configuration`.
    async fn workspace_configuration(
        &self,
        req: WorkspaceConfigurationRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceConfigurationResponse>;

    /// `workspace/showDocument`.
    async fn workspace_show_document(
        &self,
        req: WorkspaceShowDocumentRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowDocumentResponse>;

    /// `workspace/showMessage` notification.
    async fn workspace_show_message(
        &self,
        req: WorkspaceShowMessageRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageResponse>;

    /// `workspace/showMessageRequest`.
    async fn workspace_show_message_request(
        &self,
        req: WorkspaceShowMessageRequestRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageRequestResponse>;

    // -- Scene ---------------------------------------------------------------

    /// `scene/getRoot`.
    async fn scene_get_root(
        &self,
        req: SceneGetRootRequest,
        opts: RequestOptions,
    ) -> ExtResult<SceneGetRootResponse>;

    // -- Host syscalls (wasm-only) -------------------------------------------

    /// `host/fs/read`.
    async fn host_fs_read(
        &self,
        req: HostFsReadRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostFsReadResponse>;

    /// `host/fs/write`.
    async fn host_fs_write(
        &self,
        req: HostFsWriteRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostFsWriteResponse>;

    /// `host/proc/spawn`.
    async fn host_proc_spawn(
        &self,
        req: HostProcSpawnRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostProcSpawnResponse>;

    /// `host/net/fetch`.
    async fn host_net_fetch(
        &self,
        req: HostNetFetchRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostNetFetchResponse>;

    // -- Logging -------------------------------------------------------------

    /// `log/write` notification.
    async fn log_write(
        &self,
        req: LogWriteRequest,
        opts: RequestOptions,
    ) -> ExtResult<LogWriteResponse>;

    /// `log/setLevel`.
    async fn log_set_level(
        &self,
        req: LogSetLevelRequest,
        opts: RequestOptions,
    ) -> ExtResult<LogSetLevelResponse>;
}
