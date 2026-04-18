//! Version-mismatch test matrix — T-039 (soul phase 2 cavekit-tests R3).
//!
//! Enumerates the outcomes of (ark proto version × ext proto version)
//! handshake permutations per decision #4c. Five cells cover the MAJOR
//! and MINOR compat boundaries:
//!
//! | Client MAJOR.MINOR | Ext MAJOR.MINOR | Expected outcome                         |
//! |--------------------|-----------------|------------------------------------------|
//! | 1.1                | 1.1             | Handshake OK, no warnings.               |
//! | 1.1                | 1.0             | Handshake OK, same MAJOR older MINOR.    |
//! | 1.0                | 1.1             | Handshake OK, WARN on newer-ext MINOR.   |
//! | 2.0                | 1.1             | `UnsupportedVersion`; no subsequent RPC. |
//! | 1.1                | 2.0             | `UnsupportedVersion`; no subsequent RPC. |
//!
//! Driven by [`StubExtension::builder().with_protocol_version(...)`]
//! (the R1 stub's version axis) — zero hand-rolled server wiring. The
//! WARN on the 1.0↔1.1 row is emitted from
//! [`ark_ext_proto::ExtensionClient::handshake`] via
//! `target = "ark.ext_proto.handshake"` and captured here by a custom
//! `tracing_subscriber::Layer` installed via `with_default`.
//!
//! For the two `UnsupportedVersion` cells we also assert that no
//! subsequent RPC is attempted against the ext — observable via the
//! stub's call-log staying empty after the failed handshake.

use ark_ext_proto::{
    Capabilities, ExtensionClient, ExtensionError, InProcClient, ProtocolVersion, RequestOptions,
};
use ark_ext_test_support::StubExtension;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;

// ---------------------------------------------------------------------------
// Tracing capture layer
// ---------------------------------------------------------------------------
//
// Scoped-subscriber pattern (per `ext_loader::tests::install_global_capture`
// but kept local + thread-scoped here because these tests never install a
// global default). Each test body wraps its `handshake(...)` call in
// `tracing::subscriber::with_default(...)`, so events fire against the
// capture layer on the current thread only.

/// One captured `ark.ext_proto.handshake` event, flattened into the
/// fields the assertions care about.
#[derive(Debug, Default, Clone)]
struct CapturedWarn {
    level: String,
    client_version: Option<String>,
    ext_version: Option<String>,
    ext_capabilities: Option<String>,
    message: Option<String>,
}

struct CaptureVisitor<'a> {
    out: &'a mut CapturedWarn,
}

impl<'a> Visit for CaptureVisitor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "client_version" => self.out.client_version = Some(value.to_string()),
            "ext_version" => self.out.ext_version = Some(value.to_string()),
            "ext_capabilities" => self.out.ext_capabilities = Some(value.to_string()),
            "message" => self.out.message = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `%display` formatting reaches `record_debug` in some tracing
        // macro expansions — strip the surrounding quotes for &str.
        let rendered = format!("{value:?}");
        let stripped = rendered.trim_matches('"').to_string();
        match field.name() {
            "client_version" => {
                if self.out.client_version.is_none() {
                    self.out.client_version = Some(stripped);
                }
            }
            "ext_version" => {
                if self.out.ext_version.is_none() {
                    self.out.ext_version = Some(stripped);
                }
            }
            "ext_capabilities" => {
                if self.out.ext_capabilities.is_none() {
                    self.out.ext_capabilities = Some(stripped);
                }
            }
            "message" => {
                if self.out.message.is_none() {
                    self.out.message = Some(stripped);
                }
            }
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
        // Same defensive override as the supervisor ext_loader capture
        // layer: force every callsite to be re-evaluated so the
        // thread-scoped subscriber sees events even if a prior test in
        // the process installed a global default.
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        true
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "ark.ext_proto.handshake" {
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

/// Install a thread-scoped capture subscriber, run `f`, collect warns.
fn with_capture<R, F>(f: F) -> (R, Vec<CapturedWarn>)
where
    F: FnOnce() -> R,
{
    let events: Arc<Mutex<Vec<CapturedWarn>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CaptureLayer {
        events: events.clone(),
    };
    let subscriber = Registry::default().with(layer);
    // Rebuild cache so prior global subscribers (from other crates'
    // tests) don't lock callsites to never-fire for this layer.
    tracing::callsite::rebuild_interest_cache();
    let result = tracing::subscriber::with_default(subscriber, f);
    let events_snapshot = events.lock().unwrap().clone();
    (result, events_snapshot)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a stub that reports `ext_version` + an advertised capability
/// bag. The advertised caps ride through `initialize` so the host sees
/// something concrete to enumerate on the WARN path.
fn stub_at(ext_version: ProtocolVersion) -> StubExtension {
    StubExtension::builder()
        .with_protocol_version(ext_version)
        .advertise_capabilities(["view.pane.v1", "ext.lifecycle.v1"])
        .build()
}

/// Handshake the stub at `client_version`. Returns the
/// [`InitializeResponse`] on success or the raw error.
async fn run_handshake(
    stub: &StubExtension,
    client_version: ProtocolVersion,
) -> Result<ark_ext_proto::InitializeResponse, ExtensionError> {
    let client = InProcClient::new(Arc::new(stub.clone()));
    client
        .handshake(
            client_version,
            Capabilities::from_iter(["view.pane.v1"]),
            "ark-test/version-mismatch".into(),
            RequestOptions::default(),
        )
        .await
}

// ---------------------------------------------------------------------------
// Row 1 — client 1.1, ext 1.1 — OK, no warn
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn handshake_version_match_1_1_and_1_1_ok_no_warn() {
    let stub = stub_at(ProtocolVersion::new(1, 1));
    let (result, warns) = with_capture(|| {
        futures::executor::block_on(run_handshake(&stub, ProtocolVersion::new(1, 1)))
    });

    let resp = result.expect("same MAJOR.MINOR must handshake OK");
    assert_eq!(resp.protocol_version, "1.1");
    assert!(
        !resp.session_token.is_empty(),
        "host must mint a session token on success"
    );
    assert!(
        warns.is_empty(),
        "matching versions must NOT emit any handshake warnings; got {warns:?}"
    );
}

// ---------------------------------------------------------------------------
// Row 2 — client 1.1 (newer), ext 1.0 (older) — OK, no warn
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn handshake_version_mismatch_client_newer_minor_ok_no_warn() {
    let stub = stub_at(ProtocolVersion::new(1, 0));
    let (result, warns) = with_capture(|| {
        futures::executor::block_on(run_handshake(&stub, ProtocolVersion::new(1, 1)))
    });

    let resp = result.expect("same MAJOR, older MINOR on ext side must handshake OK");
    assert_eq!(resp.protocol_version, "1.0");
    assert!(
        warns.is_empty(),
        "host=newer, ext=older must NOT warn (caps simply aren't advertised); got {warns:?}"
    );
}

// ---------------------------------------------------------------------------
// Row 3 — client 1.0 (older), ext 1.1 (newer) — OK, WARN
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn handshake_version_mismatch_ext_newer_minor_ok_with_warn() {
    let stub = stub_at(ProtocolVersion::new(1, 1));
    let (result, warns) = with_capture(|| {
        futures::executor::block_on(run_handshake(&stub, ProtocolVersion::new(1, 0)))
    });

    let resp = result.expect("same MAJOR, newer MINOR on ext side must handshake OK (best-effort)");
    assert_eq!(resp.protocol_version, "1.1");
    assert_eq!(
        warns.len(),
        1,
        "host=older, ext=newer must emit exactly one WARN; got {warns:?}"
    );
    let warn = &warns[0];
    assert_eq!(warn.level, "WARN", "event must fire at WARN level");
    assert_eq!(warn.client_version.as_deref(), Some("1.0"));
    assert_eq!(warn.ext_version.as_deref(), Some("1.1"));
    let caps_str = warn
        .ext_capabilities
        .as_deref()
        .expect("warn payload must carry ext_capabilities");
    // Advertised bag includes "view.pane.v1" — surfacing the concrete
    // capability name on the warn is what lets operators diagnose.
    assert!(
        caps_str.contains("pane"),
        "warn payload should name unknown advertised capabilities; got {caps_str:?}"
    );
}

// ---------------------------------------------------------------------------
// Row 4 — client 2.0, ext 1.1 — UnsupportedVersion, no subsequent RPC
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn handshake_version_mismatch_major_client_newer_rejects() {
    let stub = stub_at(ProtocolVersion::new(1, 1));
    let err = run_handshake(&stub, ProtocolVersion::new(2, 0))
        .await
        .expect_err("MAJOR mismatch (client=2.0, ext=1.1) must be rejected");
    match err {
        ExtensionError::UnsupportedVersion(msg) => {
            // Error message must name both versions so operators can
            // diagnose the bump side (kit R3).
            assert!(
                msg.contains("2.0"),
                "error message must mention client 2.0: {msg}"
            );
            assert!(
                msg.contains("1.1"),
                "error message must mention ext 1.1: {msg}"
            );
            assert!(
                msg.contains("MAJOR mismatch"),
                "error message must flag the MAJOR mismatch: {msg}"
            );
        }
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
    // No subsequent RPC was attempted — the stub's call log (pane/emit
    // et al) stays empty because the caller aborts after handshake.
    assert!(
        stub.call_log().is_empty(),
        "no subsequent RPC may be dispatched after UnsupportedVersion; got {:?}",
        stub.call_log(),
    );
}

// ---------------------------------------------------------------------------
// Row 5 — client 1.1, ext 2.0 — symmetric: UnsupportedVersion, no RPC
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn handshake_version_mismatch_major_ext_newer_rejects() {
    let stub = stub_at(ProtocolVersion::new(2, 0));
    let err = run_handshake(&stub, ProtocolVersion::new(1, 1))
        .await
        .expect_err("MAJOR mismatch (client=1.1, ext=2.0) must be rejected");
    match err {
        ExtensionError::UnsupportedVersion(msg) => {
            assert!(
                msg.contains("1.1"),
                "error message must mention client 1.1: {msg}"
            );
            assert!(
                msg.contains("2.0"),
                "error message must mention ext 2.0: {msg}"
            );
            assert!(
                msg.contains("MAJOR mismatch"),
                "error message must flag the MAJOR mismatch: {msg}"
            );
        }
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
    assert!(
        stub.call_log().is_empty(),
        "no subsequent RPC may be dispatched after UnsupportedVersion (symmetric row); got {:?}",
        stub.call_log(),
    );
}
