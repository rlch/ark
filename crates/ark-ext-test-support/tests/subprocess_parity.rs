//! T-038 (cavekit-soul-phase-2-tests.md R2): parity round-trip between
//! in-process [`StubExtension`] and the `ark-stub-ext` subprocess over
//! NDJSON.
//!
//! Both legs are configured identically (same capability set, same
//! manifest, same per-method handler registrations). The in-process
//! leg calls the trait methods directly; the subprocess leg encodes
//! each call as a JSON-RPC request on the child's stdin and parses the
//! response line off its stdout. Each step asserts the two legs
//! observe the same success/error class AND the same serialized
//! result payload.
//!
//! The child is shut down by closing stdin (EOF); the bin's
//! [`NdjsonServer::serve`] loop returns and the process exits `0`.
//! `child.wait()` verifies that exit code so a hung or crashing bin
//! fails the test deterministically.

use std::process::Stdio;

use ark_ext_metadata_types::{ExtensionMetadata, IntentDecl, StringNode, ViewDecl};
use ark_ext_proto::{
    ArkExtension, OnSessionEndRequest, OnSessionEndResponse, PaneCloseRequest, PaneCloseResponse,
    PaneEmitRequest, PaneEmitResponse,
};
use ark_ext_test_support::StubExtension;
use ark_view::HandleId;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Name of the bin under test. Cargo SOMETIMES sets
/// `CARGO_BIN_EXE_<name>` for `[[bin]]` targets in the same crate as
/// integration tests — but not in every combination of
/// `cargo test`/`--test` selectors we hit in practice. Fall back to
/// resolving `target/<profile>/ark-stub-ext` relative to the
/// integration-test executable if the env var is missing.
const BIN_ENV: &str = "CARGO_BIN_EXE_ark-stub-ext";
const BIN_NAME: &str = if cfg!(windows) {
    "ark-stub-ext.exe"
} else {
    "ark-stub-ext"
};

/// Resolve the path to the built `ark-stub-ext` binary.
///
/// Cargo emits the integration-test binary into
/// `<target>/<profile>/deps/<test-name>-<hash>`; the bin target lives
/// at `<target>/<profile>/<bin-name>`. Walking up one directory from
/// `current_exe()` gets the profile dir without hard-coding `debug` /
/// `release`.
fn locate_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var(BIN_ENV) {
        return std::path::PathBuf::from(p);
    }
    let test_exe = std::env::current_exe().expect("current_exe");
    // current_exe = .../target/<profile>/deps/<test>-<hash>
    let profile_dir = test_exe
        .parent()
        .and_then(|p| p.parent())
        .expect("integration test exe should live two levels below target/<profile>");
    let candidate = profile_dir.join(BIN_NAME);
    assert!(
        candidate.is_file(),
        "expected ark-stub-ext at {candidate:?}; cargo should have built the bin \
         via `cargo test -p ark-ext-test-support` or an explicit \
         `cargo build -p ark-ext-test-support --bin ark-stub-ext`",
    );
    candidate
}

/// Build an in-process [`StubExtension`] wired with the same axes the
/// subprocess leg reads from env vars. Keeps the two leg-configs in
/// lockstep — any future change MUST touch both call sites or parity
/// breaks loudly.
fn build_in_process_stub() -> StubExtension {
    let mut manifest = ExtensionMetadata {
        name: StringNode::new("ark-stub-ext"),
        version: StringNode::new("1.0.0"),
        ark_range: StringNode::new(">=0.1"),
        zellij_range: StringNode::new(""),
        requires: vec![],
        intents: vec![],
        events: vec![],
        config: Default::default(),
        views: vec![],
        capabilities: Default::default(),
        config_sections: vec![],
        reload_gates: vec![],
    };
    manifest.intents.push(IntentDecl {
        name: "stub.hello".into(),
        args_schema: StringNode::new(""),
    });
    manifest.views.push(ViewDecl {
        name: "EditorView".into(),
        component: StringNode::new(""),
        kind: Some(StringNode::new("pane")),
    });

    StubExtension::builder()
        .advertise_capabilities(["view.pane.v1", "ext.lifecycle.v1"])
        .with_manifest(manifest)
        .with_method("pane/emit", |_req: PaneEmitRequest| {
            Ok(PaneEmitResponse::default())
        })
        .with_method("pane/close", |_req: PaneCloseRequest| {
            Ok(PaneCloseResponse::default())
        })
        .with_method("on_session_end", |_req: OnSessionEndRequest| {
            Ok(OnSessionEndResponse::default())
        })
        .build()
}

/// Spawn the subprocess configured identically to the in-process stub.
/// Returns the child handle plus piped stdin/stdout halves.
fn spawn_subprocess() -> (
    tokio::process::Child,
    tokio::process::ChildStdin,
    BufReader<tokio::process::ChildStdout>,
) {
    let path = locate_bin();
    let mut cmd = Command::new(path);
    cmd.env("ARK_STUB_VERSION", "1.0.0")
        .env("ARK_STUB_CAPABILITIES", "view.pane.v1,ext.lifecycle.v1")
        .env("ARK_STUB_INTENTS", "stub.hello")
        .env("ARK_STUB_VIEW_TYPES", "EditorView:pane")
        .env("ARK_STUB_METHODS", "pane/emit,pane/close,on_session_end")
        // Keep the "opt-out" axis exercised — an empty list still
        // parses to `Vec::new()` via `parse_csv`.
        .env("ARK_STUB_METHOD_NOT_FOUND_LIST", "");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().expect("spawn ark-stub-ext");
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    (child, stdin, stdout)
}

/// Send one JSON-RPC request line to the child and read exactly one
/// response line back. The bin's dispatcher writes one response per
/// request and flushes, so the read is guaranteed to terminate as
/// soon as the response arrives.
async fn rpc(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let mut line = serde_json::to_string(&req).expect("serialize request");
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.expect("write stdin");
    stdin.flush().await.expect("flush stdin");

    let mut resp_line = String::new();
    let n = stdout
        .read_line(&mut resp_line)
        .await
        .expect("read response");
    assert!(n > 0, "unexpected EOF on child stdout before response");
    serde_json::from_str(&resp_line).expect("parse response JSON")
}

/// Classify a JSON-RPC response into (success?, result|error-code,
/// payload). Used by the parity comparator so the in-process leg can
/// project its typed result into the same shape the wire produces.
fn classify_wire(resp: &Value) -> (bool, Option<Value>, Option<i32>) {
    if let Some(result) = resp.get("result") {
        (true, Some(result.clone()), None)
    } else if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(Value::as_i64).map(|c| c as i32);
        (false, None, code)
    } else {
        panic!("response has neither `result` nor `error`: {resp}");
    }
}

#[tokio::test]
async fn in_process_and_subprocess_agree_on_surface() {
    let stub = build_in_process_stub();
    let (mut child, mut stdin, mut stdout) = spawn_subprocess();

    // ------------------------------------------------------------------
    // Step 1 — implemented method: `pane/emit`. Both legs return
    // `PaneEmitResponse::default()` which serializes to an empty
    // object. Parity: subprocess `result` == in-process typed response
    // serialized to JSON.
    // ------------------------------------------------------------------
    let emit_req = PaneEmitRequest {
        handle: HandleId::new("h-1"),
        kind: "stub.evt".into(),
        payload: "{}".to_string(),
    };
    let in_proc_emit = stub
        .pane_emit(emit_req.clone())
        .await
        .expect("in-process pane/emit ok");
    let in_proc_emit_json =
        serde_json::to_value(&in_proc_emit).expect("serialize in-proc emit response");

    let wire_emit = rpc(
        &mut stdin,
        &mut stdout,
        1,
        "pane/emit",
        serde_json::to_value(&emit_req).unwrap(),
    )
    .await;
    let (ok, result, _code) = classify_wire(&wire_emit);
    assert!(ok, "subprocess pane/emit should succeed, got {wire_emit}");
    assert_eq!(
        result.unwrap(),
        in_proc_emit_json,
        "pane/emit response payload mismatch between in-process and subprocess stubs",
    );

    // ------------------------------------------------------------------
    // Step 2 — implemented method: `pane/close`. Same parity shape.
    // ------------------------------------------------------------------
    let close_req = PaneCloseRequest {
        handle: HandleId::new("h-1"),
    };
    let in_proc_close = stub
        .pane_close(close_req.clone())
        .await
        .expect("in-process pane/close ok");
    let in_proc_close_json =
        serde_json::to_value(&in_proc_close).expect("serialize in-proc close response");
    let wire_close = rpc(
        &mut stdin,
        &mut stdout,
        2,
        "pane/close",
        serde_json::to_value(&close_req).unwrap(),
    )
    .await;
    let (ok, result, _code) = classify_wire(&wire_close);
    assert!(ok, "subprocess pane/close should succeed, got {wire_close}");
    assert_eq!(result.unwrap(), in_proc_close_json);

    // ------------------------------------------------------------------
    // Step 3 — implemented method: `on_session_end`. Covers the
    // lifecycle-hook feature group.
    // ------------------------------------------------------------------
    let end_req = OnSessionEndRequest {
        spec: "null".into(),
        exit: "{\"kind\":\"normal\"}".into(),
    };
    let in_proc_end = stub
        .on_session_end(end_req.clone())
        .await
        .expect("in-process on_session_end ok");
    let in_proc_end_json =
        serde_json::to_value(&in_proc_end).expect("serialize in-proc session-end response");
    let wire_end = rpc(
        &mut stdin,
        &mut stdout,
        3,
        "on_session_end",
        serde_json::to_value(&end_req).unwrap(),
    )
    .await;
    let (ok, result, _code) = classify_wire(&wire_end);
    assert!(
        ok,
        "subprocess on_session_end should succeed, got {wire_end}"
    );
    assert_eq!(result.unwrap(), in_proc_end_json);

    // ------------------------------------------------------------------
    // Step 4 — UNimplemented method on BOTH sides: `stack/clear`.
    // Parity requires both legs to surface `method_not_found`
    // (JSON-RPC `-32601`) because the stub has no handler registered
    // on either side. Exercises the trait-default fall-through path.
    // ------------------------------------------------------------------
    use ark_ext_proto::{ExtensionError, StackClearRequest};
    let clear_req = StackClearRequest {
        stack: HandleId::new("s-1"),
    };
    let in_proc_clear = stub.stack_clear(clear_req.clone()).await;
    assert!(
        matches!(in_proc_clear, Err(ExtensionError::MethodNotFound(_))),
        "in-process stack/clear should be method_not_found, got {in_proc_clear:?}"
    );
    let wire_clear = rpc(
        &mut stdin,
        &mut stdout,
        4,
        "stack/clear",
        serde_json::to_value(&clear_req).unwrap(),
    )
    .await;
    let (ok, _result, code) = classify_wire(&wire_clear);
    assert!(!ok, "subprocess stack/clear should error, got {wire_clear}");
    assert_eq!(
        code,
        Some(-32601),
        "subprocess stack/clear should surface JSON-RPC method_not_found (-32601)",
    );

    // ------------------------------------------------------------------
    // Clean shutdown: drop stdin → child sees EOF → NdjsonServer
    // returns → process exits 0. `wait()` is the deterministic gate
    // against a hung or crashing bin.
    // ------------------------------------------------------------------
    drop(stdin);
    let status = child.wait().await.expect("wait child");
    assert!(
        status.success(),
        "ark-stub-ext should exit 0 on EOF, got {status:?}"
    );
}
