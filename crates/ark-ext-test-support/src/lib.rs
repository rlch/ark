//! Test-support stub [`ArkExtension`] for Phase 2 conformance tests.
//!
//! Per `cavekit-soul-phase-2-tests.md` R1. Not reachable from production
//! binaries — workspace-member + dev-dep-only pattern (ark-supervisor,
//! ark-cli etc may depend on this crate ONLY under `[dev-dependencies]`).
//!
//! # Usage
//!
//! ```no_run
//! use ark_ext_test_support::StubExtension;
//! use ark_ext_proto::{PaneEmitRequest, PaneEmitResponse};
//!
//! let stub = StubExtension::builder()
//!     .advertise_capabilities(["view.pane.v1"])
//!     .with_method("pane/emit", |_req: PaneEmitRequest| {
//!         Ok(PaneEmitResponse::default())
//!     })
//!     .build();
//! ```
//!
//! ## Axes (kit R1)
//!
//! The builder lets a test fix the four axes independently:
//!
//! 1. **Per-method behavior** — [`StubBuilder::with_method`] registers a
//!    typed closure; methods with no handler default to
//!    `method_not_found`.
//! 2. **Capability advertisement** — [`StubBuilder::advertise_capabilities`]
//!    sets the stub's capability bag.
//! 3. **Manifest surface** — [`StubBuilder::with_manifest`] injects an
//!    [`ExtensionMetadata`] the tests can introspect.
//! 4. **Protocol version** — [`StubBuilder::with_protocol_version`].
//!
//! Plus the dispatcher-opt-out seam:
//! [`StubBuilder::method_advertised_but_unimplemented`] marks a method
//! as advertised-but-unimplemented so dispatch returns
//! `method_not_found` regardless of whether a handler was also
//! registered — exercising the host dispatcher's warn-once + opt-out
//! behaviour.

use ark_ext_metadata_types::ExtensionMetadata;
use ark_ext_proto::{
    ArkExtension, ControlVerbsRequest, ControlVerbsResponse, DoctorChecksRequest,
    DoctorChecksResponse, ExtResult, ExtensionError, InitializeRequest, InitializeResponse,
    ListColumnsRequest, ListColumnsResponse, OnSessionEndRequest, OnSessionEndResponse,
    OnSessionStartRequest, OnSessionStartResponse, PHASE_2_CAPABILITY_FLAGS, PaneCloseRequest,
    PaneCloseResponse, PaneEmitRequest, PaneEmitResponse, PaneReplaceViewRequest,
    PaneReplaceViewResponse, ProtocolVersion, SceneCompileHookRequest, SceneCompileHookResponse,
    StackClearRequest, StackClearResponse, StackCloseChildRequest, StackCloseChildResponse,
    StackSpawnPaneRequest, StackSpawnPaneResponse,
};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Erased per-method handler — serde-json boundary bridges the
/// heterogeneous typed Req/Resp pairs across a single dyn-safe storage
/// slot.
type MethodHandler =
    Arc<dyn Fn(serde_json::Value) -> Result<serde_json::Value, ExtensionError> + Send + Sync>;

/// Test-support stub implementation of [`ark_ext_proto::ArkExtension`].
///
/// Per-method behavior is configured via the builder: a closure returns
/// the response, the method is left at its trait-default
/// (`method_not_found`), or the method is marked as
/// `advertised_but_unimplemented` — the last case returns
/// `method_not_found` at dispatch despite the stub's capability bag
/// claiming support.
#[derive(Clone)]
pub struct StubExtension {
    inner: Arc<StubInner>,
}

struct StubInner {
    /// Capabilities the stub advertises (kit R1 axis #2).
    advertised: HashSet<String>,
    /// Manifest-equivalent bag (kit R1 axis #3).
    manifest: ExtensionMetadata,
    /// Protocol version (kit R1 axis #4).
    protocol_version: ProtocolVersion,
    /// Methods explicitly marked as "advertised-but-unimplemented".
    /// Dispatch returns `method_not_found` regardless of method handlers.
    unimplemented: HashSet<String>,
    /// Per-method JSON-in/JSON-out handlers. Keyed by method name.
    handlers: HashMap<String, MethodHandler>,
    /// Call log — every dispatched method name pushed here in order.
    call_log: Mutex<Vec<String>>,
}

impl StubExtension {
    /// Start a fresh builder.
    pub fn builder() -> StubBuilder {
        StubBuilder::default()
    }

    /// Advertised capability set (sorted, cloned for tests).
    pub fn advertised_capabilities(&self) -> Vec<String> {
        let mut v: Vec<String> = self.inner.advertised.iter().cloned().collect();
        v.sort();
        v
    }

    /// Manifest the ext reports to the loader (read-accessor).
    pub fn manifest(&self) -> &ExtensionMetadata {
        &self.inner.manifest
    }

    /// Protocol version the stub reports in handshake.
    pub fn protocol_version(&self) -> ProtocolVersion {
        self.inner.protocol_version
    }

    /// Ordered log of method names dispatched on this stub. Clones
    /// the log so tests can assert without holding the mutex.
    pub fn call_log(&self) -> Vec<String> {
        self.inner
            .call_log
            .lock()
            .expect("call_log poisoned")
            .clone()
    }

    /// Clear the call log — useful between sub-scenarios in one test.
    pub fn clear_call_log(&self) {
        self.inner
            .call_log
            .lock()
            .expect("call_log poisoned")
            .clear();
    }

    /// Common dispatch helper — record the method name, honour the
    /// advertised-but-unimplemented opt-out, then route through the
    /// JSON-erased handler table.
    fn dispatch<Req, Resp>(&self, method: &str, req: Req) -> ExtResult<Resp>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        self.inner
            .call_log
            .lock()
            .expect("call_log poisoned")
            .push(method.to_string());

        if self.inner.unimplemented.contains(method) {
            return Err(ExtensionError::method_not_found(method));
        }

        let Some(handler) = self.inner.handlers.get(method) else {
            return Err(ExtensionError::method_not_found(method));
        };
        let req_val = serde_json::to_value(req)
            .map_err(|e| ExtensionError::Internal(format!("stub: serialize req: {e}")))?;
        let resp_val = handler(req_val)?;
        serde_json::from_value(resp_val)
            .map_err(|e| ExtensionError::Internal(format!("stub: deserialize resp: {e}")))
    }
}

/// Builder for [`StubExtension`].
#[derive(Default)]
pub struct StubBuilder {
    advertised: HashSet<String>,
    manifest: Option<ExtensionMetadata>,
    protocol_version: Option<ProtocolVersion>,
    unimplemented: HashSet<String>,
    handlers: HashMap<String, MethodHandler>,
}

impl StubBuilder {
    /// Replace the capability-advertisement set. Accepts any iterator
    /// of string-ish values.
    pub fn advertise_capabilities<I, S>(mut self, caps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.advertised = caps.into_iter().map(Into::into).collect();
        self
    }

    /// Set the stub's manifest payload.
    pub fn with_manifest(mut self, manifest: ExtensionMetadata) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// Override the protocol version reported in handshake. Defaults to
    /// [`ark_ext_proto::CURRENT_PROTOCOL_VERSION`].
    pub fn with_protocol_version(mut self, v: ProtocolVersion) -> Self {
        self.protocol_version = Some(v);
        self
    }

    /// Mark a method as advertised-but-unimplemented — the stub will
    /// return `method_not_found` regardless of whether a handler was
    /// also registered, allowing tests to exercise the dispatcher's
    /// warn-once + opt-out behaviour.
    pub fn method_advertised_but_unimplemented(mut self, method: impl Into<String>) -> Self {
        self.unimplemented.insert(method.into());
        self
    }

    /// Register a per-method handler.
    ///
    /// The handler receives a typed request and returns a typed
    /// response. The builder stores an erased closure; dispatch
    /// serializes/deserializes through JSON to bridge the dyn-safe
    /// boundary. Handlers are synchronous — the async-trait layer on
    /// [`ArkExtension`] wraps each call, and Phase-2 conformance tests
    /// don't need to model in-flight suspension (kit R1 decision: "sync
    /// closures keep test fixtures tight").
    pub fn with_method<Req, Resp, F>(mut self, method: impl Into<String>, handler: F) -> Self
    where
        Req: serde::de::DeserializeOwned + 'static,
        Resp: serde::Serialize + 'static,
        F: Fn(Req) -> Result<Resp, ExtensionError> + Send + Sync + 'static,
    {
        let method = method.into();
        let erased: MethodHandler = Arc::new(move |req_val| {
            let req: Req = serde_json::from_value(req_val)
                .map_err(|e| ExtensionError::Internal(format!("stub handler: bad req: {e}")))?;
            let resp = handler(req)?;
            serde_json::to_value(resp)
                .map_err(|e| ExtensionError::Internal(format!("stub handler: bad resp: {e}")))
        });
        self.handlers.insert(method, erased);
        self
    }

    /// Finalize the builder.
    pub fn build(self) -> StubExtension {
        let manifest = self.manifest.unwrap_or_else(empty_manifest);
        let protocol_version = self
            .protocol_version
            .unwrap_or(ark_ext_proto::CURRENT_PROTOCOL_VERSION);
        StubExtension {
            inner: Arc::new(StubInner {
                advertised: self.advertised,
                manifest,
                protocol_version,
                unimplemented: self.unimplemented,
                handlers: self.handlers,
                call_log: Mutex::new(Vec::new()),
            }),
        }
    }
}

/// Minimum-viable [`ExtensionMetadata`] — empty collections, empty
/// version ranges, name = `"stub"`. Used when the builder is finalised
/// without a caller-provided manifest.
fn empty_manifest() -> ExtensionMetadata {
    use ark_ext_metadata_types::StringNode;
    ExtensionMetadata {
        name: StringNode::new("stub"),
        version: StringNode::new("0.0.0"),
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
    }
}

#[async_trait::async_trait]
impl ArkExtension for StubExtension {
    /// Handshake override: echo the configured
    /// [`ProtocolVersion`] + advertised capability bag so
    /// [`ark_ext_proto::ExtensionClient::handshake`] can exercise the
    /// version-compat gate (kit R3 — version-mismatch matrix). The
    /// default trait body returns `method_not_found`, which would
    /// prevent the gate from ever seeing the ext's version.
    async fn initialize(&self, _req: InitializeRequest) -> ExtResult<InitializeResponse> {
        // Build the `extension_capabilities` bag from the advertised
        // set. `ark_ext_proto::Capabilities::to_wire` emits the R10
        // object-of-objects shape expected by the handshake round-trip
        // test in `ark-ext-proto/src/transport/mod.rs`.
        let caps = ark_ext_proto::Capabilities::from_iter(self.inner.advertised.iter().cloned());
        Ok(InitializeResponse {
            protocol_version: self.inner.protocol_version.to_wire(),
            extension_capabilities: caps.to_wire(),
            extension_info: r#"{"name":"ark-ext-test-support-stub","version":"0.0.0"}"#.into(),
            session_token: String::new(),
        })
    }

    async fn pane_emit(&self, req: PaneEmitRequest) -> ExtResult<PaneEmitResponse> {
        self.dispatch("pane/emit", req)
    }
    async fn pane_replace_view(
        &self,
        req: PaneReplaceViewRequest,
    ) -> ExtResult<PaneReplaceViewResponse> {
        self.dispatch("pane/replace_view", req)
    }
    async fn pane_close(&self, req: PaneCloseRequest) -> ExtResult<PaneCloseResponse> {
        self.dispatch("pane/close", req)
    }
    async fn stack_spawn_pane(
        &self,
        req: StackSpawnPaneRequest,
    ) -> ExtResult<StackSpawnPaneResponse> {
        self.dispatch("stack/spawn_pane", req)
    }
    async fn stack_close_child(
        &self,
        req: StackCloseChildRequest,
    ) -> ExtResult<StackCloseChildResponse> {
        self.dispatch("stack/close_child", req)
    }
    async fn stack_clear(&self, req: StackClearRequest) -> ExtResult<StackClearResponse> {
        self.dispatch("stack/clear", req)
    }
    async fn on_session_start(
        &self,
        req: OnSessionStartRequest,
    ) -> ExtResult<OnSessionStartResponse> {
        self.dispatch("on_session_start", req)
    }
    async fn on_session_end(&self, req: OnSessionEndRequest) -> ExtResult<OnSessionEndResponse> {
        self.dispatch("on_session_end", req)
    }
    async fn scene_compile_hook(
        &self,
        req: SceneCompileHookRequest,
    ) -> ExtResult<SceneCompileHookResponse> {
        self.dispatch("scene_compile_hook", req)
    }
    async fn control_verbs(&self, req: ControlVerbsRequest) -> ExtResult<ControlVerbsResponse> {
        self.dispatch("control_verbs", req)
    }
    async fn doctor_checks(&self, req: DoctorChecksRequest) -> ExtResult<DoctorChecksResponse> {
        self.dispatch("doctor_checks", req)
    }
    async fn list_columns(&self, req: ListColumnsRequest) -> ExtResult<ListColumnsResponse> {
        self.dispatch("list_columns", req)
    }
}

/// Convenience: a stub that advertises every Phase-2 capability but
/// implements nothing (every call resolves to `method_not_found`).
/// Useful for testing the host dispatcher's opt-out + warn-once path.
pub fn stub_advertising_everything_implementing_nothing() -> StubExtension {
    StubExtension::builder()
        .advertise_capabilities(PHASE_2_CAPABILITY_FLAGS.iter().copied())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper — minimum-arg [`PaneEmitRequest`] for brevity. Payload is
    /// an empty JSON object encoded as a string ([`OpaqueJson`] is a
    /// string alias).
    fn emit_req() -> PaneEmitRequest {
        PaneEmitRequest {
            handle: ark_view::HandleId::new("h"),
            kind: "ev".into(),
            payload: "{}".to_string(),
        }
    }

    #[tokio::test]
    async fn stub_respects_hook_toggle() {
        let a = StubExtension::builder().build();
        let b = StubExtension::builder()
            .with_method("pane/emit", |_req: PaneEmitRequest| {
                Ok(PaneEmitResponse::default())
            })
            .build();

        let ra = a.pane_emit(emit_req()).await;
        let rb = b.pane_emit(emit_req()).await;
        assert!(matches!(ra, Err(ExtensionError::MethodNotFound(_))));
        assert!(rb.is_ok());
    }

    #[tokio::test]
    async fn stub_capability_advertisement_round_trip() {
        let caps = ["view.pane.v1", "ext.lifecycle.v1"];
        let stub = StubExtension::builder().advertise_capabilities(caps).build();
        let advertised = stub.advertised_capabilities();
        assert_eq!(
            advertised,
            vec![
                "ext.lifecycle.v1".to_string(),
                "view.pane.v1".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn stub_manifest_visibility() {
        use ark_ext_metadata_types::{StringNode, ViewDecl};
        let mut manifest = empty_manifest();
        manifest.views.push(ViewDecl {
            name: "panel".into(),
            component: StringNode::new("Panel"),
            kind: Some(StringNode::new("pane")),
        });
        let stub = StubExtension::builder().with_manifest(manifest).build();
        assert_eq!(stub.manifest().views.len(), 1);
        assert_eq!(stub.manifest().views[0].name, "panel");
    }

    #[tokio::test]
    async fn stub_advertised_but_unimplemented_returns_method_not_found() {
        let stub = StubExtension::builder()
            .advertise_capabilities(["view.pane.v1"])
            .method_advertised_but_unimplemented("pane/emit")
            // handler exists but the advertised-but-unimpl marker wins
            .with_method("pane/emit", |_req: PaneEmitRequest| {
                Ok(PaneEmitResponse::default())
            })
            .build();
        let r = stub.pane_emit(emit_req()).await;
        assert!(matches!(r, Err(ExtensionError::MethodNotFound(_))));
    }

    #[tokio::test]
    async fn stub_protocol_version_override() {
        let stub = StubExtension::builder()
            .with_protocol_version(ProtocolVersion::new(1, 0))
            .build();
        assert_eq!(stub.protocol_version(), ProtocolVersion::new(1, 0));
    }

    #[tokio::test]
    async fn stub_call_log_captures_every_dispatch() {
        let stub = StubExtension::builder()
            .with_method("pane/emit", |_req: PaneEmitRequest| {
                Ok(PaneEmitResponse::default())
            })
            .with_method("pane/close", |_req: PaneCloseRequest| {
                Ok(PaneCloseResponse::default())
            })
            .build();
        let h = ark_view::HandleId::new("h");
        let _ = stub
            .pane_emit(PaneEmitRequest {
                handle: h.clone(),
                kind: "e".into(),
                payload: "{}".to_string(),
            })
            .await;
        let _ = stub.pane_close(PaneCloseRequest { handle: h }).await;
        let log = stub.call_log();
        assert_eq!(
            log,
            vec!["pane/emit".to_string(), "pane/close".to_string()]
        );
    }

    #[tokio::test]
    async fn stub_advertising_everything_implementing_nothing_has_full_slate() {
        let stub = stub_advertising_everything_implementing_nothing();
        let caps = stub.advertised_capabilities();
        assert_eq!(caps.len(), PHASE_2_CAPABILITY_FLAGS.len());
    }

    #[tokio::test]
    async fn stub_clear_call_log_resets_the_log() {
        let stub = StubExtension::builder()
            .with_method("pane/emit", |_req: PaneEmitRequest| {
                Ok(PaneEmitResponse::default())
            })
            .build();
        let _ = stub.pane_emit(emit_req()).await;
        assert_eq!(stub.call_log().len(), 1);
        stub.clear_call_log();
        assert_eq!(stub.call_log().len(), 0);
    }
}
