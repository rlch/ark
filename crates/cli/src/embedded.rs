//! Embedded wasm plugin bytes (T-098 status, T-109 picker;
//! cavekit-plugin-status R5, cavekit-plugin-picker R1,
//! cavekit-distribution R3).
//!
//! The build script copies (or falls back to zero-byte placeholders
//! for) each plugin's wasm into `$OUT_DIR/wasm/`. We embed them here
//! via `include_bytes!` so `ark doctor --fix` can materialize the
//! plugins at `~/.config/ark/plugins/<name>.wasm` with no network
//! access.
//!
//! When a wasm artifact is unavailable at build time (developer
//! doesn't have the `wasm32-wasip1` target installed), the build
//! script writes an empty placeholder. The matching
//! `<PLUGIN>_WASM_AVAILABLE` flag is `false` in that case and
//! `doctor` degrades to a no-op for that check.

/// Raw wasm bytes for `ark-plugin-status`. Empty slice when the
/// build ran without the wasm target installed (see build.rs).
pub const STATUS_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/wasm/ark-plugin-status.wasm"));

/// `true` when the build embedded a real status wasm artifact;
/// `false` when it fell back to the empty placeholder. Doctor
/// consults this to decide whether to run the plugin-install check.
pub const STATUS_WASM_AVAILABLE: bool = !STATUS_WASM.is_empty();

/// Raw wasm bytes for `ark-plugin-picker` (T-109). Empty slice when
/// the build ran without the wasm target installed (see build.rs).
pub const PICKER_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/wasm/ark-plugin-picker.wasm"));

/// `true` when the build embedded a real picker wasm artifact;
/// `false` when it fell back to the empty placeholder.
pub const PICKER_WASM_AVAILABLE: bool = !PICKER_WASM.is_empty();
