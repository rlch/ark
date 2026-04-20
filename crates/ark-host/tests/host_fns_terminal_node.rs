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

/// F-459 (Cycle 3): calling `tree()` on a container with child handles
/// must produce INDEPENDENTLY OWNED child handles — fresh
/// `ResourceTable` entries whose lifetime is decoupled from the parent.
/// Cycle 2's borrow-handle approach passed host-side tests but failed
/// the ABI-boundary contract: borrowed wasmtime resources are
/// call-scoped and become invalid once control returns to the guest.
///
/// This test covers the post-fix invariant:
///
/// 1. Two `tree()` calls on a `row` node with 3 children both succeed.
/// 2. The children returned from `tree()` are FRESH entries (rep values
///    differ from the originals that the parent's body still owns).
/// 3. Each `tree()` call allocates N new entries (one per child), so
///    the table grows by 3 per call.
/// 4. The ORIGINAL parent-owned child entries remain reachable after
///    `tree()` — deep-clone does not disturb the stored body.
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
    let original_child_reps = [c0.rep(), c1.rep(), c2.rep()];
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
    // ContainerNode whose children are FRESH owned entries.
    let first = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("first tree() must succeed");
    let first_reps: Vec<u32> = match &first {
        TerminalWidgetTree::Row(c) => {
            assert_eq!(c.children.len(), 3, "first tree() children count");
            c.children.iter().map(|r| r.rep()).collect()
        }
        other => panic!("expected Row, got {other:?}"),
    };
    for (slot, rep) in first_reps.iter().enumerate() {
        assert_ne!(
            *rep, original_child_reps[slot],
            "first tree() child {slot} rep ({rep}) must differ from the \
             parent-owned original ({}) — deep-clone contract",
            original_child_reps[slot]
        );
    }
    drop(first);

    // Table grew by 3 (one fresh entry per child). Originals remain —
    // we confirm this indirectly via the entry count (the parent's
    // stored body still owns them; if deep-clone had moved them, the
    // count would only grow by +0 or +2 instead of +3).
    let entries_after_first = ctx.resource_table.iter_mut().count();
    assert_eq!(
        entries_after_first,
        entries_before + 3,
        "ResourceTable should grow by 3 (one fresh owned entry per child) after deep-clone tree()"
    );

    // Second tree() read — allocates ANOTHER fresh set of 3 entries.
    // Pre-F-459 fix: same reps would come back. Post-fix: must differ
    // from both the original set AND the first tree()'s set.
    let second = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("second tree() must succeed — deep-clone is idempotent");
    let second_reps: Vec<u32> = match &second {
        TerminalWidgetTree::Row(c) => {
            assert_eq!(c.children.len(), 3, "second tree() children count");
            c.children.iter().map(|r| r.rep()).collect()
        }
        other => panic!("expected Row, got {other:?}"),
    };
    for (slot, rep) in second_reps.iter().enumerate() {
        assert_ne!(
            *rep, original_child_reps[slot],
            "second tree() child {slot} must be fresh, not alias of original"
        );
        assert_ne!(
            *rep, first_reps[slot],
            "second tree() child {slot} must be fresh, not alias of first tree()'s entry"
        );
    }
    drop(second);

    let entries_after_second = ctx.resource_table.iter_mut().count();
    assert_eq!(
        entries_after_second,
        entries_before + 6,
        "ResourceTable should grow by 6 total (3 + 3) after two deep-clone tree() calls"
    );
}

/// F-459 (Cycle 3): children returned from `tree()` are INDEPENDENTLY
/// OWNED — the guest can drop the parent and still use the child
/// handles afterwards, because each child lives in its own fresh table
/// slot rather than aliasing one owned by the parent.
#[test]
fn tree_children_are_independently_droppable() {
    let mut ctx = fresh_ctx();

    let c0 = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 7 }),
    ))
    .expect("child 0 new()");
    let parent = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Row(ContainerNode {
            children: vec![c0],
            layout: None,
        }),
    ))
    .expect("parent new()");
    let parent_rep = parent.rep();

    // Take a snapshot via tree(), save the returned child handle.
    let snapshot = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("tree() must succeed");
    let child_rep = match &snapshot {
        TerminalWidgetTree::Row(c) => {
            assert_eq!(c.children.len(), 1);
            c.children[0].rep()
        }
        other => panic!("expected Row, got {other:?}"),
    };
    drop(snapshot);

    // Drop the PARENT. Whatever the parent-stored child's fate
    // (wasmtime's `ResourceTable::delete` may or may not cascade into
    // the parent-owned child slot), the tree()-returned child is a
    // FRESH entry with an independent lifetime — it MUST still resolve
    // on its own. That is the invariant borrow-handles failed: a
    // borrow tied to the parent-owned slot would go invalid at the
    // moment control left the host call.
    block_on(HostTerminalNode::drop(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("parent drop() must succeed");

    // Fresh child entry must still be a valid handle for a subsequent
    // tree() call. This is the property the borrow-handle approach
    // violated: borrow handles become invalid once the host call that
    // handed them out returns.
    let reread = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(child_rep),
    ))
    .expect(
        "tree() on the detached child handle must succeed — deep-clone guarantees \
         child handles are independently owned and survive parent drop",
    );
    match reread {
        TerminalWidgetTree::Spacer(SpacerNode { flex: 7 }) => {}
        other => panic!("child payload mismatch: {other:?}"),
    }
}

/// F-459 (Cycle 3): two calls to `tree()` return trees whose child
/// handles are DIFFERENT `.rep()` values (proving fresh entries were
/// allocated each time rather than reused).
#[test]
fn tree_twice_produces_distinct_child_handles() {
    let mut ctx = fresh_ctx();

    let c0 = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 11 }),
    ))
    .expect("child new()");
    let parent = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Column(ContainerNode {
            children: vec![c0],
            layout: None,
        }),
    ))
    .expect("parent new()");
    let parent_rep = parent.rep();

    let first = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("first tree()");
    let first_child_rep = match &first {
        TerminalWidgetTree::Column(c) => c.children[0].rep(),
        other => panic!("expected Column, got {other:?}"),
    };

    let second = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(parent_rep),
    ))
    .expect("second tree()");
    let second_child_rep = match &second {
        TerminalWidgetTree::Column(c) => c.children[0].rep(),
        other => panic!("expected Column, got {other:?}"),
    };

    assert_ne!(
        first_child_rep, second_child_rep,
        "two tree() calls should allocate two distinct child entries (got {first_child_rep} both times)"
    );
}

/// F-459 (Cycle 3): deep-clone must descend through NESTED containers —
/// a row whose children are themselves rows must yield a view whose
/// full subtree uses fresh reps distinct from the stored originals.
#[test]
fn tree_deep_clones_nested_containers() {
    let mut ctx = fresh_ctx();

    // Build: Row[ Row[ Spacer ] ] — two levels of nesting.
    let leaf = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Spacer(SpacerNode { flex: 42 }),
    ))
    .expect("leaf new()");
    let leaf_rep = leaf.rep();
    let inner_row = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Row(ContainerNode {
            children: vec![leaf],
            layout: None,
        }),
    ))
    .expect("inner row new()");
    let inner_rep = inner_row.rep();
    let outer = block_on(HostTerminalNode::new(
        &mut ctx,
        TerminalWidgetTree::Row(ContainerNode {
            children: vec![inner_row],
            layout: None,
        }),
    ))
    .expect("outer row new()");
    let outer_rep = outer.rep();

    let snapshot = block_on(HostTerminalNode::tree(
        &mut ctx,
        wasmtime::component::Resource::new_own(outer_rep),
    ))
    .expect("outer tree()");

    match snapshot {
        TerminalWidgetTree::Row(outer_c) => {
            assert_eq!(outer_c.children.len(), 1);
            let cloned_inner_rep = outer_c.children[0].rep();
            assert_ne!(
                cloned_inner_rep, inner_rep,
                "deep-clone must allocate a fresh inner-row entry"
            );
            // Drill into the cloned inner row to verify its child is
            // also fresh, not aliasing the original leaf.
            let inner_snapshot = block_on(HostTerminalNode::tree(
                &mut ctx,
                wasmtime::component::Resource::new_own(cloned_inner_rep),
            ))
            .expect("inner tree() on the deep-cloned row");
            match inner_snapshot {
                TerminalWidgetTree::Row(inner_c) => {
                    assert_eq!(inner_c.children.len(), 1);
                    let cloned_leaf_rep = inner_c.children[0].rep();
                    assert_ne!(
                        cloned_leaf_rep, leaf_rep,
                        "deep-clone must descend — inner leaf must also be fresh"
                    );
                    // And the payload is preserved through the clone.
                    let leaf_snapshot = block_on(HostTerminalNode::tree(
                        &mut ctx,
                        wasmtime::component::Resource::new_own(cloned_leaf_rep),
                    ))
                    .expect("leaf tree() on deep-cloned leaf");
                    match leaf_snapshot {
                        TerminalWidgetTree::Spacer(SpacerNode { flex: 42 }) => {}
                        other => panic!("leaf payload drifted: {other:?}"),
                    }
                }
                other => panic!("expected inner Row, got {other:?}"),
            }
        }
        other => panic!("expected outer Row, got {other:?}"),
    }
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
