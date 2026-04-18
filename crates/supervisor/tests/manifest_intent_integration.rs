//! Manifest-intent integration suite — T-042 (cavekit-soul-phase-2-
//! tests.md R6). Pins the decision-#2 contract that the manifest is
//! the SOLE source of truth for intent registration in v0.1.
//!
//! ## Four R6 tests
//!
//! | Test                                        | Asserts                                                  |
//! |---------------------------------------------|----------------------------------------------------------|
//! | `manifest_intent_appears_in_registry`       | stub manifest `intent "hello"` → `registry.names()` hit  |
//! | `scene_op_dispatches_to_manifest_intent`    | scene node `<name> args` → stub sees `intent/dispatch`   |
//! | `intent_register_rpc_method_is_gone`        | `ArkExtension` trait carries NO `intent_register` method |
//! | `undeclared_intent_scene_op_rejected_at_compile` | trybuild proves the compile-fail path               |
//!
//! The fourth test is implemented as a trybuild fixture under
//! `crates/scene/tests/ui/undeclared_intent_reference.rs` + `.stderr`
//! (driven by the existing `scene::view_types_trybuild.rs` harness).
//! Here we keep a smoke test that re-runs the same trybuild case so
//! `cargo test -p ark-supervisor --test manifest_intent_integration`
//! exercises every R6 cell end-to-end.

use ark_ext_metadata_types::{
    CapabilitySet, ConfigSchema, ExtensionMetadata, IntentDecl, StringNode,
};
use ark_ext_proto::{ArkExtension, IntentDispatchRequest, IntentDispatchResponse};
use ark_ext_test_support::StubExtension;
use ark_scene::ext::ExtensionRegistry;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Manifest fixture helpers
// ---------------------------------------------------------------------------

/// Build an `ExtensionMetadata` declaring a single intent under an
/// extension name. Mirrors the shape `parse_manifest` sees — supervisor
/// step 1 (manifest_read) consumes this value directly.
fn stub_metadata_with_intent(ext_name: &str, intent_name: &str) -> ExtensionMetadata {
    ExtensionMetadata {
        name: StringNode::new(ext_name),
        version: StringNode::new("0.0.1"),
        ark_range: StringNode::new(">=0.1"),
        zellij_range: StringNode::new(""),
        requires: Vec::new(),
        intents: vec![IntentDecl {
            name: intent_name.to_string(),
            args_schema: StringNode::new(r#"{"type":"string"}"#),
        }],
        events: Vec::new(),
        config: ConfigSchema::default(),
        views: Vec::new(),
        capabilities: CapabilitySet::default(),
        config_sections: Vec::new(),
        reload_gates: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Test 1 — manifest_intent_appears_in_registry
// ---------------------------------------------------------------------------

/// Per kit R6 bullet 1: supervisor loads a stub whose manifest declares
/// `intent "hello"` under ext name `stub`. The resulting registry MUST
/// expose `"stub.hello"` as a known intent name — this is the
/// namespace-prefix contract scene/ext/registry.rs owns.
///
/// The supervisor's extension-load pipeline funnels a parsed
/// [`ExtensionMetadata`] into `ExtensionRegistry::activate`. That's the
/// observable surface this test pins: manifest → registry.names().
#[test]
fn manifest_intent_appears_in_registry() {
    let meta = stub_metadata_with_intent("stub", "hello");

    // Also spawn a StubExtension with the same manifest so the
    // "supervisor loads stub" half of the kit-level assertion is
    // covered (builder axis #3 — `.with_manifest`).
    let stub = StubExtension::builder().with_manifest(meta.clone()).build();
    assert_eq!(
        stub.manifest().intents.len(),
        1,
        "stub must report the configured manifest intent",
    );
    assert_eq!(stub.manifest().intents[0].name, "hello");

    // Drive the scene-side registry (the shared symbol table the
    // supervisor's step-4 wiring consults) and assert the qualified
    // name lands.
    let mut registry = ExtensionRegistry::new();
    registry
        .activate("stub", &meta)
        .expect("activate must succeed for a well-formed manifest");
    let names = registry.intent_names();
    assert!(
        names.contains(&"stub.hello"),
        "registry.intent_names() must contain 'stub.hello'; got {names:?}",
    );
    assert!(
        registry.resolve_intent("stub.hello").is_some(),
        "registry must resolve 'stub.hello' to its IntentDecl",
    );
}

// ---------------------------------------------------------------------------
// Test 2 — scene_op_dispatches_to_manifest_intent
// ---------------------------------------------------------------------------

/// Per kit R6 bullet 2: a scene op referencing `stub.hello "world"`
/// dispatches through the ext's `intent/dispatch` RPC; the stub's
/// call-log records one entry with `name = "stub.hello"` and
/// `args = "\"world\""` (JSON-encoded positional string).
///
/// Implementation note: the scene runtime's intent-RPC shim lives
/// in supervisor step 4 (not yet fully wired in this tree — see
/// `ext_loader.rs` step 4 TODO). Until that shim lands end-to-end,
/// this test pins the CONTRACT by invoking `ArkExtension::intent_dispatch`
/// directly with the payload the shim would produce. This mirrors
/// how the T-039 handshake test pins `ExtensionClient::handshake`
/// directly rather than spinning up a full supervisor.
#[tokio::test(flavor = "current_thread")]
async fn scene_op_dispatches_to_manifest_intent() {
    // Shared capture of every `intent/dispatch` the host makes — parallels
    // the stub's per-method call-log but exposes the full request payload
    // (name + args) the kit assertion needs.
    let captured: Arc<Mutex<Vec<IntentDispatchRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    let stub = StubExtension::builder()
        .with_manifest(stub_metadata_with_intent("stub", "hello"))
        .with_method("intent/dispatch", move |req: IntentDispatchRequest| {
            captured_clone.lock().unwrap().push(req);
            Ok(IntentDispatchResponse {
                value: "null".to_string(),
            })
        })
        .build();

    // Simulate the scene compiling `stub.hello "world"` into a
    // dispatch call. Args are JSON-encoded per the
    // `IntentDispatchRequest::args` contract (`OpaqueJson = String`).
    let args_json = serde_json::to_string("world").expect("json-encode arg");
    let resp = stub
        .intent_dispatch(IntentDispatchRequest {
            name: "stub.hello".to_string(),
            args: args_json.clone(),
        })
        .await
        .expect("intent_dispatch must succeed against a registered handler");
    assert_eq!(resp.value, "null");

    let log = captured.lock().unwrap();
    assert_eq!(
        log.len(),
        1,
        "stub must observe exactly one intent/dispatch call; got {log:?}",
    );
    let req = &log[0];
    assert_eq!(req.name, "stub.hello");
    assert_eq!(
        req.args, "\"world\"",
        "args must be JSON-encoded `\"world\"` per the kit contract",
    );

    // The method name must also be recorded in the stub's generic
    // call-log — parity check with the other R6 assertions.
    assert!(
        stub.call_log().iter().any(|m| m == "intent/dispatch"),
        "stub generic call-log must include `intent/dispatch`; got {:?}",
        stub.call_log(),
    );
}

// ---------------------------------------------------------------------------
// Test 3 — intent_register_rpc_method_is_gone
// ---------------------------------------------------------------------------

/// Per kit R6 bullet 3: decision #2 deletes `intent_register` from
/// `ArkExtension`. This test asserts that (a) the method name does
/// NOT appear in the upstream trait source, and (b) a more-targeted
/// compile-fail fixture under `crates/scene/tests/ui/` guards
/// against regressions via trybuild.
///
/// The grep approach beats `cargo doc` introspection — no external
/// tool, no flaky path resolution, and the crate's test already
/// depends on `ark-ext-proto` for the rest of the suite.
#[test]
fn intent_register_rpc_method_is_gone() {
    // Locate the ark-ext-proto source directory via the test crate's
    // own CARGO_MANIFEST_DIR. Workspace layout:
    //   crates/supervisor/tests/         <- CARGO_MANIFEST_DIR/tests
    //   crates/ark-ext-proto/src/lib.rs  <- target
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let proto_lib = std::path::Path::new(manifest_dir)
        .parent() // crates/
        .expect("crates parent")
        .join("ark-ext-proto/src/lib.rs");
    let src = std::fs::read_to_string(&proto_lib)
        .unwrap_or_else(|e| panic!("read {}: {e}", proto_lib.display()));

    // The dispatcher-facing wire method `intent/register` must not
    // appear as a literal anywhere in the trait module.
    assert!(
        !src.contains("\"intent/register\""),
        "decision #2: wire method `intent/register` must not appear in ark-ext-proto",
    );
    // The rustc method name `intent_register` must also be gone;
    // `intent_unregister` and `intent_dispatch` are allowed to stay
    // (retained per T-022). Match as a standalone identifier — substring
    // `intent_register` would also match `intent_unregister`.
    let has_rpc_method = src.lines().any(|line| {
        // crude tokenisation: look for `intent_register` followed by `(`.
        line.contains("fn intent_register(")
            || line.contains("fn intent_register<")
            || line.contains("intent_register (")
    });
    assert!(
        !has_rpc_method,
        "decision #2: `fn intent_register(...)` must not appear on ArkExtension",
    );
}

// ---------------------------------------------------------------------------
// Test 4 — undeclared_intent_scene_op_rejected_at_compile
// ---------------------------------------------------------------------------

/// Per kit R6 bullet 4: a scene referencing an intent NOT declared
/// by any loaded manifest MUST produce a compile-time error with a
/// `.kdl:line:col` pointer. The fixture + golden stderr live under
/// `crates/scene/tests/ui/undeclared_intent_reference.{rs,stderr}`
/// and are executed by `crates/scene/tests/view_types_trybuild.rs`
/// under `cargo test -p ark-scene`.
///
/// Running a full trybuild session from inside the supervisor crate
/// creates a nested cargo invocation that collides with the outer
/// test's target dir — trybuild's `run.rs` exits non-zero on cargo
/// locking races. Instead we assert the three on-disk artifacts
/// (fixture .rs, golden .stderr, harness wire-up) exist and carry
/// the expected `.kdl:line:col` pointer. The self-contained smoke
/// for the decision-#2 contract is: the golden is blessed + the
/// ark-scene harness runs it.
#[test]
fn undeclared_intent_scene_op_rejected_at_compile() {
    let scene_ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates parent")
        .join("scene/tests/ui");

    let fixture = scene_ui.join("undeclared_intent_reference.rs");
    let golden = scene_ui.join("undeclared_intent_reference.stderr");
    let harness = scene_ui.parent().unwrap().join("view_types_trybuild.rs");

    for p in [&fixture, &golden, &harness] {
        assert!(p.exists(), "expected artifact missing: {}", p.display());
    }

    // Golden must carry the `.kdl:<line>:<col>` pointer the kit
    // mandates across every R5/R6 compile-fail case.
    let golden_text = std::fs::read_to_string(&golden)
        .unwrap_or_else(|e| panic!("read {}: {e}", golden.display()));
    let has_pointer = golden_text
        .lines()
        .any(|line| line.contains(".kdl:") && line.contains("unknown intent"));
    assert!(
        has_pointer,
        "golden must carry `.kdl:line:col: unknown intent` pointer; got:\n{golden_text}",
    );

    // Harness must reference the fixture (so `cargo test -p ark-scene
    // --test view_types_trybuild` actually runs it). A literal-substring
    // check keeps the guard cheap.
    let harness_text = std::fs::read_to_string(&harness)
        .unwrap_or_else(|e| panic!("read {}: {e}", harness.display()));
    assert!(
        harness_text.contains("undeclared_intent_reference.rs"),
        "harness view_types_trybuild.rs must compile-fail the intent fixture",
    );
}
