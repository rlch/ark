//! `ark-host` — wasmtime-based plugin runtime substrate.
//!
//! T-PP-004 (cavekit-plugin-protocol R1): Tier 0 scaffold.
//!
//! Tier 3A (T-PP-025..T-PP-030) — landed: `engine` singleton, per-plugin
//! `Store<PluginCtx>`, default-deny `WasiCtx` helper, forbidden-API lint.
//! Tier 3B+ (T-PP-031..T-PP-036) — pending: bindgen, host-fn bodies,
//! `LinkerSet`, `InstancePre` cache.

pub mod bindings;
pub mod cache;
pub mod cap_fns;
pub mod engine;
pub mod host_fns;
pub mod lifecycle;
pub mod linker_set;
pub mod loader;
pub mod store;

// Tier 3A public surface — the only names Tier 3B and downstream
// crates should need. Deeper types (e.g. `LogSink`, `ticks_seen`) are
// reachable via their modules for callers that want them.
pub use engine::{engine, start_epoch_ticker};
pub use store::{
    PluginCtx, default_deny_wasi, new_default_deny_store, new_store, wasi_ctx_for_caps,
};

// Tier 3B public surface (T-PP-031..T-PP-036).
//
// `PluginHost` is the bindgen-generated struct for the `plugin-host`
// world (see `bindings.rs`) — the host-side union world that holds
// pre-instantiation indices for plugin exports. Plugins themselves
// target `plugin-base` or their own world; the host uses the union
// to bind against any variant.
pub use bindings::PluginHost;
pub use cache::{ContentHash, InstancePreCache, content_hash};
pub use linker_set::{CapsKey, LinkerSet};
