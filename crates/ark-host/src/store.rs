//! Per-plugin `Store<PluginCtx>` factory + `PluginCtx` struct.
//!
//! T-PP-028 / T-PP-029 (cavekit-plugin-protocol R1, R4, R8).
//!
//! Each loaded plugin owns exactly one `wasmtime::Store<PluginCtx>`. The
//! `PluginCtx` carries:
//!
//! * a `WasiCtx` built via the default-deny [`default_deny_wasi`]
//!   helper (no preopens, no env, no args, stdio muted, TCP/UDP and
//!   DNS all refused — cluster 3 §3.3 footgun);
//! * a `ResourceTable` for WASI Preview 2 resource handles;
//! * the plugin's identity (`plugin_id`) and granted capability set
//!   (`granted_caps`), which the Tier 3B host-fn impls will consult in
//!   their in-fn fine-grain checks (cluster 3 §3.2 approach B);
//! * a `LogSink` trait object that the Tier 3B `ark:host/log` impl
//!   will fan messages into (concrete `TracingLogSink` lands with
//!   T-PP-032).
//!
//! At construction, the factory sets `set_epoch_deadline(2)` and
//! `epoch_deadline_async_yield_and_update(2)` so plugins yield
//! cooperatively every ~100 ms given the 50 ms engine epoch ticker
//! (see `engine.rs`).

use std::collections::BTreeSet;

use wasmtime::Store;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::engine::engine;

/// Sink trait for plugin-emitted log messages.
///
/// The `ark:host/log` host-fn implementation (Tier 3B — T-PP-032) will
/// route plugin log records into this sink. Kept as a trait object so
/// tests can substitute an in-memory sink without pulling in
/// `tracing-subscriber` wiring. `Send + Sync` is required because
/// wasmtime may run guest code on a different worker thread than the
/// one that created the `Store` (under `async` execution).
pub trait LogSink: Send + Sync {
    /// Emit a log record. `level` is a stringly-typed severity (e.g.
    /// `"info"` / `"warn"`) until T-PP-032 lands a typed enum in the
    /// WIT world.
    fn log(&self, level: &str, target: &str, message: &str);
}

/// Default sink that silently drops records — used as a safe fallback
/// when no tracing subscriber is wired up yet.
pub struct NullLogSink;

impl LogSink for NullLogSink {
    fn log(&self, _level: &str, _target: &str, _message: &str) {}
}

/// Per-`Store` plugin state.
///
/// One `PluginCtx` per loaded plugin, owned by exactly one `Store`.
/// `Store<PluginCtx>` is `!Sync` (wasmtime invariant) so a `PluginCtx`
/// never crosses thread boundaries without going through the store
/// handle.
pub struct PluginCtx {
    /// WASI Preview 2 context. Built via [`default_deny_wasi`] and
    /// tightened per granted cap by Tier 3B T-PP-035 (cap-to-WasiCtx
    /// mapping). Never mutated in-place after construction.
    pub wasi: WasiCtx,
    /// Resource table for WASI Preview 2 resource handles (files,
    /// sockets, streams). Required by the `WasiView` trait impl so
    /// bindgen-generated host-fn glue can allocate / look up resources.
    pub resource_table: ResourceTable,
    /// Stable plugin identity — matches the `name` field in the plugin's
    /// `ark-meta:v1` custom section (cavekit-plugin-protocol R9). Used
    /// by host-fn impls (e.g. `ark:host/plugin-id`) and log target
    /// decoration.
    pub plugin_id: String,
    /// Set of capability ids this plugin has been granted by the user
    /// via `ark.kdl` (cavekit-plugin-protocol R5). The coarse gate is
    /// the per-cap-profile linker variant (R4, Tier 3B T-PP-034); this
    /// field backs the fine-gate in-fn checks (cluster 3 §3.2 approach B).
    ///
    /// `BTreeSet` — small, deterministic iteration for deterministic
    /// error messages on "cap not granted" denials.
    pub granted_caps: BTreeSet<String>,
    /// Sink for plugin-emitted log records. See [`LogSink`].
    pub log_sink: Box<dyn LogSink>,
}

impl PluginCtx {
    /// Convenience constructor — builds a `PluginCtx` around a
    /// pre-built `WasiCtx`.
    pub fn new(
        wasi: WasiCtx,
        plugin_id: impl Into<String>,
        granted_caps: BTreeSet<String>,
        log_sink: Box<dyn LogSink>,
    ) -> Self {
        Self {
            wasi,
            resource_table: ResourceTable::new(),
            plugin_id: plugin_id.into(),
            granted_caps,
            log_sink,
        }
    }
}

impl WasiView for PluginCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.resource_table,
        }
    }
}

/// Builds a default-deny `WasiCtx` per cavekit-plugin-protocol R1.
///
/// Invariants enforced here (cluster 3 §3.3 footgun — defaults for
/// sockets are *permissive*, so the only safe path is to construct the
/// `WasiCtx` exclusively through this helper):
///
/// | Capability           | State     | Why                                |
/// |----------------------|-----------|------------------------------------|
/// | `allow_tcp`          | `false`   | No outbound TCP unless `network`   |
/// |                      |           | cap is granted (T-PP-035).         |
/// | `allow_udp`          | `false`   | Ditto.                             |
/// | `allow_ip_name_lookup` | `false` | DNS is its own cap leg.            |
/// | preopens             | `[]`      | `fs-read` / `fs-write` add them.   |
/// | env                  | `[]`      | Plugins don't inherit process env. |
/// | args                 | `[]`      | No argv to a plugin.               |
/// | stdin / stdout / stderr | muted  | wasmtime-wasi defaults mute these  |
/// |                      |           | (stdin closed, stdout/stderr eat). |
///
/// Per-cap profile wiring (e.g. `fs-read` adding a preopen, `network`
/// re-enabling TCP) is layered on TOP of this baseline by the Tier 3B
/// `cap_to_wasi` mapping (T-PP-035); the baseline is what you get when
/// the user grants zero caps.
///
/// The wasmtime-wasi 43 `WasiCtxBuilder::new` defaults already match
/// everything except `allow_tcp` / `allow_udp` — those default *true*
/// at the builder level (with all addresses denied in the default
/// socket-addr check). We still call the `allow_tcp(false)` /
/// `allow_udp(false)` methods explicitly so the default-deny posture is
/// a syntactic invariant — `grep allow_tcp ark-host/src/` will find the
/// exact line that enforces it.
pub fn default_deny_wasi() -> WasiCtx {
    let mut b = WasiCtxBuilder::new();
    b.allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false);
    // NOTE: we do NOT call any of `inherit_stdin` / `inherit_stdout` /
    // `inherit_stderr` / `inherit_env` / `inherit_args`. The builder's
    // defaults already mute stdin (closed) and stdout/stderr (eat), and
    // leave env + args empty. We also never call `preopened_dir` —
    // preopens are added by the per-cap wiring in T-PP-035, never here.
    b.build()
}

/// Builds a fresh `Store<PluginCtx>` bound to the process-global engine,
/// with epoch deadlines configured for cooperative yielding.
///
/// Combined with the 50 ms epoch ticker (`engine::start_epoch_ticker`),
/// `set_epoch_deadline(2)` + `epoch_deadline_async_yield_and_update(2)`
/// gives guests a maximum ~100 ms compute slice between yields.
pub fn new_store(ctx: PluginCtx) -> Store<PluginCtx> {
    let mut store = Store::new(engine(), ctx);
    store.set_epoch_deadline(2);
    store.epoch_deadline_async_yield_and_update(2);
    store
}

/// Convenience factory: build a default-deny `PluginCtx` + `Store` in
/// one call. Used by the Tier 3B/C loader to hand out a ready-to-run
/// store after per-cap WASI wiring has been applied by the caller.
pub fn new_default_deny_store(
    plugin_id: impl Into<String>,
    granted_caps: BTreeSet<String>,
    log_sink: Box<dyn LogSink>,
) -> Store<PluginCtx> {
    let ctx = PluginCtx::new(default_deny_wasi(), plugin_id, granted_caps, log_sink);
    new_store(ctx)
}
