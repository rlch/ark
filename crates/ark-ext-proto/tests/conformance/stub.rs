//! Reference stub extension implementing every `ArkExtension` method
//! the conformance suite exercises. Lives outside the `ark-ext-proto`
//! crate's `src/` so third-party extension authors can use this file
//! as a starting point — the suite verifies that any extension
//! implementing the trait's surface satisfies the protocol contract.

use std::sync::atomic::{AtomicU32, Ordering};

use ark_ext_proto::{
    ArkExtension, CancelRequest, CancelResponse, ExtResult, HostFsReadRequest, HostFsReadResponse,
    InitializeRequest, InitializeResponse, IntentDispatchRequest, IntentDispatchResponse, LogLevel,
    LogWriteRequest, LogWriteResponse, PingRequest, PingResponse, ProgressRequest, ProgressResponse,
    ShutdownRequest, ShutdownResponse, TaskCancelRequest, TaskCancelResponse, TaskCreateRequest,
    TaskCreateResponse, TaskGetRequest, TaskGetResponse, TaskId, UiKeybindRegisterRequest,
    UiKeybindRegisterResponse, UiStatusPushRequest, UiStatusPushResponse,
};
use async_trait::async_trait;

/// Reference extension stub. Echoes inputs back through structured
/// responses so the harness can assert call-arrival.
pub struct ConformanceStub {
    /// Reported `protocol_version` on `initialize`. Tests can swap
    /// this to drive the version-mismatch path.
    pub protocol_version: String,
    /// Reported capability bag on `initialize`. Carried as wire-form
    /// `OpaqueJson` (object-of-objects per R10).
    pub capabilities: String,
    /// Counts every `ping` call so the suite can assert dispatch.
    pub ping_count: AtomicU32,
    /// Counts cancel notifications.
    pub cancel_count: AtomicU32,
}

impl ConformanceStub {
    /// Default stub: protocol 1.0, full capability bag, fresh counters.
    pub fn new() -> Self {
        Self {
            protocol_version: "1.0".into(),
            capabilities: r#"{"ui":{"keybind":true,"status":true},"intents":{"dispatch":true},"events":{"emit":true,"subscribe":true},"host":{"fs":{"read":true}}}"#.into(),
            ping_count: AtomicU32::new(0),
            cancel_count: AtomicU32::new(0),
        }
    }

    /// Variant returning a different MAJOR version — drives the
    /// `UnsupportedVersion` test case.
    pub fn with_version(version: impl Into<String>) -> Self {
        Self {
            protocol_version: version.into(),
            ..Self::new()
        }
    }
}

#[async_trait]
impl ArkExtension for ConformanceStub {
    async fn initialize(&self, _req: InitializeRequest) -> ExtResult<InitializeResponse> {
        Ok(InitializeResponse {
            protocol_version: self.protocol_version.clone(),
            extension_capabilities: self.capabilities.clone(),
            extension_info: r#"{"name":"conformance-stub","version":"0.0.1"}"#.into(),
            session_token: String::new(),
        })
    }

    async fn shutdown(&self, _req: ShutdownRequest) -> ExtResult<ShutdownResponse> {
        Ok(ShutdownResponse::default())
    }

    async fn ping(&self, _req: PingRequest) -> ExtResult<PingResponse> {
        self.ping_count.fetch_add(1, Ordering::Relaxed);
        Ok(PingResponse::default())
    }

    async fn cancel(&self, _req: CancelRequest) -> ExtResult<CancelResponse> {
        self.cancel_count.fetch_add(1, Ordering::Relaxed);
        Ok(CancelResponse::default())
    }

    async fn progress(&self, _req: ProgressRequest) -> ExtResult<ProgressResponse> {
        Ok(ProgressResponse::default())
    }

    async fn task_create(
        &self,
        req: TaskCreateRequest,
    ) -> ExtResult<TaskCreateResponse> {
        Ok(TaskCreateResponse {
            task: TaskId {
                value: format!("task:{}", req.label),
            },
        })
    }

    async fn task_get(&self, req: TaskGetRequest) -> ExtResult<TaskGetResponse> {
        Ok(TaskGetResponse {
            status: "succeeded".into(),
            result: Some(format!("\"echoed:{}\"", req.task.value)),
        })
    }

    async fn task_cancel(
        &self,
        _req: TaskCancelRequest,
    ) -> ExtResult<TaskCancelResponse> {
        Ok(TaskCancelResponse::default())
    }

    async fn intent_dispatch(
        &self,
        req: IntentDispatchRequest,
    ) -> ExtResult<IntentDispatchResponse> {
        Ok(IntentDispatchResponse {
            value: format!("\"dispatched:{}\"", req.name),
        })
    }

    async fn ui_keybind_register(
        &self,
        _req: UiKeybindRegisterRequest,
    ) -> ExtResult<UiKeybindRegisterResponse> {
        Ok(UiKeybindRegisterResponse::default())
    }

    async fn ui_status_push(
        &self,
        _req: UiStatusPushRequest,
    ) -> ExtResult<UiStatusPushResponse> {
        Ok(UiStatusPushResponse::default())
    }

    async fn log_write(&self, _req: LogWriteRequest) -> ExtResult<LogWriteResponse> {
        Ok(LogWriteResponse::default())
    }

    async fn host_fs_read(
        &self,
        req: HostFsReadRequest,
    ) -> ExtResult<HostFsReadResponse> {
        Ok(HostFsReadResponse {
            contents: format!("read:{}", req.path),
        })
    }
}

/// Variant that always blocks `task_create` indefinitely so the suite
/// can exercise the request-timeout path.
pub struct BlackholeStub;

#[async_trait]
impl ArkExtension for BlackholeStub {
    async fn initialize(&self, _req: InitializeRequest) -> ExtResult<InitializeResponse> {
        Ok(InitializeResponse {
            protocol_version: "1.0".into(),
            extension_capabilities: "null".into(),
            extension_info: "null".into(),
            session_token: String::new(),
        })
    }

    async fn task_create(
        &self,
        _req: TaskCreateRequest,
    ) -> ExtResult<TaskCreateResponse> {
        // Park forever — only the timeout path can free this.
        std::future::pending().await
    }
}

/// Convenience: avoid an unused-import warning when the stub is
/// imported as a whole module.
#[allow(dead_code)]
pub fn touch_loglevel() -> LogLevel {
    LogLevel::Info
}
