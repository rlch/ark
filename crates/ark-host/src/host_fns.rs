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
pub(crate) struct TerminalNodeBody(pub(crate) TerminalWidgetTree);

/// Cycle 3 fix (F-459/F-461): `tree()` produces a DEEP CLONE of the
/// stored `TerminalWidgetTree`, with every child handle materialised as
/// a fresh `ResourceTable` entry.
///
/// # Why not borrow handles
///
/// Cycle 2 rebuilt each child via `Resource::new_borrow(rep)`. That
/// passes wasmtime's type-check and the host-side idempotence test, but
/// it fails the ABI-boundary semantics: **borrowed Wasmtime resources
/// are CALL-SCOPED**. The borrow is valid only for the duration of the
/// host call that produced it. Once control returns to the guest and
/// the guest later enters another host call holding that handle (e.g.
/// `child.tree()`), the original borrow has expired — the generated
/// glue will refuse to resolve it, producing an invalid-handle trap or
/// (worse) silent aliasing if the slot got recycled.
///
/// Deep-cloning sidesteps the borrow-scope problem entirely. Each child
/// returned by `tree()` is a FRESH `ResourceTable` entry with its own
/// owned handle. The guest gets independent drop-rights over every
/// handle in the returned tree; the parent's stored handles and the
/// view's handles have separate lifetimes; a second call to `tree()`
/// allocates a second, still-independent snapshot.
///
/// Cost is O(total nodes in tree) per `tree()` call plus one
/// `ResourceTable::push` per child. This is strictly worse than the
/// shallow-borrow approach in steady-state memory, but it is the only
/// option that matches wasmtime's published ABI contract for handles
/// that cross the host/guest boundary. v1 renders are infrequent
/// compared to the render surface itself; revisit with an `Arc<_>`
/// interior-shared representation only if profiling flags it.
///
/// # Borrow-checker shape
///
/// The deep clone is split into two passes. [`shape_snapshot`] walks a
/// stored `TerminalWidgetTree` under a short immutable borrow of the
/// resource table and copies the variant + leaf records into a
/// table-free [`TreeShape`] (containers keep their children as bare
/// `u32` reps, not handles). Once the immutable borrow is released,
/// [`rebuild_from_snapshot`] takes `&mut PluginCtx` and does the actual
/// `ResourceTable::push`es — recursively, since it may need to snapshot
/// child bodies in turn. That separation is what appeases the borrow
/// checker: we never hold a `&TerminalNodeBody` reference across a
/// table mutation.
///
/// Owned, table-free mirror of a `TerminalWidgetTree` used to smuggle
/// subtree shape across a borrow-checker boundary.
///
/// We need this intermediate because the recursive deep-clone requires
/// `&mut ctx` but the caller is inside a `resource_table.get(...)`
/// immutable borrow — the natural `for child in ... { deep_clone(child,
/// ctx) }` shape would hold both borrows simultaneously.
///
/// `TreeShape` carries the enum variant and its leaf data, PLUS the
/// child reps (not handles — plain `u32`) when it's a container. A
/// second pass (`rebuild_from_snapshot`) then does the actual
/// resource-table pushes without needing to re-enter the original
/// handle's body.
enum TreeShape {
    Text(crate::bindings::ark::plugin::widget_tree_types::TextNode),
    Spacer(crate::bindings::ark::plugin::widget_tree_types::SpacerNode),
    Cursor(crate::bindings::ark::plugin::widget_tree_types::CursorNode),
    Row(ContainerShape),
    Column(ContainerShape),
    BoxNode(ContainerShape),
}

struct ContainerShape {
    child_reps: Vec<u32>,
    layout: Option<crate::bindings::ark::plugin::widget_tree_types::LayoutHints>,
}

fn shape_snapshot(t: &TerminalWidgetTree) -> TreeShape {
    match t {
        TerminalWidgetTree::Text(n) => TreeShape::Text(n.clone()),
        TerminalWidgetTree::Spacer(n) => TreeShape::Spacer(*n),
        TerminalWidgetTree::Cursor(n) => TreeShape::Cursor(*n),
        TerminalWidgetTree::Row(c) => TreeShape::Row(container_shape(c)),
        TerminalWidgetTree::Column(c) => TreeShape::Column(container_shape(c)),
        TerminalWidgetTree::BoxNode(c) => TreeShape::BoxNode(container_shape(c)),
    }
}

fn container_shape(c: &ContainerNode) -> ContainerShape {
    ContainerShape {
        child_reps: c.children.iter().map(|r| r.rep()).collect(),
        layout: c.layout,
    }
}

/// Rebuild a `TerminalWidgetTree` from a `TreeShape`, allocating fresh
/// `ResourceTable` entries for each container's children via the
/// existing deep-clone recursion ([`deep_clone_container_from_reps`]).
///
/// The result is owned independently of the original table entry the
/// snapshot was taken from.
fn rebuild_from_snapshot(
    ctx: &mut PluginCtx,
    shape: &TreeShape,
) -> wasmtime::Result<TerminalWidgetTree> {
    match shape {
        TreeShape::Text(n) => Ok(TerminalWidgetTree::Text(n.clone())),
        TreeShape::Spacer(n) => Ok(TerminalWidgetTree::Spacer(*n)),
        TreeShape::Cursor(n) => Ok(TerminalWidgetTree::Cursor(*n)),
        TreeShape::Row(c) => Ok(TerminalWidgetTree::Row(deep_clone_container_from_reps(
            ctx, c,
        )?)),
        TreeShape::Column(c) => Ok(TerminalWidgetTree::Column(deep_clone_container_from_reps(
            ctx, c,
        )?)),
        TreeShape::BoxNode(c) => Ok(TerminalWidgetTree::BoxNode(deep_clone_container_from_reps(
            ctx, c,
        )?)),
    }
}

/// Rebuild a `ContainerNode` from a `ContainerShape` whose children are
/// referenced by rep. Each rep is looked up in the table, its subtree
/// shaped-snapshotted, then rebuilt as a fresh entry.
fn deep_clone_container_from_reps(
    ctx: &mut PluginCtx,
    source: &ContainerShape,
) -> wasmtime::Result<ContainerNode> {
    let mut new_children = Vec::with_capacity(source.child_reps.len());
    for rep in &source.child_reps {
        let snapshot = {
            let existing: &TerminalNodeBody = ctx
                .resource_table
                .get(&Resource::<TerminalNodeBody>::new_own(*rep))?;
            shape_snapshot(&existing.0)
        };
        let cloned = rebuild_from_snapshot(ctx, &snapshot)?;
        let new_entry = ctx.resource_table.push(TerminalNodeBody(cloned))?;
        new_children.push(Resource::new_own(new_entry.rep()));
    }
    Ok(ContainerNode {
        children: new_children,
        layout: source.layout,
    })
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
        // "resource missing" error on the second read. And — critical
        // per F-459 — each returned subtree must be INDEPENDENTLY OWNED:
        // the guest can drop children at will, pass them to subsequent
        // host calls, and survive the parent being dropped.
        //
        // Strategy: snapshot the stored body under a short immutable
        // borrow, then deep-clone it — recursively allocating fresh
        // `ResourceTable` entries for every nested child — into a new
        // owned tree. See `deep_clone_tree` for the full rationale.
        let parent_rep = self_.rep();
        let snapshot = {
            let body: &TerminalNodeBody = self
                .resource_table
                .get(&Resource::<TerminalNodeBody>::new_own(parent_rep))?;
            shape_snapshot(&body.0)
        };
        rebuild_from_snapshot(self, &snapshot)
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
