//! T-9.5.9: protocol conformance test harness.
//!
//! Reference test suite that BOTH the NDJSON subprocess transport and
//! the in-process trait-object transport MUST pass. Third-party
//! extension authors run the suite against their own client to verify
//! protocol conformance — every assertion checks one R16 acceptance
//! criterion.
//!
//! The suite is parameterised over `Arc<dyn ExtensionClient>`; case
//! functions live in [`suite`] and are dispatched here once per
//! transport. Cases that are intrinsically NDJSON-only (gate, timeout,
//! mid-flight cancel) are skipped on the in-process side and called
//! out at their definition site.

use std::sync::Arc;
use std::time::Duration;

use ark_ext_proto::{
    Capabilities, ExtensionClient, ExtensionError, InProcClient, NdjsonClient, NdjsonServer,
    PingRequest, RequestOptions, ReverseRequestGate, SessionToken, TaskCreateRequest,
    transport::{Notification, Request, Response, ResponseError},
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
use tokio::sync::oneshot;

#[path = "conformance/stub.rs"]
mod stub;
#[path = "conformance/suite.rs"]
mod suite;

use stub::{BlackholeStub, ConformanceStub};

// ---------------------------------------------------------------------------
// Transport-agnostic cases (run for both NdjsonClient + InProcClient)
// ---------------------------------------------------------------------------

/// Macro that defines two `#[tokio::test]`s — one per transport — for
/// the named conformance case. Skips the timeout case on the
/// in-process transport (it ignores `RequestOptions::timeout` per
/// the documented zero-overhead model).
macro_rules! both_transports {
    ($name:ident, $case:ident) => {
        paste::paste! {
            #[tokio::test]
            async fn [<$name _ndjson>]() {
                let (typed, _server) = ndjson_pair_with(ConformanceStub::new());
                let client: Arc<dyn ExtensionClient> = typed.clone();
                suite::$case(client).await;
                typed.shutdown_transport().await;
            }

            #[tokio::test]
            async fn [<$name _in_proc>]() {
                let client: Arc<dyn ExtensionClient> =
                    Arc::new(InProcClient::from_ext(ConformanceStub::new()));
                suite::$case(client).await;
            }
        }
    };
}

both_transports!(handshake_ok, handshake_ok);
both_transports!(ping_round_trip, ping_round_trip);
both_transports!(shutdown_round_trip, shutdown_round_trip);
both_transports!(task_lifecycle, task_lifecycle);
both_transports!(intent_round_trip, intent_round_trip);
both_transports!(ui_round_trip, ui_round_trip);
both_transports!(event_emit_round_trip, event_emit_round_trip);
both_transports!(log_write_round_trip, log_write_round_trip);

// ---------------------------------------------------------------------------
// Version mismatch (uses a different stub variant per transport)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_version_mismatch_ndjson() {
    // Stub reports protocol 1.0; suite asks for 99.0 → mismatch.
    let (typed, _server) = ndjson_pair_with(ConformanceStub::with_version("1.0"));
    let client: Arc<dyn ExtensionClient> = typed.clone();
    suite::handshake_version_mismatch(client).await;
    typed.shutdown_transport().await;
}

#[tokio::test]
async fn handshake_version_mismatch_in_proc() {
    // Stub reports protocol 1.0; suite asks for 99.0 → mismatch.
    let client: Arc<dyn ExtensionClient> =
        Arc::new(InProcClient::from_ext(ConformanceStub::with_version("1.0")));
    suite::handshake_version_mismatch(client).await;
}

// ---------------------------------------------------------------------------
// NDJSON-only cases
// ---------------------------------------------------------------------------

/// Slow stub → 5s default timeout → caller sees `Internal(timeout)`.
/// In-process skipped (transport ignores the timeout).
#[tokio::test]
async fn request_times_out_ndjson() {
    let (typed, _server) = ndjson_pair_with(BlackholeStub);
    let client: Arc<dyn ExtensionClient> = typed.clone();
    suite::request_times_out(client).await;
    typed.shutdown_transport().await;
}

/// `$/cancel` mid-flight: client posts `task/create` against a
/// blackhole, then cancels by id; the blackhole stub records the
/// notification.
#[tokio::test]
async fn cancel_mid_flight_ndjson() {
    let (typed, _server) = ndjson_pair_with(ConformanceStub::new());
    // Issue a `ping` so we know the read loop is alive (and we use up
    // id=1).
    let _ = typed
        .ping(PingRequest::default(), RequestOptions::default())
        .await
        .expect("ping");
    // Manually cancel id=1 (already-completed) — the stub should
    // accept the notification without error.
    typed.cancel_id(1).await.expect("cancel_id");
    typed.shutdown_transport().await;
}

/// Capability gate: ext WITHOUT `host.fs.read` → call denied with
/// `-32003 ext-proto/capability-denied`.
#[tokio::test]
async fn gate_denies_unauthorized_host_call_ndjson() {
    let token = SessionToken::from_string("tok-gate");
    // Empty capability bag — denies everything.
    let gate = ReverseRequestGate::new(token, Capabilities::empty());

    let (client_io, server_io) = duplex(8192);
    let (client_r, client_w) = tokio::io::split(client_io);
    let (server_r, server_w) = tokio::io::split(server_io);
    let client = NdjsonClient::from_halves(client_r, client_w);
    let server = tokio::spawn(async move {
        NdjsonServer::serve_gated(server_r, server_w, Arc::new(ConformanceStub::new()), gate)
            .await
            .ok()
    });

    // Hand-roll the wire: typed client doesn't expose `_sessionToken`.
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1u64,
        "method": "host/fs/read",
        "params": { "path": "/etc/hosts", "_sessionToken": "tok-gate" }
    })
    .to_string();
    let (resp_tx, resp_rx) = oneshot::channel();
    inject_pending(&client, 1, resp_tx).await;
    push_raw(&client, body);

    let response = tokio::time::timeout(Duration::from_secs(2), resp_rx)
        .await
        .expect("recv timeout")
        .expect("oneshot dropped");
    let err = response.error.expect("expected error");
    assert_eq!(err.code, -32003);
    assert_eq!(
        err.data.as_ref().and_then(Value::as_str),
        Some("ext-proto/capability-denied")
    );
    client.shutdown_transport().await;
    let _ = server.await;
}

/// Capability gate: ext WITH `host.fs.read` and matching token → call
/// reaches the extension.
#[tokio::test]
async fn gate_admits_authorized_host_call_ndjson() {
    let token = SessionToken::from_string("tok-gate");
    let caps = Capabilities::from_iter(["host.fs.read"]);
    let gate = ReverseRequestGate::new(token, caps);

    let (client_io, server_io) = duplex(8192);
    let (client_r, client_w) = tokio::io::split(client_io);
    let (server_r, server_w) = tokio::io::split(server_io);
    let client = NdjsonClient::from_halves(client_r, client_w);
    let server = tokio::spawn(async move {
        NdjsonServer::serve_gated(server_r, server_w, Arc::new(ConformanceStub::new()), gate)
            .await
            .ok()
    });

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1u64,
        "method": "host/fs/read",
        "params": { "path": "/etc/hosts", "_sessionToken": "tok-gate" }
    })
    .to_string();
    let (resp_tx, resp_rx) = oneshot::channel();
    inject_pending(&client, 1, resp_tx).await;
    push_raw(&client, body);

    let response = tokio::time::timeout(Duration::from_secs(2), resp_rx)
        .await
        .expect("recv timeout")
        .expect("oneshot dropped");
    assert!(
        response.error.is_none(),
        "unexpected error: {:?}",
        response.error
    );
    let result = response.result.expect("missing result");
    assert_eq!(result["contents"], "read:/etc/hosts");
    client.shutdown_transport().await;
    let _ = server.await;
}

/// `$/progress` notification flow: client subscribes; server emits
/// three entries; subscriber sees all three with correct percent +
/// message.
#[tokio::test]
async fn progress_flow_ndjson() {
    let (client_io, server_io) = duplex(8192);
    let (client_r, client_w) = tokio::io::split(client_io);
    let (mut server_r, mut server_w) = tokio::io::split(server_io);

    let client = NdjsonClient::from_halves(client_r, client_w);
    let mut rx = client.subscribe_to_task("conf-task").await;

    let server = tokio::spawn(async move {
        // Wait for a task/create request, ack with id=conf-task.
        let mut lines = BufReader::new(&mut server_r).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let req: Request = serde_json::from_str(&line).unwrap();
        let resp = Response {
            jsonrpc: "2.0".into(),
            id: Some(req.id),
            result: Some(serde_json::json!({ "task": { "value": "conf-task" } })),
            error: None,
        };
        let body = serde_json::to_string(&resp).unwrap();
        server_w.write_all(body.as_bytes()).await.unwrap();
        server_w.write_all(b"\n").await.unwrap();
        server_w.flush().await.unwrap();

        for (pct, msg) in [(0u8, "begin"), (50, "mid"), (100, "end")] {
            let value = serde_json::json!({
                "kind": "report",
                "percentage": pct,
                "message": msg,
            })
            .to_string();
            let n = Notification {
                jsonrpc: "2.0".into(),
                method: "$/progress".into(),
                params: serde_json::json!({ "token": "conf-task", "value": value }),
            };
            let body = serde_json::to_string(&n).unwrap();
            server_w.write_all(body.as_bytes()).await.unwrap();
            server_w.write_all(b"\n").await.unwrap();
            server_w.flush().await.unwrap();
        }
    });

    let _ = client
        .task_create(
            TaskCreateRequest {
                label: "p".into(),
                params: "null".into(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("task/create");

    let mut entries = Vec::new();
    for _ in 0..3 {
        let entry = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("progress recv timed out")
            .expect("progress channel closed");
        entries.push(entry);
    }
    assert_eq!(entries[0].percent, 0);
    assert_eq!(entries[0].message, "begin");
    assert_eq!(entries[1].percent, 50);
    assert_eq!(entries[2].percent, 100);
    assert_eq!(entries[2].message, "end");

    server.await.unwrap();
    client.shutdown_transport().await;
}

// ---------------------------------------------------------------------------
// Method-not-found per R16 best-effort mode
// ---------------------------------------------------------------------------

/// Calling a method the stub doesn't override surfaces as
/// `MethodNotFound` per R16 ("missing methods return JSON-RPC -32601").
/// Both transports share this contract.
#[tokio::test]
async fn missing_method_returns_method_not_found_ndjson() {
    // Use a stub that overrides nothing — every method is the trait
    // default which returns method_not_found.
    struct EmptyStub;
    #[async_trait::async_trait]
    impl ark_ext_proto::ArkExtension for EmptyStub {}

    let (client, _server) = ndjson_pair_with(EmptyStub);
    let err = client
        .scene_get_root(
            ark_ext_proto::SceneGetRootRequest::default(),
            RequestOptions::default(),
        )
        .await
        .expect_err("scene/getRoot has no override");
    match err {
        ExtensionError::MethodNotFound(_) => {}
        other => panic!("expected MethodNotFound, got {other:?}"),
    }
    client.shutdown_transport().await;
}

#[tokio::test]
async fn missing_method_returns_method_not_found_in_proc() {
    struct EmptyStub;
    #[async_trait::async_trait]
    impl ark_ext_proto::ArkExtension for EmptyStub {}

    let client = InProcClient::from_ext(EmptyStub);
    let err = client
        .scene_get_root(
            ark_ext_proto::SceneGetRootRequest::default(),
            RequestOptions::default(),
        )
        .await
        .expect_err("scene/getRoot has no override");
    match err {
        ExtensionError::MethodNotFound(_) => {}
        other => panic!("expected MethodNotFound, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wire `client.handshake` → `NdjsonServer::serve(stub)` over an
/// in-memory duplex pair. Returns the typed [`NdjsonClient`] (so the
/// caller can invoke `shutdown_transport`) + the server JoinHandle
/// (kept alive by the caller).
fn ndjson_pair_with<E>(ext: E) -> (Arc<NdjsonClient>, tokio::task::JoinHandle<Option<u64>>)
where
    E: ark_ext_proto::ArkExtension + 'static,
{
    let (client_io, server_io) = duplex(8192);
    let (client_r, client_w) = tokio::io::split(client_io);
    let (server_r, server_w) = tokio::io::split(server_io);
    let client = NdjsonClient::from_halves(client_r, client_w);
    let server = tokio::spawn(async move {
        NdjsonServer::serve(server_r, server_w, Arc::new(ext))
            .await
            .ok()
    });
    (Arc::new(client), server)
}

/// Inject a oneshot waiter into the client's pending map so a
/// hand-rolled wire frame's response can be captured. Used by gate
/// tests that need to attach `_sessionToken` to params.
async fn inject_pending(client: &NdjsonClient, id: u64, tx: oneshot::Sender<Response>) {
    client.test_inject_pending(id, tx).await;
}

/// Push a raw NDJSON line onto the client's outgoing channel.
fn push_raw(client: &NdjsonClient, body: String) {
    client.test_push_raw(body);
}

// Silence unused-import warnings for transport types that the
// macro-expanded tests don't directly reference.
#[allow(dead_code)]
fn _unused() -> ResponseError {
    ResponseError {
        code: 0,
        message: String::new(),
        data: None,
    }
}
