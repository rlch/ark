//! Capability-gated `ark:cap/*` host-function impls.
//!
//! T-PP-033 (cavekit-plugin-protocol R4): implements the six cap
//! interfaces. Each fn follows the Approach B pattern from cluster 3
//! §3.2:
//!
//! 1. **Coarse gate** — the cap is only added to the `Linker<PluginCtx>`
//!    variant for cap-sets that contain it (T-PP-034). A plugin whose
//!    cap profile lacks `<cap>` never reaches the host-fn body.
//! 2. **Fine gate** — every host-fn body re-checks
//!    [`PluginCtx::granted_caps`] as defence-in-depth. A denial returns
//!    `wasmtime::Error::new(PluginLoadError::CapNotGranted { ... })`
//!    per R4 acceptance "denial returns a nominal error, not a trap".
//!
//! The v1 WIT surface for each cap interface is a single placeholder
//! `ok: func()`. That stub is what this module implements. The full
//! function surfaces (real `fs-read.open` / `network.connect-tcp` /
//! etc.) arrive in later tiers when the cavekit R11 pipe-bus and real
//! I/O plumbing land.
//!
//! # Per-cap status
//!
//! | Cap              | v1 status                                                      |
//! |------------------|----------------------------------------------------------------|
//! | `fs-read`        | WASI preopen wires real read — cap `ok` stub returns `Ok(())`. |
//! | `fs-write`       | WASI preopen wires real write — cap `ok` stub returns `Ok(())`.|
//! | `network`        | STUB — returns `NotImplementedInV1` error.                     |
//! | `spawn-process`  | STUB — returns `NotImplementedInV1` error.                     |
//! | `bus-send`       | STUB — real impl lands in T-PP-054 (Tier 5).                   |
//! | `bus-receive`    | STUB — real impl lands in T-PP-054 (Tier 5).                   |
//!
//! The POINT of this tier is the gate pattern, not real semantics; the
//! body of each "stub" branch is a `wasmtime::Error` with a
//! self-describing message so the guest sees a nominal error rather
//! than a panic.

use ark_plugin_protocol::PluginLoadError;
use wasmtime::Error;

use crate::bindings::ark::plugin::{
    bus_receive, bus_send, fs_read, fs_write, network, spawn_process,
};
use crate::store::PluginCtx;

/// Ensures the plugin was granted the named cap; otherwise returns a
/// structured `wasmtime::Error` wrapping [`PluginLoadError::CapNotGranted`].
///
/// This is the "fine gate" defensive check every cap host-fn body runs
/// before doing any real work.
fn ensure_cap(ctx: &PluginCtx, cap: &str) -> wasmtime::Result<()> {
    if ctx.granted_caps.contains(cap) {
        Ok(())
    } else {
        Err(Error::new(PluginLoadError::CapNotGranted {
            plugin: ctx.plugin_id.clone(),
            missing: vec![cap.to_string()],
        }))
    }
}

/// Convenience: build a "not implemented in v1" error for stub cap
/// bodies. The guest sees this as a nominal error return (not a trap).
fn not_implemented_in_v1(cap: &str) -> Error {
    Error::msg(format!(
        "ark:cap/{cap}: not implemented in v1 — real impl lands in a later tier"
    ))
}

impl fs_read::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "fs-read")?;
        // Real fs-read semantics are provided by the WASI preopen wired
        // in `wasi_ctx_for_caps` (T-PP-035). The `ok()` placeholder
        // function exists only so the import is visible to the R3 caps
        // drift check — a successful call means the gate fired cleanly.
        Ok(())
    }
}

impl fs_write::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "fs-write")?;
        // As with `fs-read`: real fs-write goes through the WASI
        // preopen with upgraded `DirPerms::all()` / `FilePerms::all()`.
        Ok(())
    }
}

impl network::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "network")?;
        // v1 stub — real `connect-tcp` / `resolve-dns` surface lands
        // in a later tier once the kit R4 network sub-contract is
        // specified.
        Err(not_implemented_in_v1("network"))
    }
}

impl spawn_process::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "spawn-process")?;
        // v1 stub — real `std::process::Command` wiring lands once
        // the kit specifies argument / environment scrubbing rules
        // for sandboxed process spawning.
        Err(not_implemented_in_v1("spawn-process"))
    }
}

impl bus_send::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "bus-send")?;
        // v1 stub — real `PipeMessage` delivery lands in T-PP-054
        // (Tier 5) when the pipe-bus is wired through WIT resources.
        Err(not_implemented_in_v1("bus-send"))
    }
}

impl bus_receive::Host for PluginCtx {
    async fn ok(&mut self) -> wasmtime::Result<()> {
        ensure_cap(self, "bus-receive")?;
        // v1 stub — see bus-send.
        Err(not_implemented_in_v1("bus-receive"))
    }
}
