//! NDJSON / JSON-RPC 2.0 transport for subprocess extensions.
//!
//! Each line on the wire is exactly one JSON object — a `Request`,
//! `Response`, or `Notification` envelope per JSON-RPC 2.0. The client
//! spawns a child process (or wraps a pair of pipe halves), drives a
//! background read loop that routes responses to per-request
//! `oneshot::Sender<Response>` entries keyed by JSON-RPC id, and a
//! background write loop that drains an mpsc of outgoing messages onto
//! stdin.
//!
//! # Correlation + cancel
//!
//! * Each outgoing request mints a fresh `u64` id from an
//!   `AtomicU64::fetch_add(1)` counter.
//! * The client registers a `oneshot::Sender` in a shared
//!   `Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>` before
//!   sending.
//! * The read loop pops the entry when the matching response arrives
//!   and forwards it through the channel.
//! * On timeout the client fires a `$/cancel` notification with the
//!   pending id and removes the pending entry; a late response that
//!   arrives after cancel is dropped silently (per MCP semantics).
//!
//! # Ownership
//!
//! [`NdjsonClient`] is `Clone` and carries `Arc`-wrapped state so it
//! can be cheaply handed out to every ark layer that needs to dispatch
//! into the extension. Dropping the last clone closes stdin (via the
//! writer task's channel close) and lets the background tasks wind
//! down; the caller is responsible for reaping the child.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use super::{ExtensionClient, ProgressReceiver, RequestOptions, TaskProgress};
use crate::*;

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Protocol tag — always `"2.0"`.
    pub jsonrpc: String,
    /// Request identifier. Must be unique per in-flight request on a
    /// connection; the response with the matching id is routed back.
    pub id: u64,
    /// JSON-RPC method name (e.g. `"initialize"`, `"task/create"`).
    pub method: String,
    /// Method parameters — a JSON object or `null` if the method has
    /// no params.
    pub params: Value,
}

/// JSON-RPC 2.0 notification envelope.
///
/// Notifications carry no id and receive no response (`$/cancel`,
/// `$/progress`, `log/write`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Protocol tag — always `"2.0"`.
    pub jsonrpc: String,
    /// JSON-RPC method name.
    pub method: String,
    /// Method parameters.
    pub params: Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Protocol tag — always `"2.0"`.
    pub jsonrpc: String,
    /// Response identifier — matches the [`Request::id`] that prompted
    /// this response. Carried as `Option<u64>` on the wire for
    /// forward-compat with string ids (we only mint numeric ones).
    pub id: Option<u64>,
    /// Method result on success. Mutually exclusive with
    /// [`Response::error`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error descriptor on failure. Mutually exclusive with
    /// [`Response::result`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    /// Numeric error code. Standard JSON-RPC codes are in
    /// `-32768..-32000`; `-32601` = method not found, `-32602` =
    /// invalid params. Extension-specific errors live outside that
    /// range.
    pub code: i32,
    /// Human-readable message.
    pub message: String,
    /// Optional structured error data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    /// Convert a wire error into [`ExtensionError`]. The standard
    /// JSON-RPC codes map onto matching variants; everything else
    /// funnels into [`ExtensionError::Internal`] with the raw message.
    pub fn into_extension_error(self) -> ExtensionError {
        match self.code {
            -32601 => ExtensionError::MethodNotFound(self.message),
            -32602 => ExtensionError::InvalidParams(self.message),
            _ => ExtensionError::Internal(self.message),
        }
    }
}

/// Any message the read loop can see on the wire. JSON-RPC conflates
/// request/response/notification into a single object shape — we
/// disambiguate by looking at which fields are present.
///
/// Variant-contents are currently opaque to the read-loop (v1 only
/// routes the `Response` variant); the carried payload lives in the
/// enum as documentation of the on-wire encoding even though it isn't
/// consumed yet. T-9.5.6 wires server-side request + notification
/// dispatch.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum WireMessage {
    /// Request: has `id` AND `method`. Server-to-client; used by the
    /// reverse-path when the extension invokes host methods.
    /// Listed first so serde's untagged matcher tries the most
    /// distinctive shape (requires `method`) before the looser
    /// `Response` variant whose `result`/`error` fields are both
    /// optional.
    Request(Request),
    /// Notification: has `method` but no `id`.
    Notification(Notification),
    /// Response: has `id` AND (`result` OR `error`).
    Response(Response),
}

// ---------------------------------------------------------------------------
// NdjsonClient
// ---------------------------------------------------------------------------

/// Per-task progress subscriber registry. The read loop demuxes
/// `$/progress` notifications by their `token` field and forwards each
/// entry to every live subscriber. Subscribers are appended via
/// [`NdjsonClient::subscribe_progress`]; senders are pruned lazily
/// (next dispatch drops dead receivers).
type ProgressBus = Arc<Mutex<HashMap<String, Vec<mpsc::UnboundedSender<TaskProgress>>>>>;

/// Shared state behind every [`NdjsonClient`] clone.
struct Inner {
    /// Monotonic request id generator.
    next_id: AtomicU64,
    /// In-flight requests → the oneshot that receives their response.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>,
    /// Outgoing-message queue. The writer task drains this onto stdin.
    tx: mpsc::UnboundedSender<String>,
    /// Per-task subscriber registry for `$/progress` notifications.
    progress: ProgressBus,
    /// Handles for the background read + write tasks. Dropped on
    /// [`NdjsonClient::shutdown_transport`] to release resources.
    read_handle: Mutex<Option<JoinHandle<()>>>,
    write_handle: Mutex<Option<JoinHandle<()>>>,
}

/// Bidirectional NDJSON / JSON-RPC 2.0 client over an async stdio pair.
///
/// Construct via [`NdjsonClient::spawn`] for the common subprocess case,
/// or [`NdjsonClient::from_halves`] when wrapping an existing reader /
/// writer pair (tests, in-memory pipes, or non-process transports like
/// TCP).
#[derive(Clone)]
pub struct NdjsonClient {
    inner: Arc<Inner>,
}

impl NdjsonClient {
    /// Spawn `cmd` with piped stdin/stdout and wrap it as a client.
    ///
    /// The child process is not stored by the client — callers keep the
    /// [`Child`] handle so they can `.wait()` / `.kill()` per R16
    /// supervision (ark follows `shutdown` RPC → stdin-close →
    /// `SIGTERM` → `SIGKILL`).
    pub fn spawn(mut cmd: Command) -> io::Result<(Self, Child)> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        Ok((Self::from_halves(stdout, stdin), child))
    }

    /// Wrap a generic `(read, write)` pair. Used by tests that feed in
    /// a `tokio::io::duplex` pipe and by future non-process transports.
    pub fn from_halves<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let progress: ProgressBus = Arc::new(Mutex::new(HashMap::new()));

        let read_handle = tokio::spawn(Self::read_loop(
            reader,
            pending.clone(),
            progress.clone(),
        ));
        let write_handle = tokio::spawn(Self::write_loop(writer, rx));

        Self {
            inner: Arc::new(Inner {
                next_id: AtomicU64::new(1),
                pending,
                tx,
                progress,
                read_handle: Mutex::new(Some(read_handle)),
                write_handle: Mutex::new(Some(write_handle)),
            }),
        }
    }

    /// Background read task — reads one NDJSON object per line and
    /// routes it to either a pending oneshot (responses) or the
    /// per-task progress bus (`$/progress` notifications). T-9.5.6
    /// wires the progress demux; reverse-path requests
    /// (`host/*`/`workspace/*`) are accepted but not yet dispatched.
    async fn read_loop<R>(
        reader: R,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>,
        progress: ProgressBus,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let msg: WireMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => {
                    // Drop unparseable lines. A production client would
                    // route these to tracing; we stay dep-light here.
                    continue;
                }
            };
            match msg {
                WireMessage::Response(resp) => {
                    if let Some(id) = resp.id {
                        let tx = pending.lock().await.remove(&id);
                        if let Some(tx) = tx {
                            // Fire-and-forget: if the receiver already
                            // gave up (timeout / cancel) this drops.
                            let _ = tx.send(resp);
                        }
                    }
                }
                WireMessage::Notification(notif) => {
                    if notif.method == "$/progress" {
                        if let Some(entry) = decode_progress(&notif.params) {
                            Self::route_progress(&progress, entry).await;
                        }
                    }
                    // Other notifications (`log/write`, etc.) are
                    // not surfaced in v1.
                }
                WireMessage::Request(_) => {
                    // Reverse-path requests are accepted but not yet
                    // dispatched (T-9.5.8 wires the gate); the
                    // server-side handler lives in the supervisor
                    // crate.
                }
            }
        }
    }

    /// Forward one decoded [`TaskProgress`] entry to every live
    /// subscriber for that task. Dead subscribers (receivers dropped)
    /// are pruned in place — keeps the registry from growing
    /// unbounded.
    async fn route_progress(progress: &ProgressBus, entry: TaskProgress) {
        let mut bus = progress.lock().await;
        let Some(subscribers) = bus.get_mut(&entry.task) else {
            return;
        };
        subscribers.retain(|tx| tx.send(entry.clone()).is_ok());
        if subscribers.is_empty() {
            bus.remove(&entry.task);
        }
    }

    /// Background write task — drains the outgoing-message queue onto
    /// the writer with `\n` framing. Exits cleanly when every sender
    /// clone is dropped.
    async fn write_loop<W>(mut writer: W, mut rx: mpsc::UnboundedReceiver<String>)
    where
        W: AsyncWrite + Unpin,
    {
        while let Some(line) = rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
        let _ = writer.shutdown().await;
    }

    /// Core request driver. Serializes the typed params, mints an id,
    /// registers a oneshot, sends, awaits the response under the
    /// configured timeout, deserializes, and returns.
    ///
    /// On timeout this emits a `$/cancel` notification with the pending
    /// id (MCP semantics), removes the pending entry, and returns
    /// [`ExtensionError::Internal`] with a `timeout` message so ark's
    /// diagnostics can attribute the failure.
    pub async fn call<P, R>(&self, method: &str, params: P, opts: RequestOptions) -> ExtResult<R>
    where
        P: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let params = serde_json::to_value(&params)
            .map_err(|e| ExtensionError::Internal(format!("serialize params: {e}")))?;
        let req = Request {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        };
        let body = serde_json::to_string(&req)
            .map_err(|e| ExtensionError::Internal(format!("serialize request: {e}")))?;

        let (resp_tx, resp_rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, resp_tx);

        if self.inner.tx.send(body).is_err() {
            self.inner.pending.lock().await.remove(&id);
            return Err(ExtensionError::Internal("transport closed".into()));
        }

        let deadline = Instant::now() + opts.timeout;
        match timeout_at(deadline, resp_rx).await {
            Ok(Ok(response)) => {
                if let Some(err) = response.error {
                    return Err(err.into_extension_error());
                }
                let value = response.result.unwrap_or(Value::Null);
                serde_json::from_value(value)
                    .map_err(|e| ExtensionError::Internal(format!("deserialize result: {e}")))
            }
            Ok(Err(_cancelled)) => Err(ExtensionError::Internal(
                "response channel dropped before reply".into(),
            )),
            Err(_elapsed) => {
                // Remove pending entry; a late response is silently
                // dropped by the read-loop thanks to the removed key.
                self.inner.pending.lock().await.remove(&id);
                // Fire a `$/cancel` notification so the extension can
                // abort server-side work (MCP cancellation semantics).
                let _ = self
                    .send_notification(
                        "$/cancel",
                        serde_json::json!({ "id": id.to_string() }),
                    )
                    .await;
                Err(ExtensionError::Internal(format!(
                    "request {method} timed out after {:?}",
                    opts.timeout
                )))
            }
        }
    }

    /// Send a one-way notification. No id, no response awaiting.
    pub async fn send_notification<P: Serialize>(
        &self,
        method: &str,
        params: P,
    ) -> ExtResult<()> {
        let params = serde_json::to_value(&params)
            .map_err(|e| ExtensionError::Internal(format!("serialize params: {e}")))?;
        let n = Notification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        };
        let body = serde_json::to_string(&n)
            .map_err(|e| ExtensionError::Internal(format!("serialize notification: {e}")))?;
        self.inner
            .tx
            .send(body)
            .map_err(|_| ExtensionError::Internal("transport closed".into()))?;
        Ok(())
    }

    /// Manually cancel an outstanding request by id. Exposed for tests
    /// and for any future hot-path that wants to short-circuit a call
    /// (`ark cancel` cli verb, for instance).
    pub async fn cancel_id(&self, id: u64) -> ExtResult<()> {
        // Drop the pending entry — the read-loop will then silently
        // drop any matching late response.
        self.inner.pending.lock().await.remove(&id);
        self.send_notification("$/cancel", serde_json::json!({ "id": id.to_string() }))
            .await
    }

    /// Tear the background tasks down. Usually not needed — dropping
    /// the last clone closes the write channel naturally — but tests
    /// call this for determinism.
    pub async fn shutdown_transport(&self) {
        if let Some(h) = self.inner.read_handle.lock().await.take() {
            h.abort();
        }
        if let Some(h) = self.inner.write_handle.lock().await.take() {
            h.abort();
        }
    }

    /// Register a subscriber for `$/progress` notifications keyed by
    /// `task_id`. The returned [`ProgressReceiver`] yields one
    /// [`TaskProgress`] entry per matching notification until the
    /// extension stops emitting (no explicit "end" signal — callers
    /// also hold the [`crate::TaskGetResponse::status`] field as the
    /// authoritative completion marker).
    ///
    /// Multiple subscribers per task are allowed; each gets its own
    /// receiver and sees every entry. Late subscribers (after some
    /// entries have already been emitted) miss the earlier ones —
    /// the bus is not buffered.
    pub async fn subscribe_to_task(&self, task_id: &str) -> ProgressReceiver {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner
            .progress
            .lock()
            .await
            .entry(task_id.to_string())
            .or_default()
            .push(tx);
        rx
    }
}

/// Decode a `$/progress` notification's `params` payload into the
/// transport-typed [`TaskProgress`] struct. Returns `None` when the
/// payload doesn't carry a `token` (per JSON-RPC progress
/// conventions); this guards against the read loop forwarding garbage.
fn decode_progress(params: &Value) -> Option<TaskProgress> {
    // The notification carries `{ token, value: <opaque> }` — the
    // extension protocol's `ProgressRequest` type. `value` itself is
    // an LSP-style envelope; we extract `percentage` and `message`
    // best-effort and stash the raw payload for callers needing the
    // full `kind`/etc fields.
    let token = params.get("token")?.as_str()?.to_string();
    let value = params.get("value").cloned().unwrap_or(Value::Null);
    let raw = match &value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // The `value` field is an `OpaqueJson` — a JSON-encoded string.
    // The extension's `ProgressRequest` carries the wrapper as a
    // string; the inner LSP-style envelope lives inside that. Try to
    // parse the inner form so we can pluck `message` + `percentage`.
    let inner = match &value {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    };
    let (percent, message) = inner
        .as_ref()
        .map(|v| {
            let p = v
                .get("percentage")
                .and_then(|x| x.as_u64())
                .unwrap_or(0)
                .min(100) as u8;
            let m = v
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            (p, m)
        })
        .unwrap_or((0, String::new()));
    Some(TaskProgress {
        task: token,
        percent,
        message,
        raw,
    })
}

#[async_trait]
impl ExtensionClient for NdjsonClient {
    // -- Lifecycle -----------------------------------------------------------

    async fn initialize(
        &self,
        req: InitializeRequest,
        opts: RequestOptions,
    ) -> ExtResult<InitializeResponse> {
        self.call("initialize", req, opts).await
    }

    async fn initialized(
        &self,
        req: InitializedRequest,
        _opts: RequestOptions,
    ) -> ExtResult<InitializedResponse> {
        self.send_notification("initialized", req).await?;
        Ok(InitializedResponse::default())
    }

    async fn shutdown(
        &self,
        req: ShutdownRequest,
        opts: RequestOptions,
    ) -> ExtResult<ShutdownResponse> {
        self.call("shutdown", req, opts).await
    }

    async fn ping(&self, req: PingRequest, opts: RequestOptions) -> ExtResult<PingResponse> {
        self.call("ping", req, opts).await
    }

    // -- Async + cancel ------------------------------------------------------

    async fn cancel(
        &self,
        req: CancelRequest,
        _opts: RequestOptions,
    ) -> ExtResult<CancelResponse> {
        self.send_notification("$/cancel", req).await?;
        Ok(CancelResponse::default())
    }

    async fn progress(
        &self,
        req: ProgressRequest,
        _opts: RequestOptions,
    ) -> ExtResult<ProgressResponse> {
        self.send_notification("$/progress", req).await?;
        Ok(ProgressResponse::default())
    }

    async fn task_create(
        &self,
        req: TaskCreateRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskCreateResponse> {
        self.call("task/create", req, opts).await
    }

    async fn task_get(
        &self,
        req: TaskGetRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskGetResponse> {
        self.call("task/get", req, opts).await
    }

    async fn task_cancel(
        &self,
        req: TaskCancelRequest,
        opts: RequestOptions,
    ) -> ExtResult<TaskCancelResponse> {
        self.call("task/cancel", req, opts).await
    }

    // -- Event bus -----------------------------------------------------------

    async fn event_subscribe(
        &self,
        req: EventSubscribeRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventSubscribeResponse> {
        self.call("event/subscribe", req, opts).await
    }

    async fn event_unsubscribe(
        &self,
        req: EventUnsubscribeRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventUnsubscribeResponse> {
        self.call("event/unsubscribe", req, opts).await
    }

    async fn event_emit(
        &self,
        req: EventEmitRequest,
        opts: RequestOptions,
    ) -> ExtResult<EventEmitResponse> {
        self.call("event/emit", req, opts).await
    }

    async fn event_notify(
        &self,
        req: EventNotifyRequest,
        _opts: RequestOptions,
    ) -> ExtResult<EventNotifyResponse> {
        self.send_notification("event/notify", req).await?;
        Ok(EventNotifyResponse::default())
    }

    // -- Intents -------------------------------------------------------------

    async fn intent_register(
        &self,
        req: IntentRegisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentRegisterResponse> {
        self.call("intent/register", req, opts).await
    }

    async fn intent_unregister(
        &self,
        req: IntentUnregisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentUnregisterResponse> {
        self.call("intent/unregister", req, opts).await
    }

    async fn intent_dispatch(
        &self,
        req: IntentDispatchRequest,
        opts: RequestOptions,
    ) -> ExtResult<IntentDispatchResponse> {
        self.call("intent/dispatch", req, opts).await
    }

    // -- UI: keybind / status ------------------------------------------------

    async fn ui_keybind_register(
        &self,
        req: UiKeybindRegisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiKeybindRegisterResponse> {
        self.call("ui/keybind/register", req, opts).await
    }

    async fn ui_keybind_unregister(
        &self,
        req: UiKeybindUnregisterRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiKeybindUnregisterResponse> {
        self.call("ui/keybind/unregister", req, opts).await
    }

    async fn ui_status_push(
        &self,
        req: UiStatusPushRequest,
        _opts: RequestOptions,
    ) -> ExtResult<UiStatusPushResponse> {
        self.send_notification("ui/status/push", req).await?;
        Ok(UiStatusPushResponse::default())
    }

    // -- UI: panes -----------------------------------------------------------

    async fn ui_pane_request(
        &self,
        req: UiPaneRequestRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiPaneRequestResponse> {
        self.call("ui/pane/request", req, opts).await
    }

    async fn ui_pane_close(
        &self,
        req: UiPaneCloseRequest,
        opts: RequestOptions,
    ) -> ExtResult<UiPaneCloseResponse> {
        self.call("ui/pane/close", req, opts).await
    }

    // -- Workspace -----------------------------------------------------------

    async fn workspace_apply_edit(
        &self,
        req: WorkspaceApplyEditRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceApplyEditResponse> {
        self.call("workspace/applyEdit", req, opts).await
    }

    async fn workspace_configuration(
        &self,
        req: WorkspaceConfigurationRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceConfigurationResponse> {
        self.call("workspace/configuration", req, opts).await
    }

    async fn workspace_show_document(
        &self,
        req: WorkspaceShowDocumentRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowDocumentResponse> {
        self.call("workspace/showDocument", req, opts).await
    }

    async fn workspace_show_message(
        &self,
        req: WorkspaceShowMessageRequest,
        _opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageResponse> {
        self.send_notification("workspace/showMessage", req).await?;
        Ok(WorkspaceShowMessageResponse::default())
    }

    async fn workspace_show_message_request(
        &self,
        req: WorkspaceShowMessageRequestRequest,
        opts: RequestOptions,
    ) -> ExtResult<WorkspaceShowMessageRequestResponse> {
        self.call("workspace/showMessageRequest", req, opts).await
    }

    // -- Scene ---------------------------------------------------------------

    async fn scene_get_root(
        &self,
        req: SceneGetRootRequest,
        opts: RequestOptions,
    ) -> ExtResult<SceneGetRootResponse> {
        self.call("scene/getRoot", req, opts).await
    }

    // -- Host syscalls (wasm-only) -------------------------------------------

    async fn host_fs_read(
        &self,
        req: HostFsReadRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostFsReadResponse> {
        self.call("host/fs/read", req, opts).await
    }

    async fn host_fs_write(
        &self,
        req: HostFsWriteRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostFsWriteResponse> {
        self.call("host/fs/write", req, opts).await
    }

    async fn host_proc_spawn(
        &self,
        req: HostProcSpawnRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostProcSpawnResponse> {
        self.call("host/proc/spawn", req, opts).await
    }

    async fn host_net_fetch(
        &self,
        req: HostNetFetchRequest,
        opts: RequestOptions,
    ) -> ExtResult<HostNetFetchResponse> {
        self.call("host/net/fetch", req, opts).await
    }

    // -- Logging -------------------------------------------------------------

    async fn log_write(
        &self,
        req: LogWriteRequest,
        _opts: RequestOptions,
    ) -> ExtResult<LogWriteResponse> {
        self.send_notification("log/write", req).await?;
        Ok(LogWriteResponse::default())
    }

    async fn log_set_level(
        &self,
        req: LogSetLevelRequest,
        opts: RequestOptions,
    ) -> ExtResult<LogSetLevelResponse> {
        self.call("log/setLevel", req, opts).await
    }

    // -- Capability + progress hooks -----------------------------------------

    async fn subscribe_progress(&self, task: TaskId) -> ProgressReceiver {
        self.subscribe_to_task(&task.value).await
    }
}

// ---------------------------------------------------------------------------
// NdjsonServer — host-side helper for echo-style stubs
// ---------------------------------------------------------------------------

/// Minimal server-side helper for NDJSON / JSON-RPC 2.0.
///
/// Wraps an [`ArkExtension`] impl and drives its methods from an
/// `AsyncRead + AsyncWrite` pair. v1 handles the request subset used by
/// the client's round-trip tests; full method coverage ships with
/// T-9.5.6 (host-side dispatcher). The server currently dispatches
/// notifications to `ping`-like methods and responds to `ping` /
/// `task/get` / `initialize` per the test matrix.
///
/// The server is intentionally **small** — production host-side
/// dispatch lives in the supervisor crate. This struct exists so the
/// transport crate can own an in-crate end-to-end test without pulling
/// in a bigger dep graph.
pub struct NdjsonServer;

impl NdjsonServer {
    /// Drive `ext` to completion on `(reader, writer)` — reads one
    /// NDJSON request per line, dispatches, writes the response back,
    /// loops until EOF. Returns the total number of requests handled.
    pub async fn serve<R, W, E>(reader: R, mut writer: W, ext: Arc<E>) -> io::Result<u64>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        E: ArkExtension + 'static,
    {
        let mut lines = BufReader::new(reader).lines();
        let mut handled = 0u64;
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let msg: WireMessage = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let req = match msg {
                WireMessage::Request(r) => r,
                _ => continue,
            };
            let resp = Self::dispatch(&ext, req).await;
            let body = serde_json::to_string(&resp)
                .unwrap_or_else(|e| format!("{{\"serialize-error\":\"{e}\"}}"));
            writer.write_all(body.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            handled += 1;
        }
        Ok(handled)
    }

    /// Dispatch a single parsed [`Request`] against an extension impl
    /// and produce a wire-ready [`Response`].
    async fn dispatch<E: ArkExtension>(ext: &Arc<E>, req: Request) -> Response {
        let id = req.id;
        let method = req.method.as_str();
        let result: Result<Value, ExtensionError> = match method {
            "initialize" => dispatch_typed::<E, InitializeRequest, InitializeResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.initialize(r).await },
            )
            .await,
            "shutdown" => dispatch_typed::<E, ShutdownRequest, ShutdownResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.shutdown(r).await },
            )
            .await,
            "ping" => dispatch_typed::<E, PingRequest, PingResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.ping(r).await },
            )
            .await,
            "task/create" => dispatch_typed::<E, TaskCreateRequest, TaskCreateResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.task_create(r).await },
            )
            .await,
            "task/get" => dispatch_typed::<E, TaskGetRequest, TaskGetResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.task_get(r).await },
            )
            .await,
            other => Err(ExtensionError::method_not_found(other)),
        };
        match result {
            Ok(v) => Response {
                jsonrpc: "2.0".into(),
                id: Some(id),
                result: Some(v),
                error: None,
            },
            Err(e) => Response {
                jsonrpc: "2.0".into(),
                id: Some(id),
                result: None,
                error: Some(ResponseError {
                    code: match &e {
                        ExtensionError::MethodNotFound(_) => -32601,
                        ExtensionError::InvalidParams(_) => -32602,
                        _ => -32000,
                    },
                    message: e.to_string(),
                    data: None,
                }),
            },
        }
    }
}

/// Parse, await, and re-serialize a typed request/response pair so the
/// dispatcher table stays type-generic. Error paths mirror JSON-RPC
/// `-32602 invalid params` for deserialization failures.
async fn dispatch_typed<E, Req, Resp, Fut>(
    ext: &Arc<E>,
    params: Value,
    call: impl FnOnce(Arc<E>, Req) -> Fut,
) -> Result<Value, ExtensionError>
where
    E: ArkExtension,
    Req: for<'de> Deserialize<'de>,
    Resp: Serialize,
    Fut: std::future::Future<Output = ExtResult<Resp>>,
{
    let req: Req = serde_json::from_value(params)
        .map_err(|e| ExtensionError::InvalidParams(e.to_string()))?;
    let resp = call(ext.clone(), req).await?;
    serde_json::to_value(&resp).map_err(|e| ExtensionError::Internal(e.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::duplex;

    /// Stub extension that returns a non-default `PingResponse` so
    /// tests can assert the round-trip actually reached the impl.
    struct EchoExt;
    #[async_trait]
    impl ArkExtension for EchoExt {
        async fn ping(&self, _req: PingRequest) -> ExtResult<PingResponse> {
            Ok(PingResponse::default())
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
    }

    /// Wire an [`NdjsonClient`] + [`NdjsonServer`] across an in-memory
    /// duplex pair, drive a round-trip, and assert the response was
    /// routed through the id-correlation table correctly.
    #[tokio::test]
    async fn request_response_round_trip() {
        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let server_handle = tokio::spawn(async move {
            NdjsonServer::serve(server_r, server_w, Arc::new(EchoExt))
                .await
                .unwrap()
        });

        let resp = client
            .task_create(
                TaskCreateRequest {
                    label: "hello".into(),
                    params: "null".into(),
                },
                RequestOptions::default(),
            )
            .await
            .expect("task_create round-trip");
        assert_eq!(resp.task.value, "task:hello");

        let got = client
            .task_get(
                TaskGetRequest {
                    task: TaskId {
                        value: "abc".into(),
                    },
                },
                RequestOptions::default(),
            )
            .await
            .expect("task_get round-trip");
        assert_eq!(got.status, "succeeded");
        assert_eq!(got.result.as_deref(), Some("\"echoed:abc\""));

        client.shutdown_transport().await;
        // Server sees EOF and exits cleanly.
        let handled = server_handle.await.unwrap();
        assert_eq!(handled, 2);
    }

    /// Stub extension whose method never returns so the client hits
    /// its timeout. Used for the cancel-on-timeout + `$/cancel` flow.
    struct BlackholeExt;
    #[async_trait]
    impl ArkExtension for BlackholeExt {
        async fn task_create(
            &self,
            _req: TaskCreateRequest,
        ) -> ExtResult<TaskCreateResponse> {
            // Park forever — timeout is the only escape.
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn call_times_out_when_extension_never_replies() {
        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let _server = tokio::spawn(async move {
            NdjsonServer::serve(server_r, server_w, Arc::new(BlackholeExt))
                .await
                .ok()
        });

        let err = client
            .task_create(
                TaskCreateRequest {
                    label: "stuck".into(),
                    params: "null".into(),
                },
                RequestOptions {
                    timeout: Duration::from_millis(100),
                },
            )
            .await
            .expect_err("should have timed out");
        match err {
            ExtensionError::Internal(m) => assert!(m.contains("timed out"), "msg = {m}"),
            other => panic!("expected Internal(timeout), got {other:?}"),
        }
        client.shutdown_transport().await;
    }

    #[tokio::test]
    async fn cancel_id_removes_pending_entry() {
        // Use duplex pair but don't wire a server — we just want the
        // client to have a pending id we can clean up.
        let (client_io, _server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let client = NdjsonClient::from_halves(client_r, client_w);

        // Spawn a call in the background so we can cancel it.
        let client_for_call = client.clone();
        let call = tokio::spawn(async move {
            client_for_call
                .ping(
                    PingRequest::default(),
                    RequestOptions {
                        timeout: Duration::from_secs(5),
                    },
                )
                .await
        });

        // Give the call time to register its pending entry.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(client.inner.pending.lock().await.len(), 1);

        client.cancel_id(1).await.unwrap();
        // After cancel, the entry is gone.
        assert_eq!(client.inner.pending.lock().await.len(), 0);

        // The in-flight call is now orphaned — it will timeout on its
        // 5s deadline. Abort the task so the test finishes promptly.
        call.abort();
        client.shutdown_transport().await;
    }

    /// T-9.5.6: end-to-end progress round-trip. The server writes a
    /// task-create response then emits three `$/progress` notifications
    /// whose `token` matches the minted task id; the client subscribes
    /// before the notifications hit and sees all three.
    #[tokio::test]
    async fn progress_notifications_route_to_subscriber() {
        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (mut server_r, mut server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);

        // Subscribe BEFORE the server emits anything.
        let mut rx = client.subscribe_to_task("task-1").await;

        // Hand-roll the server side so we can interleave a response
        // and three progress notifications on the same wire.
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(&mut server_r).lines();
            // Wait for the task/create request.
            let req_line = lines.next_line().await.unwrap().unwrap();
            let req: Request = serde_json::from_str(&req_line).unwrap();
            assert_eq!(req.method, "task/create");

            // Reply with the task id.
            let resp = Response {
                jsonrpc: "2.0".into(),
                id: Some(req.id),
                result: Some(serde_json::json!({
                    "task": { "value": "task-1" }
                })),
                error: None,
            };
            let line = serde_json::to_string(&resp).unwrap();
            server_w.write_all(line.as_bytes()).await.unwrap();
            server_w.write_all(b"\n").await.unwrap();
            server_w.flush().await.unwrap();

            // Emit three progress notifications.
            for (pct, msg) in [(10u8, "starting"), (50, "halfway"), (100, "done")] {
                let value = serde_json::json!({
                    "kind": "report",
                    "percentage": pct,
                    "message": msg,
                })
                .to_string();
                let n = Notification {
                    jsonrpc: "2.0".into(),
                    method: "$/progress".into(),
                    params: serde_json::json!({
                        "token": "task-1",
                        "value": value,
                    }),
                };
                let line = serde_json::to_string(&n).unwrap();
                server_w.write_all(line.as_bytes()).await.unwrap();
                server_w.write_all(b"\n").await.unwrap();
                server_w.flush().await.unwrap();
            }
        });

        // Drive the request; result not strictly needed.
        let resp = client
            .task_create(
                TaskCreateRequest {
                    label: "demo".into(),
                    params: "null".into(),
                },
                RequestOptions::default(),
            )
            .await
            .expect("task_create round-trip");
        assert_eq!(resp.task.value, "task-1");

        // Collect three progress entries.
        let mut entries = Vec::new();
        for _ in 0..3 {
            let entry = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("recv timed out")
                .expect("channel closed");
            entries.push(entry);
        }

        assert_eq!(entries[0].task, "task-1");
        assert_eq!(entries[0].percent, 10);
        assert_eq!(entries[0].message, "starting");
        assert_eq!(entries[1].percent, 50);
        assert_eq!(entries[2].percent, 100);
        assert_eq!(entries[2].message, "done");

        server.await.unwrap();
        client.shutdown_transport().await;
    }

    /// Subscribers for tasks the server never references receive nothing
    /// — and dropping the receiver does not poison the bus.
    #[tokio::test]
    async fn progress_subscriber_is_pruned_when_dropped() {
        let (client_io, _server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let client = NdjsonClient::from_halves(client_r, client_w);

        // Two subscribers for the same task; drop one, verify the bus
        // has a single live entry left.
        let _rx_alive = client.subscribe_to_task("t1").await;
        let rx_drop = client.subscribe_to_task("t1").await;
        drop(rx_drop);

        // Synthesise a progress entry and route it; the alive
        // subscriber receives it, the dropped one is pruned.
        let entry = TaskProgress {
            task: "t1".into(),
            percent: 50,
            message: "tick".into(),
            raw: "{}".into(),
        };
        NdjsonClient::route_progress(&client.inner.progress, entry).await;

        let bus = client.inner.progress.lock().await;
        let subscribers = bus.get("t1").expect("bus entry should still exist");
        assert_eq!(subscribers.len(), 1);
        drop(bus);
        client.shutdown_transport().await;
    }

    #[tokio::test]
    async fn method_not_found_propagates_to_client_error() {
        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        struct EmptyExt;
        #[async_trait]
        impl ArkExtension for EmptyExt {}
        let _server = tokio::spawn(async move {
            NdjsonServer::serve(server_r, server_w, Arc::new(EmptyExt))
                .await
                .ok()
        });

        let err = client
            .initialize(
                InitializeRequest {
                    protocol_version: "0.1".into(),
                    client_capabilities: "null".into(),
                    client_info: "ark-test".into(),
                },
                RequestOptions::default(),
            )
            .await
            .expect_err("stub has no initialize override");
        match err {
            ExtensionError::MethodNotFound(_) => {}
            other => panic!("expected MethodNotFound, got {other:?}"),
        }
        client.shutdown_transport().await;
    }
}
