//! `ark-host` — wasmtime-based plugin runtime substrate.
//!
//! T-PP-004 (cavekit-plugin-protocol R1): Tier 0 scaffold. Real
//! wasmtime wiring lands in Tier 3 (T-PP-025..T-PP-036).

pub mod cache;
pub mod engine;
pub mod lifecycle;
pub mod linker_set;
pub mod loader;
pub mod store;
