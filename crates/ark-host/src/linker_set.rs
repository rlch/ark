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
//! carries ONLY the unconditional `ark:host/*` services plus the two
//! shared type interfaces. A deny-all plugin runs against this linker.
//!
//! # No WASI surface is exposed to plugins
//!
//! Earlier drafts called [`wasmtime_wasi::p2::add_to_linker_async`]
//! on every variant so `wasi:*` imports (`wasi:filesystem`,
//! `wasi:sockets`, `wasi:io`, `wasi:clocks`, `wasi:random`, …) would
//! resolve. That wiring was removed in the Tier 3 gate fix (F-445):
//! a plugin that imports `wasi:filesystem` directly would have
//! bypassed the `ark:cap/*` coarse gate entirely — the user can grant
//! `fs-read` but the plugin could import `wasi:filesystem/types` and
//! touch the world without a host-authored fine gate.
//!
//! The rule is now: **plugins never see `wasi:*` imports.** Every
//! capability-gated I/O primitive the guest needs flows through an
//! `ark:cap/*` interface (R3 + R4), whose implementation on the host
//! side may call WASI internally. The bindgen-generated WIT surface
//! (`crates/ark-plugin-protocol/wit/plugin.wit` +
//! `widget-tree.wit`) deliberately does NOT `use` or re-export any
//! `wasi:*` type, so a conforming plugin has no lexical path to
//! `wasi:*` from `ark:plugin@1.0.0`.
//!
//! `PluginCtx` still carries a `WasiCtx` (see `store::wasi_ctx_for_caps`)
//! because host-side code may call WASI APIs when implementing cap
//! fns — the context, not the linker, is where WASI lives.
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
    /// NO `wasi:*` interface is registered on any variant. A plugin
    /// that imports `wasi:filesystem` / `wasi:sockets` / `wasi:io` /
    /// etc. fails at `instantiate_pre` in EVERY variant, including the
    /// ones with caps granted. The plugin contract is
    /// `ark:host/*` + `ark:cap/*` — WASI is a host-internal concern
    /// used inside cap-fn impls, never exposed to the guest.
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

/// Build one `Linker<PluginCtx>` with the unconditional `ark:host/*`
/// trio + the helper type interfaces + only those `ark:cap/*`
/// interfaces named in `caps`.
///
/// The per-interface `add_to_linker` functions come from the
/// `plugin-host` bindgen world (see `crates/ark-host/src/bindings.rs`
/// and the WIT contract in `crates/ark-plugin-protocol/wit/plugin.wit`).
/// The `HasSelf<PluginCtx>` marker is wasmtime 43's convenience
/// `HasData` impl that makes `D::Data<'_> = &'_ mut PluginCtx`, so
/// every generated host-fn glue (e.g. `Host::log(host, …)`) gets a
/// `&mut PluginCtx`.
///
/// WASI is NOT added. See the module preamble for the rationale (F-445
/// — blanket `wasmtime_wasi::p2::add_to_linker_async` would let
/// plugins import `wasi:filesystem` / `wasi:sockets` directly and
/// bypass the `ark:cap/*` coarse gate).
fn build_one_variant(caps: &CapsKey) -> wasmtime::Result<Linker<PluginCtx>> {
    let mut linker: Linker<PluginCtx> = Linker::new(engine());

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
