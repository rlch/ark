//! Unconditional `ark:host/*` host-function impls.
//!
//! T-PP-032 (cavekit-plugin-protocol R2): implements the three host
//! interfaces every plugin sees regardless of granted capabilities:
//!
//! * [`ark:host/log`](crate::bindings::ark::plugin::log) — routes guest
//!   log records into the host's `tracing` subscriber. The WIT surface
//!   in v1 is a single `log(message: string)` — the severity split
//!   (`info` / `warn` / `debug` / `error`) documented in the task
//!   packet is deferred until the WIT interface exposes a typed level
//!   enum (post-v1 MINOR); for now every message is emitted at
//!   `tracing::Level::INFO` via the plugin's `log_sink`.
//! * [`ark:host/clock`](crate::bindings::ark::plugin::clock) — monotonic
//!   nanoseconds from `std::time::Instant`. The first call establishes
//!   a per-plugin epoch stored in [`PluginCtx::monotonic_epoch`] so
//!   subsequent calls can return a u64 that fits comfortably in
//!   `Duration::from_nanos`.
//! * [`ark:host/plugin-id`](crate::bindings::ark::plugin::plugin_id) —
//!   clones [`PluginCtx::plugin_id`].
//!
//! Each fn returns `wasmtime::Result<T>` — a failure is a nominal error
//! return to the guest, not a trap. The host-fn body itself must never
//! panic; every plausible runtime failure (e.g. a clock that ticked
//! backwards) is wrapped in a `wasmtime::Error` via `anyhow!`.

use wasmtime::Error;
use wasmtime::component::Resource;

use crate::bindings::ark::plugin::{
    clock, log, plugin_id, types,
    widget_tree_types::{self, HostTerminalNode, TerminalNode, TerminalWidgetTree},
};
use crate::store::PluginCtx;

impl log::Host for PluginCtx {
    async fn log(&mut self, message: String) -> wasmtime::Result<()> {
        // Route through the per-plugin `LogSink` trait object (set by
        // the loader) AND mirror to the process tracing subscriber so
        // plugin messages show up in the same log stream as host
        // messages, decorated with the plugin-id as a `target`.
        self.log_sink.log("info", &self.plugin_id, &message);
        tracing::info!(target: "ark_host::plugin", plugin = %self.plugin_id, "{message}");
        Ok(())
    }
}

impl clock::Host for PluginCtx {
    async fn now_ns(&mut self) -> wasmtime::Result<u64> {
        // The first call per-plugin stamps the epoch and returns 0.
        // Subsequent calls return the delta in nanoseconds. This keeps
        // the u64 monotonic-ns counter small (worst case ~585 years of
        // wall-clock time before wraparound, which is fine).
        let now = std::time::Instant::now();
        let epoch = *self.monotonic_epoch.get_or_insert(now);
        let delta = now.checked_duration_since(epoch).ok_or_else(|| {
            Error::msg("ark:host/clock: monotonic instant went backwards — clock violation")
        })?;
        Ok(delta.as_nanos().min(u64::MAX as u128) as u64)
    }
}

impl plugin_id::Host for PluginCtx {
    async fn id(&mut self) -> wasmtime::Result<String> {
        Ok(self.plugin_id.clone())
    }
}

// ----------------------------------------------------------------
// `types` and `widget-tree-types` helper interfaces.
// ----------------------------------------------------------------
//
// These are type-only interfaces (per kit R2's ALLOWED_HELPER_INTERFACES
// partition in `ark-plugin-protocol/build.rs`). bindgen still requires
// a `Host` impl on `PluginCtx` because the generated `add_to_linker`
// wires them through. The `types::Host` impl is empty (no resources,
// no free fns); `widget_tree_types::Host` is empty too but carries the
// `HostTerminalNode` supertrait — the resource methods wire the owned
// `TerminalWidgetTree` payload through the plugin's `ResourceTable`.

impl types::Host for PluginCtx {}

impl widget_tree_types::Host for PluginCtx {}

/// Host-side wrapper body for the `terminal-node` WIT resource. Stored
/// in the `ResourceTable` keyed by `Resource<TerminalNode>` — the
/// `ResourceTable::push` return type is `Resource<U>` where `U` is the
/// body type, so we keep body = wrapped variant here.
///
/// The WIT resource exists purely to break the type-graph cycle in
/// `widget-tree.wit` (see that file's preamble) — it is not semantic
/// ownership machinery, just a workaround for wit-parser 0.245's
/// toposort.
struct TerminalNodeBody(TerminalWidgetTree);

impl HostTerminalNode for PluginCtx {
    async fn new(
        &mut self,
        tree: TerminalWidgetTree,
    ) -> wasmtime::Result<Resource<TerminalNode>> {
        // Push the tree body onto the per-plugin `ResourceTable` and
        // re-type the returned resource index as `Resource<TerminalNode>`
        // — wasmtime's `Resource<T>` is a phantom-typed index, so the
        // bitwise conversion is safe as long as we consistently store
        // `TerminalNodeBody` for this resource across new/tree/drop.
        let res = self.resource_table.push(TerminalNodeBody(tree))?;
        // Re-tag via u32 round-trip — the two phantom types share the
        // same underlying index representation.
        Ok(Resource::new_own(res.rep()))
    }

    async fn tree(
        &mut self,
        self_: Resource<TerminalNode>,
    ) -> wasmtime::Result<TerminalWidgetTree> {
        // Consume the resource — taking the payload out. The guest's
        // subsequent `drop` call on the same handle is a no-op
        // because `ResourceTable` surfaces a "not found" which
        // bindgen's `drop` glue tolerates. v1 semantics: `tree()`
        // transfers ownership of the inner payload to the caller.
        let body: TerminalNodeBody = self
            .resource_table
            .delete(Resource::<TerminalNodeBody>::new_own(self_.rep()))?;
        Ok(body.0)
    }

    async fn drop(&mut self, rep: Resource<TerminalNode>) -> wasmtime::Result<()> {
        // Tolerate a missing entry — may have been consumed by `tree()`.
        let _ = self
            .resource_table
            .delete(Resource::<TerminalNodeBody>::new_own(rep.rep()));
        Ok(())
    }
}
