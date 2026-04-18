//! `ark-stub-ext` — NDJSON subprocess variant of the in-process
//! [`ark_ext_test_support::StubExtension`] (T-038, soul phase 2
//! cavekit-tests R2).
//!
//! The bin wraps the R1 stub in a [`tokio::main`] that pumps
//! stdin→[`ark_ext_proto::transport::ndjson::NdjsonServer`]→stdout.
//! Configuration is read from env vars at startup (shell-scriptable;
//! mirrors how real extensions would typically pick up per-session
//! config from their launcher). An EOF on stdin ends the loop and the
//! process exits `0`.
//!
//! # Env var surface
//!
//! All fields are optional; unset variables fall through to empty /
//! default values.
//!
//! - `ARK_STUB_VERSION` — semver string reported in the stub's
//!   manifest (`ExtensionMetadata.version`). Defaults to `1.0.0`.
//! - `ARK_STUB_CAPABILITIES` — comma-separated list of capability
//!   flags the stub advertises (e.g.
//!   `"view.pane.v1,view.stack.v1"`). Defaults to empty.
//! - `ARK_STUB_INTENTS` — comma-separated list of intent names the
//!   stub's manifest declares. Defaults to empty.
//! - `ARK_STUB_VIEW_TYPES` — comma-separated list of `name:kind`
//!   pairs (e.g. `"EditorView:pane,Sidebar:pane"`). Defaults to
//!   empty.
//! - `ARK_STUB_METHODS` — comma-separated list of JSON-RPC method
//!   names for which the stub registers a handler that returns the
//!   default (empty) response. Any method not in this list falls
//!   through to the trait-default `method_not_found`. Defaults to
//!   empty.
//! - `ARK_STUB_METHOD_NOT_FOUND_LIST` — comma-separated list of
//!   method names marked as advertised-but-unimplemented. These
//!   return `method_not_found` at dispatch even if they appear in
//!   `ARK_STUB_METHODS` (the opt-out marker wins per
//!   [`StubBuilder::method_advertised_but_unimplemented`]).
//!
//! # Exit codes
//!
//! - `0` — clean EOF on stdin; every dispatched frame got a response
//!   written to stdout before shutdown.
//! - non-zero — panics / io errors bubble up through
//!   [`tokio::main`]'s harness.

use std::io;
use std::sync::Arc;

use ark_ext_metadata_types::{ExtensionMetadata, IntentDecl, StringNode, ViewDecl};
use ark_ext_proto::transport::ndjson::NdjsonServer;
use ark_ext_proto::{
    ControlVerbsRequest, ControlVerbsResponse, DoctorChecksRequest, DoctorChecksResponse,
    ListColumnsRequest, ListColumnsResponse, OnSessionEndRequest, OnSessionEndResponse,
    OnSessionStartRequest, OnSessionStartResponse, PaneCloseRequest, PaneCloseResponse,
    PaneEmitRequest, PaneEmitResponse, PaneReplaceViewRequest, PaneReplaceViewResponse,
    SceneCompileHookRequest, SceneCompileHookResponse, StackClearRequest, StackClearResponse,
    StackCloseChildRequest, StackCloseChildResponse, StackSpawnPaneRequest, StackSpawnPaneResponse,
};
use ark_ext_test_support::{StubBuilder, StubExtension};

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let stub = build_stub_from_env();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    // NdjsonServer::serve writes + flushes after each response, so
    // line-buffering quirks on stdout can't strand the host-side
    // reader. EOF on stdin ends the loop; the process then exits 0.
    let _ = NdjsonServer::serve(stdin, stdout, Arc::new(stub)).await?;
    Ok(())
}

fn build_stub_from_env() -> StubExtension {
    let version = std::env::var("ARK_STUB_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    let capabilities = parse_csv("ARK_STUB_CAPABILITIES");
    let intents = parse_csv("ARK_STUB_INTENTS");
    let view_types = parse_csv("ARK_STUB_VIEW_TYPES");
    let methods = parse_csv("ARK_STUB_METHODS");
    let unimpl = parse_csv("ARK_STUB_METHOD_NOT_FOUND_LIST");

    let manifest = build_manifest(&version, &intents, &view_types);

    let mut builder = StubBuilder::default()
        .advertise_capabilities(capabilities)
        .with_manifest(manifest);

    for m in &methods {
        builder = register_default_handler(builder, m);
    }
    for m in &unimpl {
        builder = builder.method_advertised_but_unimplemented(m.clone());
    }

    builder.build()
}

/// Parse a comma-separated env var into a `Vec<String>`. Empty /
/// missing env vars yield an empty vec. Individual entries are
/// trimmed; empty segments are dropped so `"a,,b"` → `["a", "b"]`.
fn parse_csv(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Build an `ExtensionMetadata` from the parsed env-var slices. View
/// entries are `name:kind` pairs; anything without a `:` falls back
/// to `kind = None`.
fn build_manifest(version: &str, intents: &[String], view_types: &[String]) -> ExtensionMetadata {
    let mut manifest = ExtensionMetadata {
        name: StringNode::new("ark-stub-ext"),
        version: StringNode::new(version),
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
    for name in intents {
        manifest.intents.push(IntentDecl {
            name: name.clone(),
            args_schema: StringNode::new(""),
        });
    }
    for vt in view_types {
        let (name, kind) = match vt.split_once(':') {
            Some((n, k)) => (n.trim().to_string(), Some(StringNode::new(k.trim()))),
            None => (vt.clone(), None),
        };
        manifest.views.push(ViewDecl {
            name,
            component: StringNode::new(""),
            kind,
        });
    }
    manifest
}

/// Register a default-response handler for `method`. Methods outside
/// the Phase-2 surface are ignored — the trait's default impl will
/// return `method_not_found` for them, which is the desired parity
/// behaviour.
fn register_default_handler(builder: StubBuilder, method: &str) -> StubBuilder {
    match method {
        "pane/emit" => builder.with_method(method, |_req: PaneEmitRequest| {
            Ok(PaneEmitResponse::default())
        }),
        "pane/replace_view" => builder.with_method(method, |_req: PaneReplaceViewRequest| {
            Ok(PaneReplaceViewResponse::default())
        }),
        "pane/close" => builder.with_method(method, |_req: PaneCloseRequest| {
            Ok(PaneCloseResponse::default())
        }),
        "stack/spawn_pane" => builder.with_method(method, |_req: StackSpawnPaneRequest| {
            Ok(StackSpawnPaneResponse {
                handle: ark_view::HandleId::new("stub-spawned"),
            })
        }),
        "stack/close_child" => builder.with_method(method, |_req: StackCloseChildRequest| {
            Ok(StackCloseChildResponse::default())
        }),
        "stack/clear" => builder.with_method(method, |_req: StackClearRequest| {
            Ok(StackClearResponse::default())
        }),
        "on_session_start" => builder.with_method(method, |_req: OnSessionStartRequest| {
            Ok(OnSessionStartResponse::default())
        }),
        "on_session_end" => builder.with_method(method, |_req: OnSessionEndRequest| {
            Ok(OnSessionEndResponse::default())
        }),
        "scene_compile_hook" => builder.with_method(method, |_req: SceneCompileHookRequest| {
            Ok(SceneCompileHookResponse::default())
        }),
        "control_verbs" => builder.with_method(method, |_req: ControlVerbsRequest| {
            Ok(ControlVerbsResponse::default())
        }),
        "doctor_checks" => builder.with_method(method, |_req: DoctorChecksRequest| {
            Ok(DoctorChecksResponse::default())
        }),
        "list_columns" => builder.with_method(method, |_req: ListColumnsRequest| {
            Ok(ListColumnsResponse::default())
        }),
        // Unknown methods: leave unregistered — trait default returns
        // `method_not_found`, which is deterministic parity behaviour
        // against the in-process stub configured the same way.
        _ => builder,
    }
}
