//! `wasmtime::component::bindgen!` output for the `ark:plugin` WIT world.
//!
//! T-PP-031 (cavekit-plugin-protocol R2): generates the typed host
//! trait scaffolding every Tier 3B+ host-fn impl must satisfy.
//!
//! # Options
//!
//! * `path: "../ark-plugin-protocol/wit"` — resolved relative to this
//!   crate's `CARGO_MANIFEST_DIR` (i.e. `crates/ark-host/`). The two
//!   source files (`plugin.wit`, `widget-tree.wit`) together declare the
//!   `ark:plugin@0.1.0` package plus its `plugin` world.
//! * `world: "plugin"` — the single world every ark plugin targets.
//! * `imports: { default: async | trappable }` — wasmtime 43 bindgen
//!   syntax for the two invariants kit R4 requires:
//!     - `async` — every host import is an `async fn` so epoch-based
//!       yielding composes with tokio (cluster 3 §3.2).
//!     - `trappable` — every host import returns
//!       `wasmtime::Result<T>` so a cap-denied call (the fine gate in
//!       T-PP-033) can return a nominal `Error` rather than a guest
//!       trap, per R4 acceptance "denial returns `wasmtime::Error`,
//!       not a trap".
//!
//! The older option names `async: true, trappable_imports: true` from
//! earlier wasmtime versions have been folded into the per-filter
//! syntax — `default: async | trappable` is the wasmtime 43 form.
//!
//! # Generated surface (consumed by T-PP-032..T-PP-034)
//!
//! bindgen generates one Rust module per WIT interface under the
//! `ark::plugin::*` hierarchy, each containing a `Host` trait the host
//! must implement on `PluginCtx`. Concretely:
//!
//! | WIT interface        | Generated trait path                                         |
//! |----------------------|--------------------------------------------------------------|
//! | `ark:plugin/log`     | `bindings::ark::plugin::log::Host`                           |
//! | `ark:plugin/clock`   | `bindings::ark::plugin::clock::Host`                         |
//! | `ark:plugin/plugin-id` | `bindings::ark::plugin::plugin_id::Host`                   |
//! | `ark:plugin/fs-read` | `bindings::ark::plugin::fs_read::Host`                       |
//! | `ark:plugin/fs-write` | `bindings::ark::plugin::fs_write::Host`                     |
//! | `ark:plugin/network` | `bindings::ark::plugin::network::Host`                       |
//! | `ark:plugin/spawn-process` | `bindings::ark::plugin::spawn_process::Host`           |
//! | `ark:plugin/bus-send` | `bindings::ark::plugin::bus_send::Host`                     |
//! | `ark:plugin/bus-receive` | `bindings::ark::plugin::bus_receive::Host`               |
//!
//! The `types` and `widget-tree-types` helper interfaces generate only
//! data-type modules (no `Host` trait) — they carry shared record
//! definitions.
//!
//! Host-trait impls for the unconditional `ark:host/*` interfaces live
//! in `host_fns.rs` (T-PP-032); cap-gated `ark:cap/*` trait impls live
//! in `cap_fns.rs` (T-PP-033). Both are wired into per-cap-profile
//! `Linker<PluginCtx>` variants by `linker_set.rs` (T-PP-034).

wasmtime::component::bindgen!({
    path: "../ark-plugin-protocol/wit",
    world: "plugin",
    imports: {
        default: async | trappable,
    },
});
