//! Closed set of per-cap-profile `Linker<PluginCtx>` variants.
//!
//! T-PP-034 (cavekit-plugin-protocol R4): the runtime holds exactly one
//! `wasmtime::component::Linker<PluginCtx>` per distinct
//! [`CapsKey`] (ordered set of cap ids). The v1 rule from kit R4
//! "Approach B": cap gating is TWO layers:
//!
//! 1. **Coarse gate** — the per-cap-profile linker: a plugin whose
//!    `ark-caps:v1` section lists `{fs-read, network}` is instantiated
//!    against the linker built from `CapsKey::from(["fs-read",
//!    "network"])`. The `ark:cap/fs-write` interface never appears in
//!    that linker's imports — a plugin that tries to call a
//!    non-imported interface gets a link-time error, not a trap.
//!
//! 2. **Fine gate** — the in-fn check in `cap_fns.rs`. A second line of
//!    defence against mis-wired linkers and a stable place to return a
//!    nominal `wasmtime::Error::new(PluginLoadError::CapNotGranted { …
//!    })` that downstream diagnostics key off.
//!
//! The **`empty` variant** — the linker built from `CapsKey::new()` —
//! is always present, even when no declared plugin asks for it. It
//! carries the unconditional `ark:host/*` services and WASI p2 only.
//! A deny-all plugin runs against this linker.
//!
//! # WASI p2 wiring
//!
//! Every variant calls [`wasmtime_wasi::p2::add_to_linker_async`] so
//! `wasi:clocks` / `wasi:filesystem` / `wasi:io` / `wasi:sockets`
//! imports resolve through the per-plugin `WasiCtx` wired at
//! instantiation time. The WasiCtx itself is default-deny — granting
//! `network` at the cap level ALSO upgrades the WasiCtx to allow
//! TCP/UDP/DNS (see `store::wasi_ctx_for_caps`, T-PP-035); granting
//! `fs-read` adds a read-only preopen.
//!
//! # Performance
//!
//! Lookup is a `HashMap<CapsKey, Linker<PluginCtx>>`. `CapsKey` is a
//! `BTreeSet<String>` — its `Hash` implementation visits each element
//! in sort order, giving stable O(k) hashing where k = #caps. A plugin
//! reinstantiation path is therefore an O(1) HashMap hit + whatever
//! `instantiate_pre` costs (which is then amortised by
//! `InstancePreCache` in T-PP-036). No micro-benchmark is included —
//! HashMap lookup on a <12-entry map is unconditionally well under
//! 100 ns on any current hardware.

use std::collections::{BTreeSet, HashMap};

use wasmtime::component::{HasSelf, Linker};

use crate::PluginCtx;
use crate::bindings::ark::plugin::{
    bus_receive, bus_send, clock, fs_read, fs_write, log, network, plugin_id, spawn_process, types,
    widget_tree_types,
};
use crate::engine::engine;

/// Ordered set of cap ids. Used as the HashMap key for linker variants
/// and instance-pre cache entries.
///
/// Must be a `BTreeSet` (not `HashSet`) so the derived `Hash` is stable
/// across runs — a permutation of insertion order must not change the
/// cache key.
pub type CapsKey = BTreeSet<String>;

/// Closed set of `Linker<PluginCtx>` variants, one per distinct cap
/// profile seen at startup. Built once; queried for every plugin
/// instantiation.
pub struct LinkerSet {
    variants: HashMap<CapsKey, Linker<PluginCtx>>,
}

impl LinkerSet {
    /// Build a `LinkerSet` covering every `CapsKey` in `all_declared_caps`
    /// plus an always-present `empty` variant.
    ///
    /// Each variant is wired with:
    /// * WASI p2 (`wasmtime_wasi::p2::add_to_linker_async`) — resolves
    ///   any `wasi:*` imports the plugin has, subject to the plugin's
    ///   `WasiCtx` (built separately by `store::wasi_ctx_for_caps`).
    /// * The three unconditional `ark:host/*` interfaces (`log`,
    ///   `clock`, `plugin-id`) — always registered.
    /// * The helper `types` / `widget-tree-types` interfaces —
    ///   registered because `widget-tree-types` carries the
    ///   `terminal-node` resource used in `widget-tree` payloads that
    ///   every plugin returns from `render`.
    /// * Each `ark:cap/*` interface IFF the variant's `CapsKey`
    ///   contains that cap id. A plugin whose WIT imports
    ///   `ark:cap/fs-read` but runs against a linker that was built
    ///   without fs-read fails at `instantiate_pre` time — this is the
    ///   R4 "coarse gate" (Approach B proper).
    ///
    /// The cap fine-gate inside each cap-fn body (`cap_fns.rs`) is a
    /// defense-in-depth check that also returns
    /// `PluginLoadError::CapNotGranted` if somehow a mis-wired linker
    /// still exposes the fn at call time.
    pub fn build(all_declared_caps: Vec<CapsKey>) -> wasmtime::Result<Self> {
        let mut variants: HashMap<CapsKey, Linker<PluginCtx>> = HashMap::new();
        // Always include the `empty` variant (deny-all caps).
        let mut keys: Vec<CapsKey> = all_declared_caps;
        keys.push(CapsKey::new());
        keys.sort_by_key(|k| k.len());
        keys.dedup();

        for key in keys {
            if variants.contains_key(&key) {
                continue;
            }
            let linker = build_one_variant(&key)?;
            variants.insert(key, linker);
        }

        Ok(Self { variants })
    }

    /// Returns the `Linker<PluginCtx>` variant built for `key`, if any.
    ///
    /// Lookup is O(1) in the cap-set count and O(k) in cap-id count for
    /// the HashMap hash of the key. Callers should cache the returned
    /// reference for the lifetime of a plugin instantiation.
    pub fn for_caps(&self, key: &CapsKey) -> Option<&Linker<PluginCtx>> {
        self.variants.get(key)
    }

    /// Number of distinct linker variants currently built. Exposed for
    /// tests; downstream code should not rely on it.
    #[doc(hidden)]
    pub fn variant_count(&self) -> usize {
        self.variants.len()
    }
}

/// Build one `Linker<PluginCtx>` with WASI p2 + the unconditional
/// `ark:host/*` trio + the helper type interfaces + only those
/// `ark:cap/*` interfaces named in `caps`.
///
/// The per-interface `add_to_linker` functions come from the
/// `plugin-host` bindgen world (see `crates/ark-host/src/bindings.rs`
/// and the WIT contract in `crates/ark-plugin-protocol/wit/plugin.wit`).
/// The `HasSelf<PluginCtx>` marker is wasmtime 43's convenience
/// `HasData` impl that makes `D::Data<'_> = &'_ mut PluginCtx`, so
/// every generated host-fn glue (e.g. `Host::log(host, …)`) gets a
/// `&mut PluginCtx`.
fn build_one_variant(caps: &CapsKey) -> wasmtime::Result<Linker<PluginCtx>> {
    let mut linker: Linker<PluginCtx> = Linker::new(engine());
    // WASI p2 — resolves `wasi:clocks` / `wasi:io` / `wasi:filesystem` /
    // `wasi:sockets` imports against the per-plugin `WasiCtx` carried
    // on `PluginCtx`.
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

    // Unconditional `ark:host/*` services — every plugin imports these.
    log::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    clock::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    plugin_id::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;

    // Helper type interfaces. `widget-tree-types` carries the
    // `terminal-node` resource used inside every `widget-tree` the
    // guest returns from `render`; `types` is a no-op instance
    // registration (no fns/resources) that bindgen still emits
    // because the interface declares shared data types.
    types::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    widget_tree_types::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;

    // Capability-gated interfaces — registered IFF the variant's
    // CapsKey contains the matching cap id. A plugin that imports an
    // interface NOT registered here fails at `instantiate_pre` with
    // a link-time error — the R4 coarse gate (Approach B proper).
    if caps.contains("fs-read") {
        fs_read::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }
    if caps.contains("fs-write") {
        fs_write::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }
    if caps.contains("network") {
        network::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }
    if caps.contains("spawn-process") {
        spawn_process::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }
    if caps.contains("bus-send") {
        bus_send::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }
    if caps.contains("bus-receive") {
        bus_receive::add_to_linker::<_, HasSelf<PluginCtx>>(&mut linker, |ctx| ctx)?;
    }

    Ok(linker)
}
