//! Process-global wasmtime `Engine` singleton.
//!
//! T-PP-025 (Tier 3): `Lazy<Engine>` constructed at first plugin-host
//! access. Asserted feature flags at construction:
//! - `wasm_component_model(true)`
//! - `async_support(true)`
//! - `epoch_interruption(true)`
//! - `consume_fuel(false)` (panic if drift)
//!
//! Tier 0 scaffold — body empty.
