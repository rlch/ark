//! Process-global wasmtime `Engine` singleton.
//!
//! T-PP-025 / T-PP-026 / T-PP-027 (cavekit-plugin-protocol R1).
//!
//! ark-host owns exactly one `wasmtime::Engine` per process. The engine is
//! constructed in a single place (`engine()`) from a deterministic `Config`
//! so that the required feature flags cannot silently drift:
//!
//! * `wasm_component_model(true)` — plugins are component-model components.
//! * `async_support(true)`        — the host integrates wasmtime with tokio
//!   (the method is a deprecated no-op in wasmtime 43 because the
//!   `async` cargo feature already enables async support; we call it
//!   explicitly anyway so the invariant is visible in source).
//! * `epoch_interruption(true)`   — cooperative yield is epoch-based
//!   (cluster 3 §3.2 — ark needs liveness, not determinism).
//! * `consume_fuel(false)`        — fuel is explicitly OFF. The default is
//!   already `false`; we document it here so the invariant is obvious.
//!
//! Any other construction path in `ark-host/src/` is forbidden — the
//! `lint_forbidden_apis.rs` integration test (T-PP-030) greps `src/` for
//! core-wasmtime identifiers that would let a rogue code path assemble
//! an unsafe engine.
//!
//! # Epoch ticker
//!
//! `start_epoch_ticker()` spawns a dedicated OS thread (NOT a tokio task
//! — the ticker must run independently of whether the async runtime is
//! parked) that calls `Engine::increment_epoch` every ~50 ms. Together
//! with the per-`Store` `set_epoch_deadline(2)` +
//! `epoch_deadline_async_yield_and_update(2)` set in `store::new_store`,
//! this guarantees plugins yield cooperatively every ~100 ms.
//!
//! The ticker thread is intentionally leaked on process exit — it holds a
//! `&'static Engine` reference and has no state to flush; the OS reaps it
//! on `exit(2)`. This is fine for v1.

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::{Config, Engine};

/// Process-global engine. Constructed exactly once by [`engine`].
static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Counter incremented every time the engine construction closure runs.
///
/// `OnceLock::get_or_init` guarantees the closure runs at most once even
/// under thread contention; this counter lets the T-PP-026 integration
/// test verify that invariant at runtime (not just by reading docs).
pub(crate) static ENGINE_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Counter of epoch ticks fired by the background ticker thread.
///
/// Incremented once per `Engine::increment_epoch` call in the ticker
/// loop. Exposed to tests via [`ticks_seen`] so T-PP-027 can assert the
/// ticker actually runs — wasmtime does not expose a public read path
/// for its internal epoch counter, so we mirror it here.
pub(crate) static TICKS_SEEN: AtomicU64 = AtomicU64::new(0);

/// The ticker's join handle. Stored so we can enforce "exactly one
/// ticker per process" via `OnceLock::set`'s atomic first-writer-wins
/// semantics. Intentionally never joined (see module doc).
static TICKER_HANDLE: OnceLock<JoinHandle<()>> = OnceLock::new();

/// The interval between ticker calls to `Engine::increment_epoch`.
///
/// `set_epoch_deadline(2)` + `epoch_deadline_async_yield_and_update(2)`
/// turns a 50 ms tick into a ~100 ms maximum guest compute slice.
pub(crate) const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(50);

/// Returns the process-global [`Engine`], constructing it on the first
/// call.
///
/// See module-level docs for the exact feature-flag invariants. All
/// subsequent calls return a reference to the same `Engine` — it is
/// internally ref-counted (`Clone + Send + Sync`) so callers may clone
/// the returned reference into their own `Store` construction without
/// allocating a new engine.
pub fn engine() -> &'static Engine {
    ENGINE.get_or_init(build_engine)
}

/// Builds the one-and-only `Engine`.
///
/// Kept as a private function (rather than inlined into the closure) so
/// the code path producing the `Config` is visible as a single
/// named symbol under `goToDefinition`. Any rogue call-site trying to
/// construct its own `Engine` would have to reference `wasmtime::Engine`
/// in `ark-host/src/`, which the forbidden-API lint refuses to accept
/// outside this file.
fn build_engine() -> Engine {
    ENGINE_INIT_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut config = Config::new();
    #[allow(deprecated)] // wasmtime 43: async_support is a no-op because
    // the `async` cargo feature already enables it. We still call it so
    // the invariant is explicit in source (and so a future wasmtime bump
    // that un-deprecates the method keeps working unchanged).
    config.async_support(true);
    config
        .wasm_component_model(true)
        .epoch_interruption(true)
        .consume_fuel(false);
    Engine::new(&config)
        .expect("wasmtime Engine::new must succeed with component-model + async + epoch")
}

/// Spawns a background OS thread that ticks the engine's epoch every
/// [`EPOCH_TICK_INTERVAL`]. Idempotent — repeated calls are no-ops after
/// the first.
///
/// A plain OS thread is used rather than a tokio task so the ticker runs
/// even when the runtime is parked or between runtime instances (e.g.
/// integration tests that tear down and rebuild their `tokio::Runtime`).
///
/// The thread runs for the lifetime of the process and is intentionally
/// leaked on exit — it holds only a `&'static Engine` reference and has
/// no state to flush.
pub fn start_epoch_ticker() {
    // Ensure engine() is constructed before the ticker tries to use it,
    // so the ticker thread never races the first engine() caller.
    let _ = engine();
    let _ = TICKER_HANDLE.get_or_init(|| {
        std::thread::Builder::new()
            .name("ark-host-epoch-ticker".into())
            .spawn(|| {
                let engine = engine();
                loop {
                    std::thread::sleep(EPOCH_TICK_INTERVAL);
                    engine.increment_epoch();
                    TICKS_SEEN.fetch_add(1, Ordering::Relaxed);
                }
            })
            .expect("spawn ark-host-epoch-ticker thread")
    });
}

/// Number of epoch ticks the background ticker has fired since process
/// start. Exposed to tests only (crate-visible) so T-PP-027 can assert
/// the ticker is actually running without needing a wasmtime API that
/// exposes the internal epoch counter.
#[doc(hidden)]
pub fn ticks_seen() -> u64 {
    TICKS_SEEN.load(Ordering::Relaxed)
}

/// Number of times the `Engine` construction closure has run.
/// Crate-visible only — used by T-PP-026 singleton test.
#[doc(hidden)]
pub fn engine_init_count() -> usize {
    ENGINE_INIT_COUNT.load(Ordering::Relaxed)
}
