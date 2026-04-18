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

use super::{
    ExtensionClient, ProgressReceiver, RequestOptions, ReverseRequestGate, TaskProgress,
    method_to_capability,
};
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
    /// JSON-RPC codes (`-32601`, `-32602`) map onto matching variants;
    /// the ark-extended codes (`-32003` capability-denied, `-32004`
    /// unsupported-version, `-32005` crashed, `-32006` handle-gone)
    /// map onto their typed variants. Everything else funnels into
    /// [`ExtensionError::Internal`] with the raw message.
    pub fn into_extension_error(self) -> ExtensionError {
        match self.code {
            -32601 => ExtensionError::MethodNotFound(self.message),
            -32602 => ExtensionError::InvalidParams(self.message),
            -32003 => ExtensionError::CapabilityDenied(self.message),
            -32004 => ExtensionError::UnsupportedVersion(self.message),
            // -32005 is `ext/crashed` — the wire form carries the
            // tail in `message`; the structured `Crashed { name, … }`
            // variant is built host-side by the supervisor crate
            // (this code path is hit only when an extension echoes a
            // crash diagnostic over RPC, which is rare). Map to
            // Internal with the message preserved so callers still
            // see the diagnostic.
            -32005 => ExtensionError::Internal(self.message),
            -32006 => {
                // `ext-proto/handle-gone` — structured payload rides
                // in `data` as `{ "handle": "<id>", "cause": "<tag>" }`.
                // If `data` is absent or malformed, fall back to
                // Internal preserving the message so the caller still
                // sees the diagnostic.
                let data = self.data.as_ref().and_then(|v| v.as_object());
                let handle = data
                    .and_then(|o| o.get("handle"))
                    .and_then(|v| v.as_str())
                    .map(ark_view::HandleId::new);
                let cause = data
                    .and_then(|o| o.get("cause"))
                    .and_then(|v| serde_json::from_value::<ark_view::InvalidationCause>(v.clone()).ok());
                match (handle, cause) {
                    (Some(handle), Some(cause)) => {
                        ExtensionError::HandleGone { handle, cause }
                    }
                    _ => ExtensionError::Internal(self.message),
                }
            }
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

    /// Inject a oneshot waiter into the pending map for `id`. Used by
    /// the protocol conformance harness (T-9.5.9) when it hand-rolls a
    /// wire frame and needs to capture the response. Hidden from
    /// rustdoc — third-party callers should use the typed
    /// [`ExtensionClient`] surface.
    #[doc(hidden)]
    pub async fn test_inject_pending(&self, id: u64, tx: oneshot::Sender<Response>) {
        self.inner.pending.lock().await.insert(id, tx);
    }

    /// Push a raw NDJSON line onto the outgoing-message channel.
    /// Companion to [`NdjsonClient::test_inject_pending`] for the
    /// conformance harness; not for production use.
    #[doc(hidden)]
    pub fn test_push_raw(&self, body: String) {
        let _ = self.inner.tx.send(body);
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

    // -- Pane / Stack handle ops (Phase 2 R6) --------------------------------

    async fn pane_emit(
        &self,
        req: PaneEmitRequest,
        opts: RequestOptions,
    ) -> ExtResult<PaneEmitResponse> {
        self.call("pane/emit", req, opts).await
    }

    async fn pane_replace_view(
        &self,
        req: PaneReplaceViewRequest,
        opts: RequestOptions,
    ) -> ExtResult<PaneReplaceViewResponse> {
        self.call("pane/replace_view", req, opts).await
    }

    async fn pane_close(
        &self,
        req: PaneCloseRequest,
        opts: RequestOptions,
    ) -> ExtResult<PaneCloseResponse> {
        self.call("pane/close", req, opts).await
    }

    async fn stack_spawn_pane(
        &self,
        req: StackSpawnPaneRequest,
        opts: RequestOptions,
    ) -> ExtResult<StackSpawnPaneResponse> {
        self.call("stack/spawn_pane", req, opts).await
    }

    async fn stack_close_child(
        &self,
        req: StackCloseChildRequest,
        opts: RequestOptions,
    ) -> ExtResult<StackCloseChildResponse> {
        self.call("stack/close_child", req, opts).await
    }

    async fn stack_clear(
        &self,
        req: StackClearRequest,
        opts: RequestOptions,
    ) -> ExtResult<StackClearResponse> {
        self.call("stack/clear", req, opts).await
    }

    // -- Session lifecycle hooks (Phase 2 ext-surface R1) --------------------

    async fn on_session_start(
        &self,
        req: OnSessionStartRequest,
        opts: RequestOptions,
    ) -> ExtResult<OnSessionStartResponse> {
        self.call("on_session_start", req, opts).await
    }

    async fn on_session_end(
        &self,
        req: OnSessionEndRequest,
        opts: RequestOptions,
    ) -> ExtResult<OnSessionEndResponse> {
        self.call("on_session_end", req, opts).await
    }

    // -- Feature-group hooks (Phase 2 ext-surface R2) ------------------------

    async fn scene_compile_hook(
        &self,
        req: SceneCompileHookRequest,
        opts: RequestOptions,
    ) -> ExtResult<SceneCompileHookResponse> {
        self.call("scene_compile_hook", req, opts).await
    }

    async fn control_verbs(
        &self,
        req: ControlVerbsRequest,
        opts: RequestOptions,
    ) -> ExtResult<ControlVerbsResponse> {
        self.call("control_verbs", req, opts).await
    }

    async fn doctor_checks(
        &self,
        req: DoctorChecksRequest,
        opts: RequestOptions,
    ) -> ExtResult<DoctorChecksResponse> {
        self.call("doctor_checks", req, opts).await
    }

    async fn list_columns(
        &self,
        req: ListColumnsRequest,
        opts: RequestOptions,
    ) -> ExtResult<ListColumnsResponse> {
        self.call("list_columns", req, opts).await
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
    pub async fn serve<R, W, E>(reader: R, writer: W, ext: Arc<E>) -> io::Result<u64>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        E: ArkExtension + 'static,
    {
        Self::serve_inner(reader, writer, ext, None).await
    }

    /// Same as [`NdjsonServer::serve`] but every dispatched request is
    /// first gated through `gate` per R16: requests for `host/*` /
    /// `workspace/*` methods MUST present the session token (carried
    /// in `params._sessionToken`) and the corresponding dotted
    /// capability identifier MUST be in
    /// [`crate::Capabilities::granted`]. Failures surface as
    /// JSON-RPC error responses with code `-32001` (the
    /// `ext-proto/capability-denied` family); the typed
    /// [`crate::ExtensionError::CapabilityDenied`] flows through the
    /// existing `ResponseError::into_extension_error` path.
    ///
    /// The token field is stripped from `params` before the request
    /// is deserialized into the typed request struct, so extension
    /// code never sees the token leak into its payload.
    pub async fn serve_gated<R, W, E>(
        reader: R,
        writer: W,
        ext: Arc<E>,
        gate: ReverseRequestGate,
    ) -> io::Result<u64>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
        E: ArkExtension + 'static,
    {
        Self::serve_inner(reader, writer, ext, Some(gate)).await
    }

    async fn serve_inner<R, W, E>(
        reader: R,
        mut writer: W,
        ext: Arc<E>,
        gate: Option<ReverseRequestGate>,
    ) -> io::Result<u64>
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
            let resp = Self::dispatch(&ext, req, gate.as_ref()).await;
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
    /// and produce a wire-ready [`Response`]. When `gate` is `Some`,
    /// any `host/*` / `workspace/*` method is checked first and the
    /// `params._sessionToken` field is stripped before the typed
    /// request is built.
    async fn dispatch<E: ArkExtension>(
        ext: &Arc<E>,
        mut req: Request,
        gate: Option<&ReverseRequestGate>,
    ) -> Response {
        let id = req.id;

        // Capability + session-token gate per R16. Strip the token
        // before the typed request is deserialized so extension code
        // never sees it leak.
        if let Some(gate) = gate {
            if let Some(capability) = method_to_capability(&req.method) {
                let token = pop_session_token(&mut req.params);
                if let Err(e) = gate.check(&token, &capability) {
                    return error_response(id, &e);
                }
            }
        }

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
            "task/cancel" => dispatch_typed::<E, TaskCancelRequest, TaskCancelResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.task_cancel(r).await },
            )
            .await,
            "event/subscribe" => {
                dispatch_typed::<E, EventSubscribeRequest, EventSubscribeResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.event_subscribe(r).await },
                )
                .await
            }
            "event/unsubscribe" => {
                dispatch_typed::<E, EventUnsubscribeRequest, EventUnsubscribeResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.event_unsubscribe(r).await },
                )
                .await
            }
            "event/emit" => dispatch_typed::<E, EventEmitRequest, EventEmitResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.event_emit(r).await },
            )
            .await,
            "intent/unregister" => {
                dispatch_typed::<E, IntentUnregisterRequest, IntentUnregisterResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.intent_unregister(r).await },
                )
                .await
            }
            "intent/dispatch" => {
                dispatch_typed::<E, IntentDispatchRequest, IntentDispatchResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.intent_dispatch(r).await },
                )
                .await
            }
            "ui/keybind/register" => {
                dispatch_typed::<E, UiKeybindRegisterRequest, UiKeybindRegisterResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.ui_keybind_register(r).await },
                )
                .await
            }
            "ui/keybind/unregister" => {
                dispatch_typed::<
                    E,
                    UiKeybindUnregisterRequest,
                    UiKeybindUnregisterResponse,
                    _,
                >(ext, req.params, |e, r| async move {
                    e.ui_keybind_unregister(r).await
                })
                .await
            }
            "ui/pane/request" => {
                dispatch_typed::<E, UiPaneRequestRequest, UiPaneRequestResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.ui_pane_request(r).await },
                )
                .await
            }
            "ui/pane/close" => dispatch_typed::<E, UiPaneCloseRequest, UiPaneCloseResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.ui_pane_close(r).await },
            )
            .await,
            "pane/emit" => dispatch_typed::<E, PaneEmitRequest, PaneEmitResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.pane_emit(r).await },
            )
            .await,
            "pane/replace_view" => {
                dispatch_typed::<E, PaneReplaceViewRequest, PaneReplaceViewResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.pane_replace_view(r).await },
                )
                .await
            }
            "pane/close" => dispatch_typed::<E, PaneCloseRequest, PaneCloseResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.pane_close(r).await },
            )
            .await,
            "stack/spawn_pane" => {
                dispatch_typed::<E, StackSpawnPaneRequest, StackSpawnPaneResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.stack_spawn_pane(r).await },
                )
                .await
            }
            "stack/close_child" => {
                dispatch_typed::<E, StackCloseChildRequest, StackCloseChildResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.stack_close_child(r).await },
                )
                .await
            }
            "stack/clear" => dispatch_typed::<E, StackClearRequest, StackClearResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.stack_clear(r).await },
            )
            .await,
            "on_session_start" => {
                dispatch_typed::<E, OnSessionStartRequest, OnSessionStartResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.on_session_start(r).await },
                )
                .await
            }
            "on_session_end" => {
                dispatch_typed::<E, OnSessionEndRequest, OnSessionEndResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.on_session_end(r).await },
                )
                .await
            }
            "scene_compile_hook" => {
                dispatch_typed::<E, SceneCompileHookRequest, SceneCompileHookResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.scene_compile_hook(r).await },
                )
                .await
            }
            "control_verbs" => {
                dispatch_typed::<E, ControlVerbsRequest, ControlVerbsResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.control_verbs(r).await },
                )
                .await
            }
            "doctor_checks" => {
                dispatch_typed::<E, DoctorChecksRequest, DoctorChecksResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.doctor_checks(r).await },
                )
                .await
            }
            "list_columns" => {
                dispatch_typed::<E, ListColumnsRequest, ListColumnsResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.list_columns(r).await },
                )
                .await
            }
            "host/fs/read" => dispatch_typed::<E, HostFsReadRequest, HostFsReadResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.host_fs_read(r).await },
            )
            .await,
            "host/fs/write" => dispatch_typed::<E, HostFsWriteRequest, HostFsWriteResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.host_fs_write(r).await },
            )
            .await,
            "host/proc/spawn" => {
                dispatch_typed::<E, HostProcSpawnRequest, HostProcSpawnResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.host_proc_spawn(r).await },
                )
                .await
            }
            "host/net/fetch" => {
                dispatch_typed::<E, HostNetFetchRequest, HostNetFetchResponse, _>(
                    ext,
                    req.params,
                    |e, r| async move { e.host_net_fetch(r).await },
                )
                .await
            }
            "workspace/applyEdit" => dispatch_typed::<
                E,
                WorkspaceApplyEditRequest,
                WorkspaceApplyEditResponse,
                _,
            >(ext, req.params, |e, r| async move {
                e.workspace_apply_edit(r).await
            })
            .await,
            "workspace/configuration" => dispatch_typed::<
                E,
                WorkspaceConfigurationRequest,
                WorkspaceConfigurationResponse,
                _,
            >(
                ext, req.params, |e, r| async move { e.workspace_configuration(r).await }
            )
            .await,
            "workspace/showDocument" => dispatch_typed::<
                E,
                WorkspaceShowDocumentRequest,
                WorkspaceShowDocumentResponse,
                _,
            >(
                ext, req.params, |e, r| async move { e.workspace_show_document(r).await }
            )
            .await,
            "workspace/showMessageRequest" => dispatch_typed::<
                E,
                WorkspaceShowMessageRequestRequest,
                WorkspaceShowMessageRequestResponse,
                _,
            >(ext, req.params, |e, r| async move {
                e.workspace_show_message_request(r).await
            })
            .await,
            "scene/getRoot" => dispatch_typed::<E, SceneGetRootRequest, SceneGetRootResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.scene_get_root(r).await },
            )
            .await,
            "log/setLevel" => dispatch_typed::<E, LogSetLevelRequest, LogSetLevelResponse, _>(
                ext,
                req.params,
                |e, r| async move { e.log_set_level(r).await },
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
            Err(e) => error_response(id, &e),
        }
    }
}

/// Strip the `_sessionToken` field from a `params` JSON object, if
/// present, and return its string value (or empty string when
/// absent). Used by [`NdjsonServer::dispatch`]'s gate step before the
/// typed request is built.
fn pop_session_token(params: &mut Value) -> String {
    if let Value::Object(map) = params {
        if let Some(Value::String(s)) = map.remove("_sessionToken") {
            return s;
        }
    }
    String::new()
}

/// Build a JSON-RPC error response for any `ExtensionError`. The wire
/// `code` matches R12: `-32601` for `MethodNotFound`, `-32602` for
/// `InvalidParams`, `-32003` for `CapabilityDenied`, `-32004` for
/// `UnsupportedVersion`, `-32000` for everything else (catch-all
/// internal). The response's `data` field carries the
/// [`crate::ExtensionError::code`] string so consumers can match
/// against the `ext-proto/*` family without parsing `message`.
fn error_response(id: u64, e: &ExtensionError) -> Response {
    let code = match e {
        ExtensionError::MethodNotFound(_) => -32601,
        ExtensionError::InvalidParams(_) => -32602,
        ExtensionError::CapabilityDenied(_) => -32003,
        ExtensionError::UnsupportedVersion(_) => -32004,
        ExtensionError::Crashed { .. } => -32005,
        ExtensionError::HandleGone { .. } => -32006,
        ExtensionError::Internal(_) => -32000,
    };
    // HandleGone carries structured payload (handle + cause) the peer
    // needs to reconstruct the typed variant. Encode as an object with
    // the `code` string alongside the fields. Other variants keep the
    // plain-string `data` shape for back-compat.
    let data = match e {
        ExtensionError::HandleGone { handle, cause } => Some(serde_json::json!({
            "code": e.code(),
            "handle": handle,
            "cause": cause,
        })),
        _ => Some(Value::String(e.code().to_string())),
    };
    Response {
        jsonrpc: "2.0".into(),
        id: Some(id),
        result: None,
        error: Some(ResponseError {
            code,
            message: e.to_string(),
            data,
        }),
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

    #[test]
    fn handle_gone_wire_roundtrip_preserves_handle_and_cause() {
        use ark_view::{HandleId, InvalidationCause};
        let original = ExtensionError::HandleGone {
            handle: HandleId::new("abc-123"),
            cause: InvalidationCause::UserClosed,
        };
        let encoded = error_response(1, &original);
        let resp_err = encoded.error.expect("error present");
        assert_eq!(resp_err.code, -32006, "wire code for HandleGone");
        let decoded = resp_err.into_extension_error();
        match decoded {
            ExtensionError::HandleGone { handle, cause } => {
                assert_eq!(handle.as_str(), "abc-123");
                assert!(matches!(cause, InvalidationCause::UserClosed));
            }
            other => panic!("expected HandleGone, got {other:?}"),
        }
    }

    #[test]
    fn handle_gone_decoder_falls_back_to_internal_when_data_missing() {
        // Malformed payload (no structured fields) → Internal fallback.
        let malformed = ResponseError {
            code: -32006,
            message: "handle gone".into(),
            data: None,
        };
        match malformed.into_extension_error() {
            ExtensionError::Internal(msg) => assert_eq!(msg, "handle gone"),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

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

    /// Stub extension that implements `host_fs_read` so the gate test
    /// can verify the call lands when the capability matches.
    struct HostFsExt;
    #[async_trait]
    impl ArkExtension for HostFsExt {
        async fn host_fs_read(
            &self,
            req: HostFsReadRequest,
        ) -> ExtResult<HostFsReadResponse> {
            Ok(HostFsReadResponse {
                contents: format!("read:{}", req.path),
            })
        }
    }

    /// T-9.5.8: with the right capability + right session token, a
    /// `host/fs/read` reverse-request reaches the extension.
    #[tokio::test]
    async fn gated_dispatch_admits_authorized_host_call() {
        use crate::{Capabilities, SessionToken};

        let token = SessionToken::from_string("tok-grant");
        let caps = Capabilities::from_iter(["host.fs.read"]);
        let gate = ReverseRequestGate::new(token, caps);

        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let server = tokio::spawn(async move {
            NdjsonServer::serve_gated(server_r, server_w, Arc::new(HostFsExt), gate)
                .await
                .ok()
        });

        // Hand-roll the wire so we can attach `_sessionToken` to
        // params (the typed client API doesn't expose this — the
        // supervisor crate is the producer of these wire frames in
        // real usage).
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5u64,
            "method": "host/fs/read",
            "params": {
                "path": "/etc/hosts",
                "_sessionToken": "tok-grant",
            }
        })
        .to_string();
        let (resp_tx, resp_rx) = oneshot::channel();
        client.inner.pending.lock().await.insert(5, resp_tx);
        client.inner.tx.send(body).unwrap();

        let response = tokio::time::timeout(Duration::from_secs(2), resp_rx)
            .await
            .expect("recv timed out")
            .expect("oneshot dropped");
        assert!(response.error.is_none(), "unexpected: {:?}", response.error);
        let result = response.result.expect("result missing");
        assert_eq!(result["contents"], "read:/etc/hosts");

        client.shutdown_transport().await;
        let _ = server.await;
    }

    /// T-9.5.8: without the capability, the call is denied even when
    /// the token is valid.
    #[tokio::test]
    async fn gated_dispatch_denies_when_capability_missing() {
        use crate::{Capabilities, SessionToken};

        let token = SessionToken::from_string("tok-grant");
        // Extension only has `ui.keybind`, NOT `host.fs.read`.
        let caps = Capabilities::from_iter(["ui.keybind"]);
        let gate = ReverseRequestGate::new(token, caps);

        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let server = tokio::spawn(async move {
            NdjsonServer::serve_gated(server_r, server_w, Arc::new(HostFsExt), gate)
                .await
                .ok()
        });

        // Send a wire frame with the right token but unauthorized
        // method.
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7u64,
            "method": "host/fs/read",
            "params": {
                "path": "/etc/hosts",
                "_sessionToken": "tok-grant",
            }
        })
        .to_string();
        // Register a oneshot for id=7 so we can capture the error
        // response.
        let (resp_tx, resp_rx) = oneshot::channel();
        client.inner.pending.lock().await.insert(7, resp_tx);
        client.inner.tx.send(body).unwrap();

        let response = tokio::time::timeout(Duration::from_secs(2), resp_rx)
            .await
            .expect("recv timed out")
            .expect("oneshot dropped");
        let err = response.error.expect("expected error response");
        assert_eq!(err.code, -32003, "wire code should be capability-denied");
        assert!(
            err.message.contains("host.fs.read"),
            "message should name capability, got {}",
            err.message
        );
        // The structured `data` field carries the ext-proto/* code.
        assert_eq!(
            err.data,
            Some(Value::String("ext-proto/capability-denied".into()))
        );

        client.shutdown_transport().await;
        let _ = server.await;
    }

    /// T-9.5.8: the call is also denied when the token mismatches,
    /// even if the capability is granted.
    #[tokio::test]
    async fn gated_dispatch_denies_when_token_mismatches() {
        use crate::{Capabilities, SessionToken};

        let token = SessionToken::from_string("tok-correct");
        let caps = Capabilities::from_iter(["host.fs.read"]);
        let gate = ReverseRequestGate::new(token, caps);

        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let server = tokio::spawn(async move {
            NdjsonServer::serve_gated(server_r, server_w, Arc::new(HostFsExt), gate)
                .await
                .ok()
        });

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8u64,
            "method": "host/fs/read",
            "params": {
                "path": "/etc/hosts",
                "_sessionToken": "tok-WRONG",
            }
        })
        .to_string();
        let (resp_tx, resp_rx) = oneshot::channel();
        client.inner.pending.lock().await.insert(8, resp_tx);
        client.inner.tx.send(body).unwrap();

        let response = tokio::time::timeout(Duration::from_secs(2), resp_rx)
            .await
            .expect("recv timed out")
            .expect("oneshot dropped");
        let err = response.error.expect("expected error response");
        assert_eq!(err.code, -32003);
        assert!(
            err.message.contains("invalid session token"),
            "message should mention token mismatch, got {}",
            err.message
        );

        client.shutdown_transport().await;
        let _ = server.await;
    }

    /// Lifecycle methods are not gated — they need to run before the
    /// session token is even minted.
    #[tokio::test]
    async fn gated_dispatch_skips_check_for_lifecycle_methods() {
        use crate::{Capabilities, SessionToken};

        let token = SessionToken::from_string("tok");
        let gate = ReverseRequestGate::new(token, Capabilities::empty());

        let (client_io, server_io) = duplex(8192);
        let (client_r, client_w) = tokio::io::split(client_io);
        let (server_r, server_w) = tokio::io::split(server_io);

        let client = NdjsonClient::from_halves(client_r, client_w);
        let server = tokio::spawn(async move {
            NdjsonServer::serve_gated(server_r, server_w, Arc::new(EchoExt), gate)
                .await
                .ok()
        });

        // No `_sessionToken` on params — `ping` is a lifecycle
        // method so the gate skips it.
        let _resp: PingResponse = client
            .ping(PingRequest::default(), RequestOptions::default())
            .await
            .expect("ping should NOT be gated");

        client.shutdown_transport().await;
        let _ = server.await;
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
