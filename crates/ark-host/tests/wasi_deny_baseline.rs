//! T-PP-029 (cavekit-plugin-protocol R1, R4, R8): default-deny WASI
//! baseline.
//!
//! wasmtime-wasi 43 does not expose public getters for the internal
//! socket-use flags, so the tightest verifiable invariant in pure Rust
//! is "`default_deny_wasi()` returns a `WasiCtx` without panicking and
//! the function is the only `WasiCtxBuilder`-construction path in
//! `src/`" — the lint test (T-PP-030) enforces the second half. This
//! file enforces the first half plus the factory-level invariants we
//! *can* see from the outside:
//!
//! * `PluginCtx::new` with the default-deny ctx produces a `WasiView`
//!   impl whose `ctx()` call hands back a non-aliased `WasiCtxView`.
//! * `new_store` wires up `Store<PluginCtx>` without panicking and the
//!   epoch-deadline setters succeed (they would panic on an engine
//!   constructed without `epoch_interruption(true)` — so this indirectly
//!   re-checks the engine invariant from T-PP-025).
//! * The Tier 3B loader's plan is: take this baseline `WasiCtx`, then
//!   apply per-granted-cap deltas. By construction here (no preopens,
//!   no env, no args, TCP/UDP/DNS all false) a zero-grant plugin cannot
//!   see anything from the host.

use std::collections::BTreeSet;

use ark_host::store::NullLogSink;
use ark_host::{PluginCtx, default_deny_wasi, new_default_deny_store, new_store};
use wasmtime_wasi::WasiView;

#[test]
fn default_deny_wasi_builds_without_panic() {
    // The build() call on WasiCtxBuilder panics on second use; each
    // call must yield a fresh context.
    let _ctx1 = default_deny_wasi();
    let _ctx2 = default_deny_wasi();
    let _ctx3 = default_deny_wasi();
}

#[test]
fn plugin_ctx_wasi_view_trait_impls_are_consistent() {
    // Check that the WasiView trait impl wires through to the same
    // ResourceTable and WasiCtx we put in.
    let wasi = default_deny_wasi();
    let mut ctx = PluginCtx::new(wasi, "test-plugin", BTreeSet::new(), Box::new(NullLogSink));
    // Grab pointers through the trait view.
    let view = ctx.ctx();
    let wasi_ptr: *const _ = &*view.ctx;
    let table_ptr: *const _ = &*view.table;
    // Drop the borrow and re-check the trait returns the same pointers.
    drop(view);
    let view2 = ctx.ctx();
    let wasi_ptr2: *const _ = &*view2.ctx;
    let table_ptr2: *const _ = &*view2.table;
    assert_eq!(wasi_ptr, wasi_ptr2, "WasiView::ctx must re-borrow the same WasiCtx");
    assert_eq!(
        table_ptr, table_ptr2,
        "WasiView::ctx must re-borrow the same ResourceTable"
    );
}

#[test]
fn new_store_sets_epoch_deadline() {
    // If the engine were constructed without epoch_interruption(true),
    // these setters would panic — so the test doubles as a T-PP-025
    // smoke check for the engine flag.
    let wasi = default_deny_wasi();
    let ctx = PluginCtx::new(
        wasi,
        "epoch-smoke-test",
        BTreeSet::new(),
        Box::new(NullLogSink),
    );
    let _store = new_store(ctx);
    // Surviving this line is the assertion — if epoch_interruption
    // were off, set_epoch_deadline would have panicked.
}

#[test]
fn new_default_deny_store_factory_works() {
    let mut grants = BTreeSet::new();
    grants.insert("fs-read".to_owned());
    let store = new_default_deny_store("factory-test", grants, Box::new(NullLogSink));
    // Inspect the ctx we just built.
    let ctx = store.data();
    assert_eq!(ctx.plugin_id, "factory-test");
    assert!(ctx.granted_caps.contains("fs-read"));
    assert_eq!(ctx.granted_caps.len(), 1);
}

#[test]
fn zero_granted_caps_is_the_empty_set() {
    // The default-deny baseline means "no grants" is a valid and fully
    // functional PluginCtx. The store is a valid `Store<PluginCtx>` —
    // a plugin instantiated against such a store would simply have no
    // capability to do anything network- or filesystem-side.
    let store = new_default_deny_store("no-caps", BTreeSet::new(), Box::new(NullLogSink));
    let ctx = store.data();
    assert!(
        ctx.granted_caps.is_empty(),
        "default-deny store must start with zero granted caps"
    );
}
