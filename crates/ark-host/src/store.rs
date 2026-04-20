//! Per-plugin `Store<PluginCtx>` factory + `PluginCtx` struct.
//!
//! T-PP-028 (Tier 3): sets `set_epoch_deadline(2)` +
//! `epoch_deadline_async_yield_and_update(2)` at construction. Owns a
//! default-deny `WasiCtx` per T-PP-029.
//!
//! Tier 0 scaffold — body empty.
