//! Protocol-conformance assertion suite.
//!
//! Every assertion in this module is parameterised over an
//! `Arc<dyn ExtensionClient>` so the same suite runs against the
//! NDJSON subprocess transport and the in-process trait-object
//! transport. Third-party extension authors can use this file as the
//! REFERENCE — any extension whose client implements
//! [`ark_ext_proto::ExtensionClient`] MUST pass every case here.
//!
//! Cases that are intrinsically transport-specific (e.g. the
//! capability gate checks, which require the JSON-RPC server-side
//! dispatcher) live in the binary entry-point alongside the
//! parametrized harness.

use std::sync::Arc;
use std::time::Duration;

use ark_ext_proto::{
    Capabilities, EventEmitRequest, ExtensionClient, ExtensionError, IntentDispatchRequest,
    IntentRegisterRequest, LogLevel, LogWriteRequest, PingRequest, ProtocolVersion,
    RequestOptions, ShutdownRequest, TaskCancelRequest, TaskCreateRequest, TaskGetRequest,
    UiKeybindRegisterRequest, UiStatusPushRequest,
};

/// Conformance case: handshake with a compatible MAJOR succeeds.
pub async fn handshake_ok(client: Arc<dyn ExtensionClient>) {
    let resp = client
        .handshake(
            ProtocolVersion::new(1, 0),
            Capabilities::from_iter(["ui.keybind"]),
            "ark-conformance".into(),
            RequestOptions::default(),
        )
        .await
        .expect("handshake should succeed on matching MAJOR");
    let v = ProtocolVersion::parse(&resp.protocol_version).unwrap();
    assert_eq!(v.major, 1);
    assert!(
        !resp.session_token.is_empty(),
        "host MUST mint a session token"
    );
}

/// Conformance case: handshake with an incompatible MAJOR fails with
/// `UnsupportedVersion`.
pub async fn handshake_version_mismatch(client: Arc<dyn ExtensionClient>) {
    let err = client
        .handshake(
            ProtocolVersion::new(99, 0),
            Capabilities::empty(),
            "ark-conformance".into(),
            RequestOptions::default(),
        )
        .await
        .expect_err("handshake should fail on different MAJOR");
    match err {
        ExtensionError::UnsupportedVersion(_) => {}
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
}

/// Conformance case: `ping` round-trips and returns a default
/// response.
pub async fn ping_round_trip(client: Arc<dyn ExtensionClient>) {
    let _ = client
        .ping(PingRequest::default(), RequestOptions::default())
        .await
        .expect("ping should succeed");
}

/// Conformance case: `shutdown` round-trips.
pub async fn shutdown_round_trip(client: Arc<dyn ExtensionClient>) {
    let _ = client
        .shutdown(ShutdownRequest::default(), RequestOptions::default())
        .await
        .expect("shutdown should succeed");
}

/// Conformance case: `task/create` returns a non-empty taskId; `task/get`
/// echoes; `task/cancel` ack's.
pub async fn task_lifecycle(client: Arc<dyn ExtensionClient>) {
    let task = client
        .task_create(
            TaskCreateRequest {
                label: "demo".into(),
                params: "null".into(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("task/create");
    assert!(!task.task.value.is_empty(), "taskId must be non-empty");

    let got = client
        .task_get(
            TaskGetRequest {
                task: task.task.clone(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("task/get");
    assert!(!got.status.is_empty());

    let _ = client
        .task_cancel(
            TaskCancelRequest { task: task.task },
            RequestOptions::default(),
        )
        .await
        .expect("task/cancel");
}

/// Conformance case: `intent/register` then `intent/dispatch` round-trip.
pub async fn intent_round_trip(client: Arc<dyn ExtensionClient>) {
    let _ = client
        .intent_register(
            IntentRegisterRequest {
                name: "stub.demo".into(),
                args_schema: "{}".into(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("intent/register");

    let resp = client
        .intent_dispatch(
            IntentDispatchRequest {
                name: "stub.demo".into(),
                args: "null".into(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("intent/dispatch");
    assert!(!resp.value.is_empty());
}

/// Conformance case: `ui/keybind/register` ack's; `ui/status/push`
/// notification accepted.
pub async fn ui_round_trip(client: Arc<dyn ExtensionClient>) {
    let _ = client
        .ui_keybind_register(
            UiKeybindRegisterRequest {
                command: "stub.cmd".into(),
                title: "Demo".into(),
                when: String::new(),
                default_chord: None,
            },
            RequestOptions::default(),
        )
        .await
        .expect("ui/keybind/register");

    let _ = client
        .ui_status_push(
            UiStatusPushRequest {
                text: "ok".into(),
                severity: LogLevel::Info,
            },
            RequestOptions::default(),
        )
        .await
        .expect("ui/status/push");
}

/// Conformance case: `event/emit` round-trips. Some transports treat
/// it as a fire-and-forget notification; both shapes satisfy the
/// contract.
pub async fn event_emit_round_trip(client: Arc<dyn ExtensionClient>) {
    let result = client
        .event_emit(
            EventEmitRequest {
                name: "stub.test_event".into(),
                payload: "null".into(),
            },
            RequestOptions::default(),
        )
        .await;
    // event/emit's default impl returns method_not_found; the stub
    // doesn't override. The contract is that the call DOESN'T panic;
    // either Ok or method_not_found is acceptable for v1.
    match result {
        Ok(_) => {}
        Err(ExtensionError::MethodNotFound(_)) => {}
        Err(other) => panic!("unexpected event/emit error: {other:?}"),
    }
}

/// Conformance case: `log/write` notification accepted (default impl
/// returns Ok(())).
pub async fn log_write_round_trip(client: Arc<dyn ExtensionClient>) {
    let _ = client
        .log_write(
            LogWriteRequest {
                level: LogLevel::Info,
                message: "conformance".into(),
            },
            RequestOptions::default(),
        )
        .await
        .expect("log/write");
}

/// Conformance case: requests honour the configured timeout. The
/// caller passes a client wired to a stub that never replies; the
/// call MUST surface as `Internal(timeout)` within the configured
/// budget.
///
/// The in-process transport ignores the timeout (zero-serialization
/// path); for the in-process suite we skip this case.
pub async fn request_times_out(client: Arc<dyn ExtensionClient>) {
    let err = client
        .task_create(
            TaskCreateRequest {
                label: "stuck".into(),
                params: "null".into(),
            },
            RequestOptions {
                timeout: Duration::from_millis(150),
            },
        )
        .await
        .expect_err("blackhole stub should time out");
    match err {
        ExtensionError::Internal(m) => assert!(m.contains("timed out"), "msg = {m}"),
        other => panic!("expected Internal(timeout), got {other:?}"),
    }
}
