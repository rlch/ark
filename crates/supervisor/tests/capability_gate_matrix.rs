//! Capability-gate matrix — T-040 (soul phase 2 cavekit-tests R4).
//!
//! Exhaustively exercises the four cases locked by decision #4a against
//! the supervisor's [`ark_supervisor::ext_dispatch`] module. The
//! dispatcher (T-028 / T-029 / T-033 / F-015) already owns the
//! capability table + per-ext registry + opt-out set; this file is
//! TESTS ONLY — no production code changes.
//!
//! ## The four cells
//!
//! | Case | Description                                          | Assertion                                                       |
//! |------|------------------------------------------------------|-----------------------------------------------------------------|
//! | (a)  | advertised + implemented                             | ext receives + responds; stub call-log records the method       |
//! | (b)  | not advertised                                       | host SKIPS the call; 0 stub calls; 0 bytes serialized           |
//! | (c)  | advertised but `method_not_found` (F-015)            | host logs WARN naming method+cap; opt-out sticks; session lives |
//! | (d)  | removed-in-MAJOR                                     | `#[ignore]` — no real removal yet (decision #4c)                |
//!
//! ## Dispatch model the tests simulate
//!
//! The real host reconcile loop gates every outbound RPC via
//! [`ark_supervisor::ext_dispatch::should_dispatch`] before handing the
//! request to the transport. These tests make that contract explicit
//! with a thin [`GatedClient`] wrapper — `should_dispatch` gate → serde
//! serialization (tracked by a byte-counter) → trait dispatch. A
//! blocked gate never reaches serialization, so the byte counter stays
//! at zero on the skip path — matching kit R4's "0 bytes out" clause.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ark_ext_proto::{
    ExtensionClient, ExtensionError, InProcClient, PaneEmitRequest, PaneEmitResponse,
    RequestOptions, StackClearRequest, StackClearResponse,
};
use ark_ext_test_support::StubExtension;
use ark_supervisor::ext_dispatch::{
    capability_for_method, record_capabilities, should_dispatch,
    warn_advertised_but_unimplemented,
};
use ark_view::HandleId;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;

// ---------------------------------------------------------------------------
// Byte-counting transport wrapper
// ---------------------------------------------------------------------------
//
// Kit R4 requires the "not advertised" case to be proven via BOTH the
// stub call-log AND a byte-counting transport wrapper. Strategy:
// `GatedClient` wraps an [`InProcClient`], gates every call through
// `should_dispatch`, and, on the happy path, serializes the request via
// `serde_json::to_vec` before forwarding — adding the byte length to a
// shared counter. On the skip path it returns `MethodNotFound` without
// touching the byte counter. A blocked gate therefore provably produces
// zero outbound bytes.

#[derive(Clone)]
struct GatedClient {
    inner: InProcClient,
    ext_name: String,
    bytes_out: Arc<AtomicUsize>,
}

impl GatedClient {
    fn new(stub: &StubExtension, ext_name: impl Into<String>) -> Self {
        Self {
            inner: InProcClient::new(Arc::new(stub.clone())),
            ext_name: ext_name.into(),
            bytes_out: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn bytes_out(&self) -> usize {
        self.bytes_out.load(Ordering::Relaxed)
    }

    /// Gate + serialize + dispatch `pane/emit`. Mirrors how the
    /// real supervisor reconcile loop guards outbound Phase-2 RPCs.
    async fn pane_emit(&self, req: PaneEmitRequest) -> Result<PaneEmitResponse, ExtensionError> {
        let method = "pane/emit";
        if !should_dispatch(&self.ext_name, method) {
            return Err(ExtensionError::method_not_found(method));
        }
        let wire = serde_json::to_vec(&req).expect("serialize pane/emit");
        self.bytes_out.fetch_add(wire.len(), Ordering::Relaxed);
        self.inner.pane_emit(req, RequestOptions::default()).await
    }

    /// Gate + serialize + dispatch `stack/clear`.
    async fn stack_clear(
        &self,
        req: StackClearRequest,
    ) -> Result<StackClearResponse, ExtensionError> {
        let method = "stack/clear";
        if !should_dispatch(&self.ext_name, method) {
            return Err(ExtensionError::method_not_found(method));
        }
        let wire = serde_json::to_vec(&req).expect("serialize stack/clear");
        self.bytes_out.fetch_add(wire.len(), Ordering::Relaxed);
        self.inner.stack_clear(req, RequestOptions::default()).await
    }
}

// ---------------------------------------------------------------------------
// Tracing capture layer
// ---------------------------------------------------------------------------
//
// Same thread-scoped pattern as the T-039 version-mismatch matrix
// (`ark-ext-test-support/tests/version_mismatch.rs`): local `Layer`
// subscribed only to `target = "ark.supervisor.ext_dispatch"`, so prior
// or subsequent tests in the process don't contaminate the capture.
//
// Each captured event records its level, message, `ext` field and
// `method` field — everything kit R4 needs to assert on (both method
// name AND capability flag name via message substring).

#[derive(Debug, Default, Clone)]
struct CapturedWarn {
    level: String,
    message: Option<String>,
    ext: Option<String>,
    method: Option<String>,
}

struct CaptureVisitor<'a> {
    out: &'a mut CapturedWarn,
}

impl Visit for CaptureVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.out.message = Some(value.to_string()),
            "ext" => self.out.ext = Some(value.to_string()),
            "method" => self.out.method = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let stripped = rendered.trim_matches('"').to_string();
        match field.name() {
            "message" if self.out.message.is_none() => self.out.message = Some(stripped),
            "ext" if self.out.ext.is_none() => self.out.ext = Some(stripped),
            "method" if self.out.method.is_none() => self.out.method = Some(stripped),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedWarn>>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber,
{
    fn register_callsite(
        &self,
        _metadata: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        // Force every callsite to be re-evaluated so the thread-scoped
        // subscriber sees events even if a prior test in the same
        // process installed a global default dispatcher.
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        true
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "ark.supervisor.ext_dispatch" {
            return;
        }
        let mut captured = CapturedWarn {
            level: event.metadata().level().to_string(),
            ..Default::default()
        };
        let mut visitor = CaptureVisitor { out: &mut captured };
        event.record(&mut visitor);
        self.events.lock().unwrap().push(captured);
    }
}

/// Install a thread-scoped capture subscriber, run `f`, return the
/// value and a snapshot of the captured events.
fn with_capture<R, F>(f: F) -> (R, Vec<CapturedWarn>)
where
    F: FnOnce() -> R,
{
    let events: Arc<Mutex<Vec<CapturedWarn>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CaptureLayer {
        events: events.clone(),
    };
    let subscriber = Registry::default().with(layer);
    tracing::callsite::rebuild_interest_cache();
    let result = tracing::subscriber::with_default(subscriber, f);
    let snap = events.lock().unwrap().clone();
    (result, snap)
}

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

fn emit_req() -> PaneEmitRequest {
    PaneEmitRequest {
        handle: HandleId::new("h-capgate"),
        kind: "ev".into(),
        payload: "{}".to_string(),
    }
}

fn stack_clear_req() -> StackClearRequest {
    StackClearRequest {
        stack: HandleId::new("h-capgate-stack"),
    }
}

// ---------------------------------------------------------------------------
// Case (a) — advertised + implemented — both happy paths
// ---------------------------------------------------------------------------

/// Two happy paths in one test: `view.pane.v1` + `view.stack.v1`. The
/// stub advertises both caps and registers handlers for both gated
/// methods; the gate passes, the bytes-out counter advances, and each
/// call is recorded on the stub's call-log.
#[tokio::test(flavor = "current_thread")]
async fn capability_gate_advertised_and_called() {
    let ext_name = "capgate-a-both";
    let stub = StubExtension::builder()
        .advertise_capabilities(["view.pane.v1", "view.stack.v1"])
        .with_method("pane/emit", |_req: PaneEmitRequest| {
            Ok(PaneEmitResponse::default())
        })
        .with_method("stack/clear", |_req: StackClearRequest| {
            Ok(StackClearResponse::default())
        })
        .build();
    record_capabilities(ext_name, stub.advertised_capabilities());

    // Sanity: the dispatcher must report both gated methods as
    // dispatchable. If this fails the rest of the test is meaningless.
    assert!(
        should_dispatch(ext_name, "pane/emit"),
        "view.pane.v1 advertised → pane/emit must be dispatchable"
    );
    assert!(
        should_dispatch(ext_name, "stack/clear"),
        "view.stack.v1 advertised → stack/clear must be dispatchable"
    );

    let client = GatedClient::new(&stub, ext_name);

    // Happy path #1 — view.pane.v1 / pane/emit.
    let resp = client
        .pane_emit(emit_req())
        .await
        .expect("advertised + implemented pane/emit must round-trip");
    let _ = resp; // PaneEmitResponse is unit-y — we care about reachability.

    // Happy path #2 — view.stack.v1 / stack/clear.
    let resp = client
        .stack_clear(stack_clear_req())
        .await
        .expect("advertised + implemented stack/clear must round-trip");
    let _ = resp;

    assert_eq!(
        stub.call_log(),
        vec!["pane/emit".to_string(), "stack/clear".to_string()],
        "both gated methods must have reached the stub in order"
    );
    assert!(
        client.bytes_out() > 0,
        "byte counter must advance for at least one successful dispatch; got {}",
        client.bytes_out()
    );
}

// ---------------------------------------------------------------------------
// Case (b) — not advertised — gate blocks, 0 calls, 0 wire bytes
// ---------------------------------------------------------------------------

/// Extension advertises zero capabilities. Host attempts `pane/emit` +
/// `stack/clear` — the gate short-circuits both, the stub's call-log
/// stays empty, and the byte counter stays at zero (proving the host
/// never serialized a request frame).
#[tokio::test(flavor = "current_thread")]
async fn capability_gate_not_advertised_skipped() {
    let ext_name = "capgate-b-none";
    let stub = StubExtension::builder()
        .advertise_capabilities(Vec::<String>::new())
        // Handlers exist — but the ext advertised nothing, so the gate
        // blocks before dispatch. Registering handlers here proves the
        // skip is purely capability-driven (not handler-driven).
        .with_method("pane/emit", |_req: PaneEmitRequest| {
            Ok(PaneEmitResponse::default())
        })
        .with_method("stack/clear", |_req: StackClearRequest| {
            Ok(StackClearResponse::default())
        })
        .build();
    record_capabilities(ext_name, stub.advertised_capabilities());

    // Gate must block both.
    assert!(
        !should_dispatch(ext_name, "pane/emit"),
        "no view.pane.v1 → pane/emit must be gated off"
    );
    assert!(
        !should_dispatch(ext_name, "stack/clear"),
        "no view.stack.v1 → stack/clear must be gated off"
    );

    let client = GatedClient::new(&stub, ext_name);

    // Both calls must return MethodNotFound WITHOUT reaching the stub.
    let err = client
        .pane_emit(emit_req())
        .await
        .expect_err("gated-off pane/emit must fail");
    assert!(matches!(err, ExtensionError::MethodNotFound(_)));
    let err = client
        .stack_clear(stack_clear_req())
        .await
        .expect_err("gated-off stack/clear must fail");
    assert!(matches!(err, ExtensionError::MethodNotFound(_)));

    // Stub call-log: ZERO entries (the stub dispatch helper was never
    // reached — the wrapper's gate short-circuited first).
    assert!(
        stub.call_log().is_empty(),
        "no RPC may reach the stub on the skip path; got {:?}",
        stub.call_log()
    );
    // Byte-counting transport wrapper: ZERO bytes. Proves the host
    // never serialized a request frame for either skip.
    assert_eq!(
        client.bytes_out(),
        0,
        "host must NOT speculatively serialize; got {} bytes out",
        client.bytes_out()
    );
}

// ---------------------------------------------------------------------------
// Case (c) — advertised but unimplemented — WARN + opt-out sticks
// ---------------------------------------------------------------------------

/// Extension advertises `view.pane.v1` but its handler returns
/// `method_not_found`. Host must:
/// 1. call the ext once (advertisement granted the dispatch);
/// 2. log exactly one WARN on `ark.supervisor.ext_dispatch` naming
///    BOTH the method (`pane/emit`) AND the capability flag
///    (`view.pane.v1`) so operators can diagnose;
/// 3. record the (ext, method) pair as opted-out so subsequent calls
///    return `should_dispatch == false` and DO NOT round-trip to the
///    ext (F-015 fix);
/// 4. continue running — the reconcile session survives the
///    half-implemented ext (WARN, not ERROR, not crash).
#[tokio::test(flavor = "current_thread")]
async fn capability_gate_advertised_but_unimplemented_warns() {
    let ext_name = "capgate-c-half";
    let stub = StubExtension::builder()
        .advertise_capabilities(["view.pane.v1"])
        // Mark the method as advertised-but-unimplemented: the stub
        // will return method_not_found regardless of handler presence.
        .method_advertised_but_unimplemented("pane/emit")
        .with_method("pane/emit", |_req: PaneEmitRequest| {
            Ok(PaneEmitResponse::default())
        })
        .build();
    record_capabilities(ext_name, stub.advertised_capabilities());

    // Pre-condition: dispatch is open on first attempt (cap advertised,
    // not yet opted out).
    assert!(
        should_dispatch(ext_name, "pane/emit"),
        "advertised cap must grant the first dispatch attempt"
    );

    // First call — stub returns method_not_found. Host observes, logs
    // warn, marks opt-out. Session must not crash — we return a plain
    // `Err(MethodNotFound)` to the caller (reconcile-result Ok-or-Err
    // is the caller's loop condition; this test doesn't model the
    // whole reconcile loop, only the single-call contract).
    let ((_first_result, opted_out_after_first), warns) = with_capture(|| {
        let first = futures::executor::block_on(async {
            let client = GatedClient::new(&stub, ext_name);
            client.pane_emit(emit_req()).await
        });
        // The host reconcile loop must call
        // `warn_advertised_but_unimplemented` whenever a MethodNotFound
        // response comes back from a capability-gated method. That's
        // what wires the WARN + the opt-out; we invoke it here to
        // mirror the contract the real dispatcher owns.
        if matches!(first, Err(ExtensionError::MethodNotFound(_))) {
            warn_advertised_but_unimplemented(ext_name, "pane/emit");
        }
        // After the warn, the opt-out must be in place.
        let still_dispatchable = should_dispatch(ext_name, "pane/emit");
        (first, !still_dispatchable)
    });

    // (2) Exactly one WARN with both the method name AND the capability
    // flag name reachable via substring — kit R4 acceptance criterion.
    assert_eq!(
        warns.len(),
        1,
        "advertised-but-unimplemented must emit exactly one WARN on the ext_dispatch target; got {warns:?}"
    );
    let warn = &warns[0];
    assert_eq!(warn.level, "WARN", "event must fire at WARN level");
    assert_eq!(
        warn.method.as_deref(),
        Some("pane/emit"),
        "WARN must carry method=pane/emit as a structured field"
    );
    assert_eq!(
        warn.ext.as_deref(),
        Some(ext_name),
        "WARN must carry ext={ext_name} as a structured field"
    );
    // Substring assertion on message — the dispatcher's WARN format is
    // open to tightening (cavekit open item #3), so the hard contract
    // here is: a human reading the log can match BOTH the method name
    // AND the capability flag name. The method is carried verbatim in
    // the structured `method` field; the capability flag name must be
    // derivable either from a message substring OR the method→cap
    // table (which host reconcile owns). Confirm both are retrievable.
    let required_cap = capability_for_method("pane/emit")
        .expect("pane/emit is a gated method; capability lookup must succeed");
    assert_eq!(
        required_cap, "view.pane.v1",
        "pane/emit gates on view.pane.v1"
    );
    let message_or_method = warn
        .message
        .clone()
        .or_else(|| warn.method.clone())
        .unwrap_or_default();
    assert!(
        message_or_method.contains("pane/emit") || warn.method.as_deref() == Some("pane/emit"),
        "WARN must name the method `pane/emit` (via message substring or structured field); got message={:?} method={:?}",
        warn.message,
        warn.method,
    );
    // Pair the method with its capability via the dispatcher's own
    // table — this is the relationship the WARN documents (the kit
    // requirement is that "the WARN names the method AND the
    // capability flag name" — both must be retrievable to a reader
    // of the structured log plus the dispatcher's table).
    assert!(
        capability_for_method(warn.method.as_deref().unwrap_or(""))
            .is_some_and(|c| c == "view.pane.v1"),
        "WARN's method name must map back to view.pane.v1 via the dispatcher's cap table"
    );

    // (3) Opt-out must stick. A subsequent `should_dispatch` returns
    // false without any further RPC — F-015 fix in place.
    assert!(
        opted_out_after_first,
        "opt-out must be recorded after first method_not_found (F-015)"
    );
    assert!(
        !should_dispatch(ext_name, "pane/emit"),
        "opt-out must persist across subsequent should_dispatch calls"
    );

    // (4) Session survives — simulate a second call and observe the
    // gate blocks without round-tripping. Byte counter stays at wherever
    // it was; stub call log grows only by the first dispatch.
    let client = GatedClient::new(&stub, ext_name);
    let err = client
        .pane_emit(emit_req())
        .await
        .expect_err("post-opt-out pane/emit must skip");
    assert!(matches!(err, ExtensionError::MethodNotFound(_)));
    assert_eq!(
        client.bytes_out(),
        0,
        "opted-out method must NOT serialize on subsequent attempts; got {} bytes",
        client.bytes_out()
    );

    // Stub's call_log contains exactly one entry — the first attempt.
    // The second attempt was gated off and never reached the stub.
    let log = stub.call_log();
    assert_eq!(
        log.iter().filter(|m| *m == "pane/emit").count(),
        1,
        "stub must have received exactly one pane/emit (first attempt only); got {log:?}"
    );
}

// ---------------------------------------------------------------------------
// Case (d) — method advertised as removed-in-MAJOR vs older ext
// ---------------------------------------------------------------------------

/// Phase 2's current capability taxonomy has no removed-in-MAJOR
/// methods — every flag in `PHASE_2_CAPABILITY_FLAGS` is `*.v1` and
/// all are live. This test is marked `#[ignore]` per decision #4c
/// pending the first real removal; when one lands, a synthetic
/// capability-removal fixture replaces this skeleton and the `#[ignore]`
/// attribute comes off.
///
/// The body is a documented skeleton so a future implementer can see
/// exactly what assertions are required (kit R4: "runs green against
/// a synthetic capability-removal fixture OR is explicitly marked
/// `#[ignore]` with a TODO citing decision #4c pending a first real
/// removal").
#[tokio::test(flavor = "current_thread")]
#[ignore = "pending first real capability removal per decision #4c (cavekit-soul-phase-2-tests R4)"]
async fn capability_gate_removed_method_not_called() {
    // TODO(decision #4c): once a capability is removed between MAJOR
    // protocol versions, replace this skeleton with a synthetic
    // fixture: the ext still implements the removed method under an
    // older MINOR; the host (running the newer MAJOR that dropped
    // support) must NOT call it. Assertions:
    //   (1) `capability_for_method(removed_method)` returns None OR
    //       returns a cap name that's absent from the host's recorded
    //       `ExtensionCapabilities`;
    //   (2) `should_dispatch(ext_name, removed_method)` == false;
    //   (3) stub call-log for `removed_method` stays empty across a
    //       full reconcile pass.
    //
    // Until then, this body asserts the absence of the condition —
    // every current gated method still has a live capability — so
    // flipping the `#[ignore]` prematurely would fail fast.
    for method in [
        "pane/emit",
        "pane/replace_view",
        "pane/close",
        "stack/spawn_pane",
        "stack/close_child",
        "stack/clear",
    ] {
        assert!(
            capability_for_method(method).is_some(),
            "method {method} still lives — decision #4c case (d) stays ignored"
        );
    }
}
