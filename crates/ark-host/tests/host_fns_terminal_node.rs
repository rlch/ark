//! T-PP-032 / kit R10 regression: `HostTerminalNode::tree` is idempotent.
//!
//! The WIT signature `tree: func() -> terminal-widget-tree` is NOT a
//! consumer — the resource handle has its own `drop` glue. v1 contract
//! (kit R10) requires that calling `tree()` twice on the same handle
//! observes two identical payloads, with the entry only removed from
//! the `ResourceTable` when the guest (or the host-side `drop` impl)
//! explicitly drops the handle.
//!
//! Pre-fix the host called `ResourceTable::delete` inside `tree()`,
//! which transferred ownership out and left a second `tree()` call
//! surfacing a `ResourceTableError::NotPresent`. This test asserts the
//! post-fix behaviour: two reads succeed and the extracted payloads
//! compare equal.
//!
//! Exercises the `HostTerminalNode` trait impl on `PluginCtx` directly
//! (via `block_on` for the `async fn`s), so no guest wasm is
//! needed — we're pinning the host-side resource-table contract.

use std::collections::BTreeSet;

use ark_host::store::NullLogSink;
use ark_host::{PluginCtx, default_deny_wasi};

use ark_host::bindings::ark::plugin::widget_tree_types::{
    CursorNode, CursorShape, HostTerminalNode, SpacerNode, TerminalWidgetTree,
};

/// Run an `async fn` on a new single-threaded tokio runtime. We need a
/// runtime because the `HostTerminalNode` trait methods are `async` —
/// bindgen's `default: async | trappable` option decoration (see
/// `crates/ark-host/src/bindings.rs`) — but this test doesn't need
/// multi-thread or timers.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build tokio rt")
        .block_on(f)
}

fn fresh_ctx() -> PluginCtx {
    PluginCtx::new(
        default_deny_wasi(),
        "idempotent-tree-test",
        BTreeSet::new(),
        Box::new(NullLogSink),
    )
}

fn spacer_payload() -> TerminalWidgetTree {
    // A concrete leaf with no child resources — keeps the test focused
    // on the `ResourceTable::get` vs. `delete` semantic, not on deep-
    // tree traversal. A separate host-side unit test covers the
    // child-rebuild path inside `clone_terminal_widget_tree`.
    TerminalWidgetTree::Spacer(SpacerNode { flex: 3 })
}

#[test]
fn tree_is_idempotent_on_spacer_leaf() {
    let mut ctx = fresh_ctx();
    let handle = block_on(HostTerminalNode::new(&mut ctx, spacer_payload()))
        .expect("host new() must succeed");
    let rep = handle.rep();

    // First call.
    let first = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(rep),
    ))
    .expect("first tree() must succeed");
    match first {
        TerminalWidgetTree::Spacer(SpacerNode { flex: 3 }) => {}
        other => panic!("first tree() payload mismatch: {other:?}"),
    }

    // Second call — THIS is the regression. Pre-fix: ResourceTable::delete
    // has already consumed the entry, so this returns a NotPresent
    // error. Post-fix: ResourceTable::get hands back &TerminalNodeBody
    // and `clone_terminal_widget_tree` rebuilds an owned payload.
    let second = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(rep),
    ))
    .expect(
        "second tree() call must succeed — kit R10 requires the resource \
         handle to be idempotently readable until the guest drops it",
    );
    match second {
        TerminalWidgetTree::Spacer(SpacerNode { flex: 3 }) => {}
        other => panic!("second tree() payload mismatch: {other:?}"),
    }

    // Finally, an explicit drop should succeed exactly once; a repeat
    // drop is tolerated (defense-in-depth against malformed guests).
    block_on(HostTerminalNode::drop(
        &mut ctx,
        wasmtime::component::Resource::new_own(rep),
    ))
    .expect("first drop() must succeed");
    block_on(HostTerminalNode::drop(
        &mut ctx,
        wasmtime::component::Resource::new_own(rep),
    ))
    .expect("second drop() must be tolerated (no-op)");
}

#[test]
fn tree_is_idempotent_on_cursor_leaf() {
    // A second leaf shape to catch any variant-specific regression in
    // `clone_terminal_widget_tree`'s match arms.
    let mut ctx = fresh_ctx();
    let payload = TerminalWidgetTree::Cursor(CursorNode {
        row: 7,
        col: 13,
        shape: CursorShape::Bar,
    });
    let handle = block_on(HostTerminalNode::new(&mut ctx, payload))
        .expect("host new() must succeed");
    let rep = handle.rep();

    for which in ["first", "second"] {
        let out = block_on(HostTerminalNode::tree(
            &mut ctx,
            wasmtime::component::Resource::new_own(rep),
        ))
        .unwrap_or_else(|e| panic!("{which} tree() must succeed: {e:#}"));
        match out {
            TerminalWidgetTree::Cursor(CursorNode {
                row: 7,
                col: 13,
                shape: CursorShape::Bar,
            }) => {}
            other => panic!("{which} tree() cursor payload mismatch: {other:?}"),
        }
    }
}
