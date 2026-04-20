//! 3-phase plugin loader.
//!
//! T-PP-035..T-PP-038 (Tier 3):
//! - Phase 1 (Inspect): `wasmparser` reads custom sections
//!   (`ark-meta:v1`, `ark-caps:v1`) BEFORE `Component::new`.
//! - Phase 2 (Compile): import-vs-section cross-check + user-grant
//!   subset check + `instantiate_pre` produces a cached
//!   `InstancePre<PluginCtx>`.
//! - Phase 3 (Instantiate): build default-deny `WasiCtx` + `Store` +
//!   dispatch `on-install` / `load` lifecycle hooks.
//!
//! Tier 0 scaffold — body empty.
