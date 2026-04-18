//! Extension load sequence orchestrator (T-030).
//!
//! Implements the 7-step pipeline from `cavekit-soul-phase-2-host-
//! dispatch.md` R8: (1) read manifest, (2) handshake, (3) validate
//! capabilities, (4) register intents, (5) register views, (6) register
//! gates, (7) Ready. Each step emits a structured `tracing` event on
//! `target = "ark.supervisor.ext_loader"` so step ordering can be
//! verified by log capture in tests.
//!
//! T-030 abstracts over transport — the handshake closure is supplied
//! by the caller so production can plug in the ndjson subprocess
//! client while tests use an in-proc stub. Registration for intents,
//! views, and reload gates is similarly abstracted (IntentRegistry
//! lives scene-side; view table + reload gate set aren't yet fully
//! wired in supervisor). What this module does own is the *sequence*
//! and the *tracing spans* that let the step-order test pass.
//!
//! A failed extension returns `Err` from [`load_extension`] but does
//! NOT halt the caller's loop in [`load_extensions`] — per R8
//! "failed exts don't block peers".

use ark_ext_metadata_types::ExtensionMetadata;
use std::future::Future;

/// Outcome of loading one extension. Holds the per-extension state the
/// caller needs to drive runtime RPC dispatch (currently the
/// advertised capability list, which is also recorded into the
/// crate-local [`crate::ext_dispatch`] registry).
#[derive(Debug, Clone)]
pub struct LoadedExtension {
    /// Extension name (from `metadata.name.value`).
    pub name: String,
    /// Capabilities the ext advertised in its `InitializeResponse`.
    /// Recorded into [`crate::ext_dispatch::record_capabilities`] as
    /// part of step 3.
    pub advertised_capabilities: Vec<String>,
}

/// Errors the loader may emit. Failed extensions surface as `Err` here
/// and the caller is expected to continue with the remaining peers.
#[derive(Debug, thiserror::Error)]
pub enum ExtLoadError {
    /// Manifest parse error. Step 1 ingests a *pre-parsed*
    /// [`ExtensionMetadata`] so this variant only fires when a caller
    /// pre-validates the manifest and wants to surface that failure
    /// through the same error channel.
    #[error("manifest parse error for {ext_name}: {source}")]
    Manifest {
        ext_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Step 2 failure — the handshake closure returned `Err`.
    #[error("handshake failed for {ext_name}: {source}")]
    Handshake {
        ext_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Step 3 failure. Currently unused (unknown caps only *warn*), but
    /// reserved so future stricter validation can return a hard error
    /// without a new variant.
    #[error("capability validation failed for {ext_name}: {reason}")]
    Capability { ext_name: String, reason: String },

    /// Step 4 failure.
    #[error("intent registration failed for {ext_name}: {source}")]
    IntentReg {
        ext_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Step 5 failure.
    #[error("view registration failed for {ext_name}: {source}")]
    ViewReg {
        ext_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// Step 6 failure.
    #[error("gate registration failed for {ext_name}: {source}")]
    GateReg {
        ext_name: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// Handshake closure type.
///
/// Given the host's advertised capability slate (as `&[&str]`), the
/// closure performs the transport-specific InitializeRequest round-trip
/// and returns the extension's advertised capabilities on success.
///
/// Boxed future lets production use an `async` ndjson client while
/// tests use a trivial `async { Ok(vec![]) }` stub.
pub type HandshakeFn<'a> = Box<
    dyn FnOnce(
            &[&'static str],
        ) -> std::pin::Pin<
            Box<
                dyn Future<
                        Output = Result<
                            Vec<String>,
                            Box<dyn std::error::Error + Send + Sync + 'static>,
                        >,
                    > + Send
                    + 'a,
            >,
        > + Send
        + 'a,
>;

/// Run the 7-step load sequence for a single extension. Each step
/// emits a `tracing::info!` event on `target = "ark.supervisor.ext_
/// loader"` with structured fields `ext`, `step`, `step_index`.
///
/// Step summary (R8):
///   1. manifest_read        — metadata already parsed (caller owns I/O)
///   2. handshake            — `InitializeRequest` round-trip
///   3. validate_capabilities — warn on advertised-but-unknown flags;
///                             record advertised caps into `ext_dispatch`
///   4. register_intents     — caller wires `IntentRegistry`
///   5. register_views       — caller wires view table
///   6. register_gates       — caller wires reload gate set
///   7. ready                — extension is live
///
/// Only step 2 can currently fail in this scaffold; steps 4-6 emit
/// their tracing events as anchor points for future wiring.
pub async fn load_extension<'a>(
    metadata: ExtensionMetadata,
    handshake: HandshakeFn<'a>,
) -> Result<LoadedExtension, ExtLoadError> {
    let ext_name = metadata.name.value.clone();

    // Step 1: manifest already parsed (caller read it from disk).
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "manifest_read",
        step_index = 1,
        "step 1: manifest read"
    );

    // Step 2: handshake.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "handshake",
        step_index = 2,
        "step 2: handshake"
    );
    let host_caps = crate::host_capabilities::HOST_PHASE_2_CAPABILITIES;
    let advertised = handshake(host_caps)
        .await
        .map_err(|e| ExtLoadError::Handshake {
            ext_name: ext_name.clone(),
            source: e,
        })?;

    // Step 3: validate caps (warn on unknown; don't fail). Record the
    // advertised set into the dispatch registry so later RPC calls are
    // gated by `should_dispatch`.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "validate_capabilities",
        step_index = 3,
        "step 3: validate capabilities"
    );
    for cap in &advertised {
        if !host_caps.contains(&cap.as_str()) {
            tracing::warn!(
                target: "ark.supervisor.ext_loader",
                ext = %ext_name,
                cap = %cap,
                "ext advertised unknown capability"
            );
        }
    }
    crate::ext_dispatch::record_capabilities(&ext_name, advertised.iter().cloned());

    // Step 4: register intents. Registration itself is scene-side; the
    // tracing event is the anchor for the caller-supplied wiring.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "register_intents",
        step_index = 4,
        intent_count = metadata.intents.len(),
        "step 4: register intents"
    );

    // Step 5: register views.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "register_views",
        step_index = 5,
        view_count = metadata.views.len(),
        "step 5: register views"
    );

    // Step 6: register reload gates.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "register_gates",
        step_index = 6,
        gate_count = metadata.reload_gates.len(),
        "step 6: register gates"
    );

    // Step 7: Ready.
    tracing::info!(
        target: "ark.supervisor.ext_loader",
        ext = %ext_name,
        step = "ready",
        step_index = 7,
        "step 7: ready"
    );

    Ok(LoadedExtension {
        name: ext_name,
        advertised_capabilities: advertised,
    })
}

/// Load a batch of extensions. Each extension runs the sequence
/// independently; one extension's failure does NOT halt peers (R8).
/// The returned `Vec<Result<_, _>>` preserves per-ext outcomes in
/// input order so the caller can diagnose partial failures.
pub async fn load_extensions<I>(extensions: I) -> Vec<Result<LoadedExtension, ExtLoadError>>
where
    I: IntoIterator<Item = (ExtensionMetadata, HandshakeFn<'static>)>,
{
    let mut results = Vec::new();
    for (meta, handshake) in extensions {
        let name = meta.name.value.clone();
        let r = load_extension(meta, handshake).await;
        if r.is_err() {
            tracing::warn!(
                target: "ark.supervisor.ext_loader",
                ext = %name,
                "extension load failed; continuing with peers"
            );
        }
        results.push(r);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode};
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::Registry;

    /// Build a minimal metadata value for one extension. The facet-kdl
    /// fields we don't care about in these tests default to empty.
    fn minimal_metadata(name: &str) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::new(">= 0.1"),
            zellij_range: StringNode::new(""),
            requires: Vec::new(),
            intents: Vec::new(),
            events: Vec::new(),
            config: ConfigSchema::default(),
            views: Vec::new(),
            capabilities: CapabilitySet::default(),
            config_sections: Vec::new(),
            reload_gates: Vec::new(),
        }
    }

    /// Happy-path handshake — advertises exactly the caps supplied.
    fn ok_handshake(caps: Vec<String>) -> HandshakeFn<'static> {
        Box::new(move |_host_caps: &[&'static str]| Box::pin(async move { Ok(caps) }))
    }

    /// Failing handshake — returns a boxed error unconditionally.
    fn err_handshake(message: &'static str) -> HandshakeFn<'static> {
        Box::new(move |_host_caps: &[&'static str]| {
            Box::pin(async move {
                let err: Box<dyn std::error::Error + Send + Sync + 'static> = Box::from(message);
                Err(err)
            })
        })
    }

    // ------------------------------------------------------------------
    // Tracing capture layer — collects (step_index, step, ext) from
    // every event on `target = "ark.supervisor.ext_loader"` so tests
    // can assert ordering.
    // ------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct CapturedEvent {
        step_index: Option<u64>,
        step: Option<String>,
        ext: Option<String>,
        level: String,
    }

    struct CaptureVisitor<'a> {
        out: &'a mut CapturedEvent,
    }

    impl<'a> Visit for CaptureVisitor<'a> {
        fn record_u64(&mut self, field: &Field, value: u64) {
            if field.name() == "step_index" {
                self.out.step_index = Some(value);
            }
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            if field.name() == "step_index" && value >= 0 {
                self.out.step_index = Some(value as u64);
            }
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "step" => self.out.step = Some(value.to_string()),
                "ext" => self.out.ext = Some(value.to_string()),
                _ => {}
            }
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            // `ext = %ext_name` records through Display but `step = "…"`
            // may come through Debug depending on how the macro expands
            // — capture both to be safe. We strip surrounding quotes
            // because Debug-formatting a &str yields `"foo"`.
            let rendered = format!("{value:?}");
            let stripped = rendered.trim_matches('"').to_string();
            match field.name() {
                "step" => {
                    if self.out.step.is_none() {
                        self.out.step = Some(stripped);
                    }
                }
                "ext" => {
                    if self.out.ext.is_none() {
                        self.out.ext = Some(stripped);
                    }
                }
                _ => {}
            }
        }
    }

    #[derive(Clone, Default)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        /// Force every callsite to be considered enabled. Without this
        /// override, tracing-core's per-callsite Interest cache may
        /// have been populated by a *previous* global subscriber (e.g.
        /// another test in the suite that installed one via
        /// `set_global_default`), causing our thread-local
        /// `with_default` subscriber to never observe the event.
        /// Returning `Interest::always()` forces `Dispatch::enabled` to
        /// run every time, so our CaptureLayer gets called.
        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn enabled(&self, _metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
            true
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let target = event.metadata().target().to_string();
            if target != "ark.supervisor.ext_loader" {
                return;
            }
            let _ = target;
            let mut captured = CapturedEvent {
                level: event.metadata().level().to_string(),
                ..Default::default()
            };
            let mut visitor = CaptureVisitor { out: &mut captured };
            event.record(&mut visitor);
            self.events.lock().unwrap().push(captured);
        }
    }

    /// Synchronous block-on for test fixtures. We intentionally avoid
    /// `tokio::runtime` here — tokio swaps the thread-local tracing
    /// dispatcher across its `block_on` boundary, which breaks the
    /// [`tracing::subscriber::with_default`] scope we need for the
    /// step-order capture test. `futures::executor::block_on` runs the
    /// future on the current thread without touching the dispatcher.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    /// (1) Happy path: minimal metadata + passing handshake -> sequence
    /// completes and records caps via the dispatch registry.
    #[test]
    fn happy_path_completes_and_records_caps() {
        let name = "test-ext-happy";
        let meta = minimal_metadata(name);
        let result = block_on(load_extension(
            meta,
            ok_handshake(vec!["view.pane.v1".into()]),
        ));
        let loaded = result.expect("load_extension should succeed");
        assert_eq!(loaded.name, name);
        assert_eq!(loaded.advertised_capabilities, vec!["view.pane.v1"]);
        // Capability registered: pane/emit is gated on view.pane.v1.
        assert!(crate::ext_dispatch::should_dispatch(name, "pane/emit"));
        // And a cap the ext did NOT advertise is rejected.
        assert!(!crate::ext_dispatch::should_dispatch(name, "stack/clear"));
    }

    /// (2) Handshake error -> ExtLoadError::Handshake.
    #[test]
    fn handshake_error_returns_handshake_variant() {
        let meta = minimal_metadata("test-ext-handshake-fail");
        let err = block_on(load_extension(meta, err_handshake("boom")))
            .expect_err("expected handshake failure");
        match err {
            ExtLoadError::Handshake { ext_name, .. } => {
                assert_eq!(ext_name, "test-ext-handshake-fail");
            }
            other => panic!("expected Handshake variant, got {other:?}"),
        }
    }

    /// (3) Ext advertises a capability NOT in the host slate: sequence
    /// still completes (unknown cap only warns — kit R8 "failed exts
    /// don't BLOCK peers"; warn is OK).
    #[test]
    fn unknown_advertised_capability_warns_but_completes() {
        let meta = minimal_metadata("test-ext-unknown-cap");
        let result = block_on(load_extension(
            meta,
            ok_handshake(vec!["totally.made.up.v9".into()]),
        ));
        let loaded = result.expect("unknown cap must not abort the sequence");
        assert_eq!(loaded.advertised_capabilities, vec!["totally.made.up.v9"]);
    }

    /// (4) `load_extensions` with one ok + one failing ext returns a
    /// Vec of length 2; peers are not blocked by the failure.
    #[test]
    fn load_extensions_continues_past_failures() {
        let batch: Vec<(ExtensionMetadata, HandshakeFn<'static>)> = vec![
            (
                minimal_metadata("batch-ok"),
                ok_handshake(vec!["ext.doctor.v1".into()]),
            ),
            (
                minimal_metadata("batch-fail"),
                err_handshake("simulated transport fault"),
            ),
        ];
        let results = block_on(load_extensions(batch));
        assert_eq!(results.len(), 2, "every peer must produce a result");
        assert!(results[0].is_ok(), "first peer should succeed");
        assert!(
            matches!(results[1], Err(ExtLoadError::Handshake { .. })),
            "second peer should surface Handshake failure",
        );
    }

    // Shared capture store for the step-order test. Using a global
    // default subscriber (rather than thread-local `with_default`)
    // sidesteps the tracing-core callsite Interest cache issue: with
    // parallel tests in the same process, a callsite that fires first
    // under `NoSubscriber` caches Interest::never, and later
    // thread-local dispatchers never see those callsites. Installing
    // the CaptureLayer globally means every callsite registers against
    // it from the start.
    static GLOBAL_CAPTURE_INIT: std::sync::Once = std::sync::Once::new();
    static GLOBAL_CAPTURE_EVENTS: std::sync::OnceLock<Arc<Mutex<Vec<CapturedEvent>>>> =
        std::sync::OnceLock::new();

    fn install_global_capture() -> Arc<Mutex<Vec<CapturedEvent>>> {
        let events = GLOBAL_CAPTURE_EVENTS
            .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
            .clone();
        GLOBAL_CAPTURE_INIT.call_once(|| {
            let layer = CaptureLayer {
                events: events.clone(),
            };
            let subscriber = Registry::default().with(layer);
            // If another test has already set a global subscriber,
            // set_global_default returns Err; in that case we fall
            // back to the existing one and this test may over-capture
            // events from other ext_loader tests. That's fine — we
            // scope assertions by `ext` name below.
            let _ = tracing::subscriber::set_global_default(subscriber);
            // Rebuild so any pre-registered callsites see the new
            // dispatcher.
            tracing::callsite::rebuild_interest_cache();
        });
        events
    }

    /// (5) Step-order test via a tracing capture layer. Filter on
    /// `target = "ark.supervisor.ext_loader"` + `ext = "step-order-
    /// ext"` and assert step_index values 1..=7 appear in order for
    /// a single successful load.
    #[test]
    fn step_order_is_one_through_seven() {
        let events = install_global_capture();

        // Snapshot current event count so we only consider events
        // emitted by *this* test — other ext_loader tests may have
        // written into the shared buffer.
        let start = events.lock().unwrap().len();

        let meta = minimal_metadata("step-order-ext");
        block_on(load_extension(meta, ok_handshake(vec![]))).expect("load should succeed");

        let events = events.lock().unwrap();
        let step_indices: Vec<u64> = events
            .iter()
            .skip(start)
            .filter(|e| e.ext.as_deref() == Some("step-order-ext"))
            .filter_map(|e| e.step_index)
            .collect();
        assert_eq!(
            step_indices,
            vec![1, 2, 3, 4, 5, 6, 7],
            "steps must fire in order 1..=7; captured: {:?}",
            events
                .iter()
                .skip(start)
                .map(|e| (e.level.clone(), e.ext.clone(), e.step.clone(), e.step_index))
                .collect::<Vec<_>>(),
        );
    }
}
