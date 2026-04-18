//! In-process trait-object dispatcher for compiled-in extensions.
//!
//! Ark's extension manifest (`ExtensionMetadata::kind`) can mark an
//! extension as `in-process`, meaning it is statically linked into the
//! ark binary and exposes an `Arc<dyn ArkExtension>` at runtime. This
//! module wraps that trait-object in an [`InProcClient`] that
//! implements the same [`super::ExtensionClient`] surface as the
//! NDJSON subprocess client — so ark's supervisor can call
//! `ExtensionClient::initialize(...)` without knowing which transport
//! it's driving.
//!
//! # Zero serialization
//!
//! Unlike the NDJSON transport, the in-process path pays NO JSON-
//! serialization cost: every method forwards directly to the trait
//! object's own method. The `RequestOptions` timeout is deliberately
//! ignored here — the trait-object implementation runs inline on the
//! same tokio runtime, and a per-call timeout wrapper would add
//! spurious overhead without buying cancellation semantics (the trait
//! method has no cooperative cancel surface; cancellation is the
//! caller's responsibility via the usual `tokio::select!` / drop
//! mechanics). A future revision may wrap each call in
//! `tokio::time::timeout(opts.timeout, ...)` if we decide in-proc
//! extensions should be deadline-supervised.

use std::sync::Arc;

use async_trait::async_trait;

use super::{ExtensionClient, RequestOptions};
use crate::*;

/// Compiled-in [`ExtensionClient`] backed by a shared
/// `Arc<dyn ArkExtension>`.
///
/// Construct via [`InProcClient::new`] with any `Arc<dyn ArkExtension>`
/// (or an `Arc<T>` that derefs to one). The client holds the trait
/// object by shared reference so multiple ark subsystems can hand out
/// `Arc<dyn ExtensionClient>` clones rooted on the same extension
/// without contention.
#[derive(Clone)]
pub struct InProcClient {
    ext: Arc<dyn ArkExtension>,
}

impl InProcClient {
    /// Wrap an `Arc<dyn ArkExtension>` as a client.
    pub fn new(ext: Arc<dyn ArkExtension>) -> Self {
        Self { ext }
    }

    /// Convenience constructor that takes any `T: ArkExtension +
    /// 'static` and wraps it in an `Arc`.
    pub fn from_ext<T>(ext: T) -> Self
    where
        T: ArkExtension + 'static,
    {
        Self {
            ext: Arc::new(ext),
        }
    }

    /// Borrow the wrapped extension. Useful in tests that want to
    /// assert against the underlying impl's side-effects.
    pub fn inner(&self) -> &Arc<dyn ArkExtension> {
        &self.ext
    }
}

#[async_trait]
impl ExtensionClient for InProcClient {
    // -- Lifecycle -----------------------------------------------------------

    async fn initialize(
        &self,
        req: InitializeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<InitializeResponse> {
        self.ext.initialize(req).await
    }

    async fn initialized(
        &self,
        req: InitializedRequest,
        _opts: RequestOptions,
    ) -> ExtResult<InitializedResponse> {
        self.ext.initialized(req).await
    }

    async fn shutdown(
        &self,
        req: ShutdownRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ShutdownResponse> {
        self.ext.shutdown(req).await
    }

    async fn ping(&self, req: PingRequest, _opts: RequestOptions) -> ExtResult<PingResponse> {
        self.ext.ping(req).await
    }

    // -- Async + cancel ------------------------------------------------------

    async fn cancel(
        &self,
        req: CancelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<CancelResponse> {
        self.ext.cancel(req).await
    }

    async fn progress(
        &self,
        req: ProgressRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ProgressResponse> {
        self.ext.progress(req).await
    }

    async fn task_create(
        &self,
        req: TaskCreateRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskCreateResponse> {
        self.ext.task_create(req).await
    }

    async fn task_get(
        &self,
        req: TaskGetRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskGetResponse> {
        self.ext.task_get(req).await
    }

    async fn task_cancel(
        &self,
        req: TaskCancelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<TaskCancelResponse> {
        self.ext.task_cancel(req).await
    }

    // -- Event bus -----------------------------------------------------------

    async fn event_subscribe(
        &self,
        req: EventSubscribeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventSubscribeResponse> {
        self.ext.event_subscribe(req).await
    }

    async fn event_unsubscribe(
        &self,
        req: EventUnsubscribeRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventUnsubscribeResponse> {
        self.ext.event_unsubscribe(req).await
    }

    async fn event_emit(
        &self,
        req: EventEmitRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventEmitResponse> {
        self.ext.event_emit(req).await
    }

    async fn event_notify(
        &self,
        req: EventNotifyRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventNotifyResponse> {
        self.ext.event_notify(req).await
    }

    // -- Intents -------------------------------------------------------------

    async fn intent_unregister(
        &self,
        req: IntentUnregisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<IntentUnregisterResponse> {
        self.ext.intent_unregister(req).await
    }

    async fn intent_dispatch(
        &self,
        req: IntentDispatchRequest,
        _opts: RequestOptions,
    ) -> ExtResult<IntentDispatchResponse> {
        self.ext.intent_dispatch(req).await
    }

    // -- UI: keybind / status ------------------------------------------------

    async fn ui_keybind_register(
        &self,
        req: UiKeybindRegisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiKeybindRegisterResponse> {
        self.ext.ui_keybind_register(req).await
    }

    async fn ui_keybind_unregister(
        &self,
        req: UiKeybindUnregisterRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiKeybindUnregisterResponse> {
        self.ext.ui_keybind_unregister(req).await
    }

    async fn ui_status_push(
        &self,
        req: UiStatusPushRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiStatusPushResponse> {
        self.ext.ui_status_push(req).await
    }

    // -- UI: panes -----------------------------------------------------------

    async fn ui_pane_request(
        &self,
        req: UiPaneRequestRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiPaneRequestResponse> {
        self.ext.ui_pane_request(req).await
    }

    async fn ui_pane_close(
        &self,
        req: UiPaneCloseRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiPaneCloseResponse> {
        self.ext.ui_pane_close(req).await
    }

    // -- Pane / Stack handle ops (Phase 2 R6) --------------------------------

    async fn pane_emit(
        &self,
        req: PaneEmitRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneEmitResponse> {
        self.ext.pane_emit(req).await
    }

    async fn pane_replace_view(
        &self,
        req: PaneReplaceViewRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneReplaceViewResponse> {
        self.ext.pane_replace_view(req).await
    }

    async fn pane_close(
        &self,
        req: PaneCloseRequest,
        _opts: RequestOptions,
    ) -> ExtResult<PaneCloseResponse> {
        self.ext.pane_close(req).await
    }

    async fn stack_spawn_pane(
        &self,
        req: StackSpawnPaneRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackSpawnPaneResponse> {
        self.ext.stack_spawn_pane(req).await
    }

    async fn stack_close_child(
        &self,
        req: StackCloseChildRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackCloseChildResponse> {
        self.ext.stack_close_child(req).await
    }

    async fn stack_clear(
        &self,
        req: StackClearRequest,
        _opts: RequestOptions,
    ) -> ExtResult<StackClearResponse> {
        self.ext.stack_clear(req).await
    }

    // -- Feature-group hooks (Phase 2 ext-surface R2) ------------------------

    async fn scene_compile_hook(
        &self,
        req: SceneCompileHookRequest,
        _opts: RequestOptions,
    ) -> ExtResult<SceneCompileHookResponse> {
        self.ext.scene_compile_hook(req).await
    }

    async fn control_verbs(
        &self,
        req: ControlVerbsRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ControlVerbsResponse> {
        self.ext.control_verbs(req).await
    }

    async fn doctor_checks(
        &self,
        req: DoctorChecksRequest,
        _opts: RequestOptions,
    ) -> ExtResult<DoctorChecksResponse> {
        self.ext.doctor_checks(req).await
    }

    async fn list_columns(
        &self,
        req: ListColumnsRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ListColumnsResponse> {
        self.ext.list_columns(req).await
    }

    // -- Workspace -----------------------------------------------------------

    async fn workspace_apply_edit(
        &self,
        req: WorkspaceApplyEditRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceApplyEditResponse> {
        self.ext.workspace_apply_edit(req).await
    }

    async fn workspace_configuration(
        &self,
        req: WorkspaceConfigurationRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceConfigurationResponse> {
        self.ext.workspace_configuration(req).await
    }

    async fn workspace_show_document(
        &self,
        req: WorkspaceShowDocumentRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowDocumentResponse> {
        self.ext.workspace_show_document(req).await
    }

    async fn workspace_show_message(
        &self,
        req: WorkspaceShowMessageRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageResponse> {
        self.ext.workspace_show_message(req).await
    }

    async fn workspace_show_message_request(
        &self,
        req: WorkspaceShowMessageRequestRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageRequestResponse> {
        self.ext.workspace_show_message_request(req).await
    }

    // -- Scene ---------------------------------------------------------------

    async fn scene_get_root(
        &self,
        req: SceneGetRootRequest,
        _opts: RequestOptions,
    ) -> ExtResult<SceneGetRootResponse> {
        self.ext.scene_get_root(req).await
    }

    // -- Host syscalls (wasm-only) -------------------------------------------

    async fn host_fs_read(
        &self,
        req: HostFsReadRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostFsReadResponse> {
        self.ext.host_fs_read(req).await
    }

    async fn host_fs_write(
        &self,
        req: HostFsWriteRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostFsWriteResponse> {
        self.ext.host_fs_write(req).await
    }

    async fn host_proc_spawn(
        &self,
        req: HostProcSpawnRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostProcSpawnResponse> {
        self.ext.host_proc_spawn(req).await
    }

    async fn host_net_fetch(
        &self,
        req: HostNetFetchRequest,
        _opts: RequestOptions,
    ) -> ExtResult<HostNetFetchResponse> {
        self.ext.host_net_fetch(req).await
    }

    // -- Logging -------------------------------------------------------------

    async fn log_write(
        &self,
        req: LogWriteRequest,
        _opts: RequestOptions,
    ) -> ExtResult<LogWriteResponse> {
        self.ext.log_write(req).await
    }

    async fn log_set_level(
        &self,
        req: LogSetLevelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<LogSetLevelResponse> {
        self.ext.log_set_level(req).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Stub extension that counts method invocations so we can prove
    /// `InProcClient` dispatches directly to the trait impl with no
    /// intermediate serialization step.
    struct CountingExt {
        ping_count: AtomicU32,
        last_label: std::sync::Mutex<Option<String>>,
    }

    impl CountingExt {
        fn new() -> Self {
            Self {
                ping_count: AtomicU32::new(0),
                last_label: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl ArkExtension for CountingExt {
        async fn ping(&self, _req: PingRequest) -> ExtResult<PingResponse> {
            self.ping_count.fetch_add(1, Ordering::Relaxed);
            Ok(PingResponse::default())
        }

        async fn task_create(
            &self,
            req: TaskCreateRequest,
        ) -> ExtResult<TaskCreateResponse> {
            *self.last_label.lock().unwrap() = Some(req.label.clone());
            Ok(TaskCreateResponse {
                task: TaskId {
                    value: format!("task:{}", req.label),
                },
            })
        }

        async fn initialize(
            &self,
            _req: InitializeRequest,
        ) -> ExtResult<InitializeResponse> {
            Ok(InitializeResponse {
                protocol_version: "0.1".into(),
                extension_capabilities: "null".into(),
                extension_info: "null".into(),
                session_token: String::new(),
            })
        }
    }

    /// Smoke test: every method forwards straight through to the trait
    /// object. Round-trip `ping` bumps the counter on the wrapped ext;
    /// `task_create` routes the label through verbatim.
    #[tokio::test]
    async fn in_proc_forwards_to_trait_impl() {
        let ext = Arc::new(CountingExt::new());
        let client = InProcClient::new(ext.clone());

        // Ping three times, verify the counter.
        for _ in 0..3 {
            client
                .ping(PingRequest::default(), RequestOptions::default())
                .await
                .unwrap();
        }
        assert_eq!(ext.ping_count.load(Ordering::Relaxed), 3);

        // task_create propagates payload fields.
        let resp = client
            .task_create(
                TaskCreateRequest {
                    label: "hello".into(),
                    params: "null".into(),
                },
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(resp.task.value, "task:hello");
        assert_eq!(
            ext.last_label.lock().unwrap().as_deref(),
            Some("hello"),
            "task_create must reach the trait impl"
        );
    }

    /// `method_not_found` from a default impl propagates verbatim.
    #[tokio::test]
    async fn in_proc_surfaces_method_not_found() {
        struct EmptyExt;
        #[async_trait]
        impl ArkExtension for EmptyExt {}
        let client = InProcClient::from_ext(EmptyExt);

        let err = client
            .task_create(
                TaskCreateRequest {
                    label: "x".into(),
                    params: "null".into(),
                },
                RequestOptions::default(),
            )
            .await
            .expect_err("default impl should refuse");
        match err {
            ExtensionError::MethodNotFound(m) => assert_eq!(m, "task/create"),
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
    }

    /// A handle can be cloned freely and both clones see the same
    /// underlying counter, proving there's only one backing extension
    /// instance. This is the whole point of the in-process transport:
    /// shared state, zero serialization.
    #[tokio::test]
    async fn in_proc_client_is_cloneable_and_shares_state() {
        let ext = Arc::new(CountingExt::new());
        let a = InProcClient::new(ext.clone());
        let b = a.clone();

        a.ping(PingRequest::default(), RequestOptions::default())
            .await
            .unwrap();
        b.ping(PingRequest::default(), RequestOptions::default())
            .await
            .unwrap();
        assert_eq!(ext.ping_count.load(Ordering::Relaxed), 2);
    }

    /// The client can be stored behind `Arc<dyn ExtensionClient>` so
    /// ark's supervisor can swap transports without touching call
    /// sites.
    #[tokio::test]
    async fn in_proc_client_is_dyn_extension_client() {
        let client: Arc<dyn ExtensionClient> =
            Arc::new(InProcClient::from_ext(CountingExt::new()));
        let resp = client
            .initialize(
                InitializeRequest {
                    protocol_version: "0.1".into(),
                    client_capabilities: "null".into(),
                    client_info: "ark-test".into(),
                },
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(resp.protocol_version, "0.1");
    }
}
