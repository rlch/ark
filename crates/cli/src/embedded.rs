//! Embedded wasm plugin bytes (T-098, cavekit-plugin-status R5,
//! cavekit-distribution R3).
//!
//! The build script copies (or falls back to a zero-byte placeholder
//! for) `ark-plugin-status.wasm` into `$OUT_DIR/wasm/`. We embed it
//! here via `include_bytes!` so `ark doctor --fix` can materialize
//! the plugin at `~/.config/ark/plugins/ark-status.wasm` with no
//! network access.
//!
//! When the wasm artifact is unavailable at build time (developer
//! doesn't have the `wasm32-wasip1` target installed), the build
//! script writes an empty placeholder. `STATUS_WASM_AVAILABLE` is
//! `false` in that case and `doctor` degrades to a no-op for this
//! check.

/// Raw wasm bytes for `ark-plugin-status`. Empty slice when the
/// build ran without the wasm target installed (see build.rs).
pub const STATUS_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/wasm/ark-plugin-status.wasm"));

/// `true` when the build embedded a real wasm artifact; `false` when
/// it fell back to the empty placeholder. Doctor consults this to
/// decide whether to run the plugin-install check.
pub const STATUS_WASM_AVAILABLE: bool = !STATUS_WASM.is_empty();
