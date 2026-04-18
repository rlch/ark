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

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    CURRENT_PROTOCOL_VERSION, CancelRequest, CancelResponse, Capabilities, EventEmitRequest,
    EventEmitResponse, EventNotifyRequest, EventNotifyResponse, EventSubscribeRequest,
    EventSubscribeResponse, EventUnsubscribeRequest, EventUnsubscribeResponse, ExtResult,
    ExtensionError, HostFsReadRequest, HostFsReadResponse, HostFsWriteRequest, HostFsWriteResponse,
    HostNetFetchRequest, HostNetFetchResponse, HostProcSpawnRequest, HostProcSpawnResponse,
    InitializeRequest, InitializeResponse, InitializedRequest, InitializedResponse,
    IntentDispatchRequest, IntentDispatchResponse, IntentUnregisterRequest,
    IntentUnregisterResponse, LogSetLevelRequest, LogSetLevelResponse,
    LogWriteRequest, LogWriteResponse, PingRequest, PingResponse, ProgressRequest,
    ProgressResponse, ProtocolVersion, SceneGetRootRequest, SceneGetRootResponse, SessionToken,
    ShutdownRequest, ShutdownResponse, TaskCancelRequest, TaskCancelResponse, TaskCreateRequest,
    TaskCreateResponse, TaskGetRequest, TaskGetResponse, TaskId, UiKeybindRegisterRequest,
    UiKeybindRegisterResponse, UiKeybindUnregisterRequest, UiKeybindUnregisterResponse,
    UiPaneCloseRequest, UiPaneCloseResponse, UiPaneRequestRequest, UiPaneRequestResponse,
    UiStatusPushRequest, UiStatusPushResponse, WorkspaceApplyEditRequest,
    WorkspaceApplyEditResponse, WorkspaceConfigurationRequest, WorkspaceConfigurationResponse,
    WorkspaceShowDocumentRequest, WorkspaceShowDocumentResponse, WorkspaceShowMessageRequest,
    WorkspaceShowMessageRequestRequest, WorkspaceShowMessageRequestResponse,
    WorkspaceShowMessageResponse,
};

pub mod in_proc;
pub mod ndjson;

pub use in_proc::InProcClient;
pub use ndjson::{NdjsonClient, NdjsonServer, Notification, Request, Response, ResponseError};

/// Channel-end the host hands back to subscribers on
/// [`ExtensionClient::subscribe_progress`]. Each `$/progress` notification
/// the read loop demuxes by `taskId` is forwarded onto every live
/// subscriber for that task. The receiver is `unbounded` to keep
/// progress emission lock-free in the read loop; consumers MUST drain
/// in a timely manner.
pub type ProgressReceiver = mpsc::UnboundedReceiver<TaskProgress>;

/// Single decoded `$/progress` payload. Mirrors the LSP-style
/// `{ kind, message, percentage }` envelope but stays struct-typed so
/// downstream consumers can route by `percent` without re-parsing JSON.
///
/// Fields are populated best-effort from the raw `value` payload of
/// [`crate::ProgressRequest`]; missing fields fall back to the
/// [`Default`] impl values (`0` percent, empty `message`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskProgress {
    /// Task handle the progress entry refers to. Matches
    /// [`crate::TaskId::value`] / [`crate::ProgressRequest::token`].
    pub task: String,
    /// Current progress percentage (0..=100). Out-of-range values are
    /// clamped on emission.
    pub percent: u8,
    /// Human-readable message — surfaced verbatim in `ark` status.
    pub message: String,
    /// Original raw payload as JSON text — preserved so consumers that
    /// need the full LSP-style envelope (`{ kind: "begin"|"report"|
    /// "end" }`) can re-parse without losing data.
    pub raw: String,
}

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

    // -- Capability + progress hooks -----------------------------------------

    /// Subscribe to `$/progress` notifications for `task`.
    ///
    /// The default implementation returns an [`Receiver`] that is
    /// permanently empty — transports that genuinely demux `$/progress`
    /// off the wire (notably [`NdjsonClient`]) override this with a
    /// per-task channel rooted in the read loop.
    ///
    /// Multiple subscribers per task are allowed; each gets its own
    /// receiver and sees every progress entry. Subscribing AFTER the
    /// final progress entry has been emitted misses those entries —
    /// the client does not buffer history (callers wanting replay
    /// should call `task/get` instead).
    ///
    /// [`Receiver`]: tokio::sync::mpsc::UnboundedReceiver
    async fn subscribe_progress(&self, _task: TaskId) -> ProgressReceiver {
        // Default: closed channel so callers see a clean EOF instead
        // of hanging forever.
        let (_tx, rx) = mpsc::unbounded_channel();
        rx
    }

    /// Capability + version negotiated handshake.
    ///
    /// This is the host-facing entry point — it wraps the raw
    /// `initialize` RPC with the R16 contract:
    ///
    /// 1. Send `protocolVersion` + `clientCapabilities` to the
    ///    extension.
    /// 2. Validate the extension's reported `protocolVersion` against
    ///    `client_version` per [`ProtocolVersion::is_compatible`]
    ///    (different MAJOR = hard fail, MINOR mismatch = best-effort).
    /// 3. Mint a fresh [`SessionToken`] and stamp it onto the response
    ///    so subsequent reverse-requests can be authenticated by the
    ///    host-side gate.
    ///
    /// Concrete transport implementations override this only when they
    /// need to wire the session token into their own state (e.g.
    /// recording it for the reverse-request gate); the default
    /// behaviour is sufficient for the in-process client.
    async fn handshake(
        &self,
        client_version: ProtocolVersion,
        client_capabilities: Capabilities,
        client_info: String,
        opts: RequestOptions,
    ) -> ExtResult<InitializeResponse> {
        let req = InitializeRequest {
            protocol_version: client_version.to_wire(),
            client_capabilities: client_capabilities.to_wire(),
            client_info,
        };
        let mut resp = self.initialize(req, opts).await?;
        let ext_version = ProtocolVersion::parse(&resp.protocol_version)?;
        if !client_version.is_compatible(ext_version) {
            return Err(ExtensionError::unsupported_version(format!(
                "client {client} vs extension {ext}: MAJOR mismatch",
                client = client_version.to_wire(),
                ext = ext_version.to_wire(),
            )));
        }
        // Host always controls the session token — overwrite whatever
        // the extension echoed (extension echoes `""` per spec).
        resp.session_token = SessionToken::mint().as_str().to_string();
        Ok(resp)
    }

    /// Convenience wrapper around [`ExtensionClient::handshake`] that
    /// uses the compile-time [`CURRENT_PROTOCOL_VERSION`].
    async fn handshake_default(
        &self,
        client_capabilities: Capabilities,
        client_info: String,
    ) -> ExtResult<InitializeResponse> {
        self.handshake(
            CURRENT_PROTOCOL_VERSION,
            client_capabilities,
            client_info,
            RequestOptions::default(),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Reverse-request gate (T-9.5.8)
// ---------------------------------------------------------------------------

/// Server-side gate evaluated on every host-bound reverse-request from
/// an extension (`host/*`, `workspace/*`).
///
/// Decision (R16):
/// 1. Token presented matches the session token issued at `initialize`.
/// 2. The dotted capability identifier (e.g. `"host.fs.read"`) is
///    listed in [`Capabilities::granted`].
///
/// Both must hold; either failure surfaces as
/// [`ExtensionError::CapabilityDenied`] with the offending capability
/// in the error message. The gate has zero state of its own — the
/// session token + capability bag are the only inputs — so it can be
/// shared across tasks without locking.
#[derive(Debug, Clone)]
pub struct ReverseRequestGate {
    session_token: SessionToken,
    capabilities: Arc<Capabilities>,
}

impl ReverseRequestGate {
    /// Construct a fresh gate. Typically called once per extension
    /// session by the supervisor crate after a successful
    /// [`ExtensionClient::handshake`].
    pub fn new(session_token: SessionToken, capabilities: Capabilities) -> Self {
        Self {
            session_token,
            capabilities: Arc::new(capabilities),
        }
    }

    /// Borrow the session token (for diagnostic logging).
    pub fn session_token(&self) -> &SessionToken {
        &self.session_token
    }

    /// Borrow the granted capability set.
    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Check a single reverse-request. Returns `Ok(())` on grant,
    /// [`ExtensionError::CapabilityDenied`] on either token mismatch
    /// or missing capability.
    ///
    /// `presented_token` is the value the extension supplied with the
    /// reverse-request (an empty string if none). `capability` is the
    /// dotted form documented on [`Capabilities::granted`] — the
    /// caller is responsible for translating the JSON-RPC method name
    /// (e.g. `"host/fs/read"`) to the dotted form (`"host.fs.read"`)
    /// via [`method_to_capability`].
    pub fn check(&self, presented_token: &str, capability: &str) -> ExtResult<()> {
        if presented_token != self.session_token.as_str() {
            return Err(ExtensionError::capability_denied(format!(
                "{capability}: invalid session token"
            )));
        }
        if !self.capabilities.allows(capability) {
            return Err(ExtensionError::capability_denied(capability.to_string()));
        }
        Ok(())
    }
}

/// Translate a JSON-RPC method name (`"host/fs/read"`,
/// `"workspace/applyEdit"`) into the dotted capability identifier
/// stored in [`Capabilities::granted`] (`"host.fs.read"`,
/// `"workspace.applyEdit"`).
///
/// Returns `None` when the method does not require a capability gate
/// (e.g. lifecycle methods), so callers can short-circuit the check.
pub fn method_to_capability(method: &str) -> Option<String> {
    if method.starts_with("host/") || method.starts_with("workspace/") {
        Some(method.replace('/', "."))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests — handshake + capability + gate
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArkExtension;
    use async_trait::async_trait;

    /// Stub extension whose `initialize` echoes a configurable
    /// protocol_version + capability bag back to the client.
    struct VersionedExt {
        version: String,
        capabilities: String,
    }

    #[async_trait]
    impl ArkExtension for VersionedExt {
        async fn initialize(
            &self,
            req: InitializeRequest,
        ) -> ExtResult<InitializeResponse> {
            // Echo the client's capabilities back unchanged so the
            // round-trip can be observed by the test.
            let _ = req;
            Ok(InitializeResponse {
                protocol_version: self.version.clone(),
                extension_capabilities: self.capabilities.clone(),
                extension_info: r#"{"name":"versioned","version":"0.0.1"}"#.into(),
                session_token: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn protocol_version_parses_major_minor() {
        let v = ProtocolVersion::parse("1.2").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.to_wire(), "1.2");
    }

    #[tokio::test]
    async fn protocol_version_rejects_patch_component() {
        let err = ProtocolVersion::parse("1.2.3").unwrap_err();
        assert!(matches!(err, ExtensionError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn protocol_version_compatible_same_major() {
        let a = ProtocolVersion::new(1, 0);
        let b = ProtocolVersion::new(1, 9);
        assert!(a.is_compatible(b));
        assert!(b.is_compatible(a));
    }

    #[tokio::test]
    async fn protocol_version_incompatible_different_major() {
        let a = ProtocolVersion::new(1, 0);
        let b = ProtocolVersion::new(2, 0);
        assert!(!a.is_compatible(b));
    }

    #[tokio::test]
    async fn handshake_succeeds_when_majors_match() {
        let ext = VersionedExt {
            version: "1.5".into(),
            capabilities: r#"{"ui":{"keybind":true}}"#.into(),
        };
        let client = InProcClient::from_ext(ext);
        let resp = client
            .handshake(
                ProtocolVersion::new(1, 0),
                Capabilities::from_iter(["ui.keybind", "intents.dispatch"]),
                "ark-test".into(),
                RequestOptions::default(),
            )
            .await
            .expect("handshake should succeed on same MAJOR");
        assert_eq!(resp.protocol_version, "1.5");
        assert!(!resp.session_token.is_empty(), "host must mint token");
        // Capability round-trip survived the wire format.
        let caps = Capabilities::from_wire(&resp.extension_capabilities).unwrap();
        assert!(caps.allows("ui.keybind"));
    }

    #[tokio::test]
    async fn handshake_fails_on_major_mismatch() {
        let ext = VersionedExt {
            version: "2.0".into(),
            capabilities: "null".into(),
        };
        let client = InProcClient::from_ext(ext);
        let err = client
            .handshake(
                ProtocolVersion::new(1, 0),
                Capabilities::empty(),
                "ark-test".into(),
                RequestOptions::default(),
            )
            .await
            .expect_err("handshake should fail on MAJOR mismatch");
        match err {
            ExtensionError::UnsupportedVersion(msg) => {
                assert!(
                    msg.contains("MAJOR mismatch"),
                    "expected MAJOR mismatch message, got {msg}"
                );
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handshake_fails_on_garbage_version() {
        let ext = VersionedExt {
            version: "not-a-version".into(),
            capabilities: "null".into(),
        };
        let client = InProcClient::from_ext(ext);
        let err = client
            .handshake(
                ProtocolVersion::new(1, 0),
                Capabilities::empty(),
                "ark-test".into(),
                RequestOptions::default(),
            )
            .await
            .expect_err("handshake should fail on bad version");
        assert!(matches!(err, ExtensionError::InvalidParams(_)), "{err:?}");
    }

    #[test]
    fn capabilities_round_trip_object_of_objects() {
        let caps = Capabilities::from_iter([
            "ui.keybind",
            "ui.pane",
            "host.fs.read",
            "intents.dispatch",
        ]);
        let wire = caps.to_wire();
        // Should decode as `{ ui: { keybind: true, pane: true },
        // host: { fs: { read: true } }, intents: { dispatch: true } }`.
        let parsed: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert!(parsed["ui"]["keybind"].as_bool().unwrap());
        assert!(parsed["ui"]["pane"].as_bool().unwrap());
        assert!(parsed["host"]["fs"]["read"].as_bool().unwrap());
        assert!(parsed["intents"]["dispatch"].as_bool().unwrap());

        let decoded = Capabilities::from_wire(&wire).unwrap();
        assert_eq!(decoded, caps);
    }

    #[test]
    fn capabilities_from_wire_rejects_non_object_root() {
        let err = Capabilities::from_wire(r#""boom""#).unwrap_err();
        assert!(matches!(err, ExtensionError::InvalidParams(_)), "{err:?}");
    }

    #[test]
    fn reverse_request_gate_grants_when_token_and_cap_match() {
        let token = SessionToken::from_string("tok-abc");
        let caps = Capabilities::from_iter(["host.fs.read"]);
        let gate = ReverseRequestGate::new(token, caps);
        gate.check("tok-abc", "host.fs.read")
            .expect("matching token + cap should grant");
    }

    #[test]
    fn reverse_request_gate_denies_on_token_mismatch() {
        let token = SessionToken::from_string("tok-abc");
        let caps = Capabilities::from_iter(["host.fs.read"]);
        let gate = ReverseRequestGate::new(token, caps);
        let err = gate.check("tok-bad", "host.fs.read").unwrap_err();
        match err {
            ExtensionError::CapabilityDenied(m) => {
                assert!(m.contains("invalid session token"), "{m}")
            }
            other => panic!("expected CapabilityDenied, got {other:?}"),
        }
    }

    #[test]
    fn reverse_request_gate_denies_on_missing_capability() {
        let token = SessionToken::from_string("tok-abc");
        let caps = Capabilities::from_iter(["ui.keybind"]);
        let gate = ReverseRequestGate::new(token, caps);
        let err = gate.check("tok-abc", "host.fs.read").unwrap_err();
        match err {
            ExtensionError::CapabilityDenied(m) => {
                assert!(m.contains("host.fs.read"), "{m}")
            }
            other => panic!("expected CapabilityDenied, got {other:?}"),
        }
    }

    #[test]
    fn method_to_capability_maps_host_and_workspace() {
        assert_eq!(
            method_to_capability("host/fs/read"),
            Some("host.fs.read".into())
        );
        assert_eq!(
            method_to_capability("workspace/applyEdit"),
            Some("workspace.applyEdit".into())
        );
        assert_eq!(method_to_capability("ping"), None);
        assert_eq!(method_to_capability("task/create"), None);
    }
}
