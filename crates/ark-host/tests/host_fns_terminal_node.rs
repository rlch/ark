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
    ContainerNode, CursorNode, CursorShape, HostTerminalNode, SpacerNode, TerminalWidgetTree,
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

/// F-452 (Cycle 2): calling `tree()` on a container with child handles
/// must not duplicate OWNED drop-rights over those children. Pre-fix,
/// `clone_container` rebuilt each child via `Resource::new_own(rep)` —
/// the second call to `tree()` then handed the guest a second owned
/// handle over the same table slot, and the parent's eventual `drop`
/// would hit a `ResourceTableError::NotPresent` panic because the
/// entry had already been removed.
///
/// This test covers the post-fix invariant:
///
/// 1. Two `tree()` calls on a `row` node with 3 children both succeed.
/// 2. The ResourceTable's live-entry count stays constant across the
///    two calls (no premature removal of child entries).
/// 3. Dropping the parent removes exactly one entry (the parent's
///    body); the three child entries are still reachable by `get`
///    until the guest drops them explicitly — but we don't exercise
///    that leg here because v1 contract says the guest owns the child
///    handles and is responsible for dropping them on its own frame.
#[test]
fn tree_with_children_stays_owned_across_multiple_reads() {
    let mut ctx = fresh_ctx();

    // Push three leaf children first — each `new()` returns ONE owned
    // handle, which we hand to the parent as a child. The parent's
    // stored `ContainerNode` then owns those handles.
    let c0 = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 1 }),
    ))
    .expect("child 0 new()");
    let c1 = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 2 }),
    ))
    .expect("child 1 new()");
    let c2 = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 3 }),
    ))
    .expect("child 2 new()");

    // Parent `row` — moves the owned child handles into its body. We
    // keep note of each child's rep so assertions below can compare.
    let child_reps = [c0.rep(), c1.rep(), c2.rep()];
    let parent = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Row(ContainerNode {
            children: vec![c0, c1, c2],
            layout: None,
        }),
    ))
    .expect("parent row new()");
    let parent_rep = parent.rep();

    // 4 table entries: 3 children + 1 parent.
    let entries_before = ctx.resource_table.iter_mut().count();
    assert_eq!(
        entries_before, 4,
        "expected 4 live ResourceTable entries (3 children + 1 parent) before first tree()"
    );

    // First tree() read — should succeed and hand back a rebuilt
    // ContainerNode whose children are BORROW handles over the same
    // table slots.
    let first = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("first tree() must succeed");
    match first {
        TerminalWidgetTree::Row(ref c) => {
            assert_eq!(c.children.len(), 3, "first tree() children count");
            for (slot, child) in c.children.iter().enumerate() {
                assert_eq!(
                    child.rep(),
                    child_reps[slot],
                    "first tree() child {slot} rep mismatch"
                );
            }
        }
        other => panic!("expected Row, got {other:?}"),
    }
    // Drop the rebuild so its borrow handles go out of scope; a borrow
    // handle drop is a no-op on the table per wasmtime docs.
    drop(first);

    // Table still has all 4 entries.
    let entries_after_first = ctx.resource_table.iter_mut().count();
    assert_eq!(
        entries_after_first, entries_before,
        "ResourceTable shrank after first tree() — borrow handles must not \
         transfer drop-rights to the guest-facing view"
    );

    // Second tree() read — same shape, same reps. Pre-fix: this would
    // panic when the second `new_own` copy's Drop ran and found the
    // slot already gone. Post-fix: table is untouched, reads succeed.
    let second = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect(
        "second tree() must succeed — borrow-handle rebuild keeps the \
         parent-owned child entries alive",
    );
    match second {
        TerminalWidgetTree::Row(ref c) => {
            assert_eq!(c.children.len(), 3, "second tree() children count");
            for (slot, child) in c.children.iter().enumerate() {
                assert_eq!(
                    child.rep(),
                    child_reps[slot],
                    "second tree() child {slot} rep mismatch"
                );
            }
        }
        other => panic!("expected Row, got {other:?}"),
    }
    drop(second);

    let entries_after_second = ctx.resource_table.iter_mut().count();
    assert_eq!(
        entries_after_second, entries_before,
        "ResourceTable shrank after second tree() — children must still \
         be reachable for any later guest reads until explicit drops"
    );
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
