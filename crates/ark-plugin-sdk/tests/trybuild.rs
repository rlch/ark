//! T-PP-023 (cavekit-plugin-protocol R9, R14): compile-fail goldens for
//! `#[derive(Plugin)]` — pins the macro's diagnostic surface so later
//! refactors can't silently relax the checks.
//!
//! Fixtures under `tests/compile-fail/` each exercise one R9/R14 gate:
//!
//! * `invalid_name.rs`          — R9 name regex (`^[a-z][a-z0-9_]*$`).
//! * `invalid_semver.rs`        — R9 semver 2.0.0 parse.
//! * `abi_mismatch.rs`          — R14 strict-equality ABI gate.
//! * `world_name_mismatch.rs`   — R9 WIT world name ↔ plugin name check.
//!
//! # Regenerating `.stderr` goldens
//!
//! ```sh
//! TRYBUILD=overwrite cargo test -p ark-plugin-sdk --test trybuild
//! ```
//!
//! Same pattern used by `crates/scene/tests/view_types_trybuild.rs` +
//! `extensions/claude-code/tests/ui/`.

#[test]
fn derive_plugin_compile_fail() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail/invalid_name.rs");
    t.compile_fail("tests/compile-fail/invalid_semver.rs");
    t.compile_fail("tests/compile-fail/abi_mismatch.rs");
    t.compile_fail("tests/compile-fail/world_name_mismatch.rs");
}
