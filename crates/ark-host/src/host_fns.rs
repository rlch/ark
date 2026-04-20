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
    widget_tree_types::{
        self, ContainerNode, HostTerminalNode, TerminalNode, TerminalWidgetTree,
    },
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

/// Reconstruct a `TerminalWidgetTree` from a shared reference, rebuilding
/// each `Resource<TerminalNode>` handle via `new_borrow(rep)` with the
/// SAME `u32` rep the table uses internally. The generated
/// `TerminalWidgetTree` is NOT `Clone` because `wasmtime::component::
/// Resource<T>` enforces host-side ownership semantics (no bitwise copy),
/// so a `#[derive(Clone)]` can't propagate. A manual shallow rebuild is
/// the right tool.
///
/// # Why borrow, not own
///
/// The children of a `ContainerNode` stored inside a `TerminalNodeBody`
/// are themselves `Resource<TerminalNode>` handles that were originally
/// pushed onto the same `ResourceTable` by earlier guest calls to
/// `terminal-node::new()`. Each of those pushes returned a *single*
/// OWNED handle. The guest then handed those owned handles to the host
/// as children of a new parent tree — at which point the host stored
/// them inside `TerminalNodeBody(TerminalWidgetTree)`. So the table has
/// exactly ONE owned ticket per live child entry, and it lives inside
/// the parent's stored body.
///
/// When the guest later calls `tree()` on the parent and the host hands
/// back a shallow rebuild of the children, those rebuilt handles must
/// NOT carry their own drop-rights — if they did, the guest's trip
/// around the generated `Drop for Resource<T>` glue would remove the
/// table entry out from under the parent, and the next `tree()` call
/// (or the parent's own `drop`) would panic with "entry already
/// removed". Borrow handles make the view non-owning: the guest can
/// read the tree, but the parent's OWNED handle remains the sole
/// drop-authority. See kit R10 acceptance: "tree() is idempotent, drop
/// is what transfers ownership".
fn clone_terminal_widget_tree(t: &TerminalWidgetTree) -> TerminalWidgetTree {
    match t {
        TerminalWidgetTree::Text(n) => TerminalWidgetTree::Text(n.clone()),
        TerminalWidgetTree::Row(c) => TerminalWidgetTree::Row(clone_container(c)),
        TerminalWidgetTree::Column(c) => TerminalWidgetTree::Column(clone_container(c)),
        TerminalWidgetTree::BoxNode(c) => TerminalWidgetTree::BoxNode(clone_container(c)),
        TerminalWidgetTree::Spacer(n) => TerminalWidgetTree::Spacer(*n),
        TerminalWidgetTree::Cursor(n) => TerminalWidgetTree::Cursor(*n),
    }
}

/// See [`clone_terminal_widget_tree`].
///
/// Each child handle is rebuilt via [`Resource::new_borrow`] — NOT
/// `new_own`. Using `new_own` here would create duplicate drop-authority
/// over the same table slot, so the second `tree()` call (or the
/// parent's own drop) would hit a `ResourceTableError::NotPresent`
/// panic when the second owner tries to delete an already-deleted
/// entry. Borrow handles are explicitly documented in wasmtime's
/// `Resource::new_borrow` as "passed to a guest as a borrowed resource;
/// the embedder knows the `rep` won't be in use by the guest
/// afterwards" — exactly what a read-only `tree()` view needs.
fn clone_container(c: &ContainerNode) -> ContainerNode {
    ContainerNode {
        children: c
            .children
            .iter()
            .map(|r| wasmtime::component::Resource::<TerminalNode>::new_borrow(r.rep()))
            .collect(),
        layout: c.layout,
    }
}

impl HostTerminalNode for PluginCtx {
    async fn new(&mut self, tree: TerminalWidgetTree) -> wasmtime::Result<Resource<TerminalNode>> {
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
        // WIT signature `tree: func() -> terminal-widget-tree` is NOT a
        // consumer — the handle has its own destructor (the `drop` impl
        // below). `tree()` is idempotent: calling it twice on the same
        // handle must yield two identical payloads without surfacing a
        // "resource missing" error on the second read. Clone the stored
        // payload instead of removing it from the table.
        //
        // Clone cost: `TerminalWidgetTree` is a tagged-union of small
        // records (color triples, u32 flex/padding, optional styled
        // strings) plus a list of `Resource<TerminalNode>` children —
        // the children are handle copies, not a deep tree clone, so the
        // clone is O(direct-children) per node. For v1 this is cheap
        // enough to not warrant an `Arc` wrapper; revisit if render
        // paths show up in profiles.
        let body: &TerminalNodeBody = self
            .resource_table
            .get(&Resource::<TerminalNodeBody>::new_own(self_.rep()))?;
        Ok(clone_terminal_widget_tree(&body.0))
    }

    async fn drop(&mut self, rep: Resource<TerminalNode>) -> wasmtime::Result<()> {
        // `tree()` no longer consumes, so the entry is expected to be
        // present — but we still `let _ =` to stay tolerant of a guest
        // that drops an already-dropped handle (defense-in-depth for
        // malformed guests). A missing entry is NOT a host-observable
        // error.
        let _ = self
            .resource_table
            .delete(Resource::<TerminalNodeBody>::new_own(rep.rep()));
        Ok(())
    }
}
