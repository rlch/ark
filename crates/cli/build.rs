//! Build script for `ark-cli` — wasm plugin embedding (T-098).
//!
//! Embeds the `ark-plugin-status` wasm artifact into the `ark` binary
//! so `ark doctor --fix` can write the plugin to the user's zellij
//! plugins dir without requiring a separate install step. See
//! `context/kits/cavekit-plugin-status.md` R5 and
//! `context/kits/cavekit-distribution.md` R3.
//!
//! # Two code paths
//!
//! 1. **Real artifact** — if a prebuilt wasm exists at
//!    `target/wasm32-wasip1/release/ark_plugin_status.wasm` (relative
//!    to the workspace root) we copy it into `$OUT_DIR/wasm/`.
//!    Distribution / release builds produce it via a dedicated
//!    `cargo build --target wasm32-wasip1 --release -p ark-plugin-status`
//!    step before invoking the top-level `cargo build`. That outer
//!    orchestration is owned by T-130.
//!
//! 2. **Placeholder** — if the artifact is absent (the common case
//!    when someone runs `cargo build --workspace` on a machine without
//!    the wasm32-wasip1 target installed) we write a zero-byte
//!    placeholder so `include_bytes!` still compiles, and emit a
//!    `cargo:warning` pointing at the artifact path. The embedded
//!    module exposes `STATUS_WASM_AVAILABLE` so `doctor` skips the
//!    check cleanly when the binary ships without a real plugin.
//!
//! ## Why we don't invoke `cargo` from build.rs
//!
//! Running `cargo build --target wasm32-wasip1 ...` from inside a
//! build.rs that is itself driven by a `cargo build --workspace`
//! introduces lock-file / target-dir contention (the inner and outer
//! cargo fight over `target/` and `Cargo.lock`), and on some hosts
//! deadlocks on the jobserver. T-098 keeps the build.rs side purely
//! about discovering an already-built artifact; orchestrating the
//! wasm build itself is T-130's responsibility.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Re-run when the plugin source changes so developers iterating
    // on the plugin crate pick up new bytes on the next `cargo build`
    // of the CLI.
    println!("cargo:rerun-if-changed=../plugins/status/src");
    println!("cargo:rerun-if-changed=../plugins/status/Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let wasm_out_dir = out_dir.join("wasm");
    fs::create_dir_all(&wasm_out_dir).expect("create $OUT_DIR/wasm");
    let dest = wasm_out_dir.join("ark-plugin-status.wasm");

    // Locate the workspace target/ directory. CARGO_MANIFEST_DIR points
    // at `crates/cli`; walk up two levels for the workspace root.
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let artifact = workspace_root
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join("ark_plugin_status.wasm");

    if artifact.is_file() {
        match fs::copy(&artifact, &dest) {
            Ok(bytes) => {
                println!(
                    "cargo:warning=embedded ark-plugin-status.wasm ({} bytes) from {}",
                    bytes,
                    artifact.display()
                );
            }
            Err(e) => {
                // Fall back to placeholder so the build still succeeds.
                println!(
                    "cargo:warning=failed to copy {} → {}: {e}; writing placeholder",
                    artifact.display(),
                    dest.display(),
                );
                write_placeholder(&dest);
            }
        }
    } else {
        println!(
            "cargo:warning=ark-plugin-status wasm not found at {}; embedding empty placeholder. \
             Run `cargo build --target wasm32-wasip1 --release -p ark-plugin-status` before \
             the release build to ship a real plugin (see cavekit-distribution.md R3).",
            artifact.display()
        );
        write_placeholder(&dest);
    }
}

fn write_placeholder(dest: &std::path::Path) {
    // Empty file — `STATUS_WASM_AVAILABLE` becomes false and doctor
    // skips the check when the build shipped without a real plugin.
    fs::write(dest, b"").expect("write placeholder wasm");
}
