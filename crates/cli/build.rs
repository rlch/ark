//! Build script for `ark-cli` — wasm plugin embedding (T-098, T-109).
//!
//! Embeds the `ark-plugin-status` and `ark-plugin-picker` wasm
//! artifacts into the `ark` binary so `ark doctor --fix` can write
//! the plugins to the user's zellij plugins dir without requiring a
//! separate install step. See `context/kits/cavekit-plugin-status.md`
//! R5, `context/kits/cavekit-plugin-picker.md` R1, and
//! `context/kits/cavekit-distribution.md` R3.
//!
//! # Two code paths
//!
//! 1. **Real artifact** — if a prebuilt wasm exists at
//!    `target/wasm32-wasip1/release/<plugin>.wasm` (relative to the
//!    workspace root) we copy it into `$OUT_DIR/wasm/`. Distribution
//!    / release builds produce it via a dedicated
//!    `cargo build --target wasm32-wasip1 --release -p <plugin>`
//!    step before invoking the top-level `cargo build`. That outer
//!    orchestration is owned by T-130.
//!
//! 2. **Placeholder** — if the artifact is absent (the common case
//!    when someone runs `cargo build --workspace` on a machine without
//!    the wasm32-wasip1 target installed) we write a zero-byte
//!    placeholder so `include_bytes!` still compiles, and emit a
//!    `cargo:warning` pointing at the artifact path. The embedded
//!    module exposes `<PLUGIN>_WASM_AVAILABLE` so `doctor` skips the
//!    check cleanly when the binary ships without a real plugin.
//!
//! ## Why we don't invoke `cargo` from build.rs
//!
//! Running `cargo build --target wasm32-wasip1 ...` from inside a
//! build.rs that is itself driven by a `cargo build --workspace`
//! introduces lock-file / target-dir contention (the inner and outer
//! cargo fight over `target/` and `Cargo.lock`), and on some hosts
//! deadlocks on the jobserver. T-098/T-109 keep the build.rs side
//! purely about discovering an already-built artifact; orchestrating
//! the wasm build itself is T-130's responsibility.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    // Re-run when either plugin source changes so developers iterating
    // on a plugin crate pick up new bytes on the next `cargo build`
    // of the CLI.
    println!("cargo:rerun-if-changed=../plugins/status/src");
    println!("cargo:rerun-if-changed=../plugins/status/Cargo.toml");
    println!("cargo:rerun-if-changed=../plugins/picker/src");
    println!("cargo:rerun-if-changed=../plugins/picker/Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    // F-615: CARGO_TARGET_DIR / `--target-dir` may move the wasm
    // artifact out of `<workspace>/target/`. Re-run when it changes.
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let wasm_out_dir = out_dir.join("wasm");
    fs::create_dir_all(&wasm_out_dir).expect("create $OUT_DIR/wasm");

    // Locate the workspace target/ directory. CARGO_MANIFEST_DIR points
    // at `crates/cli`; walk up two levels for the workspace root.
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");

    // F-615: `cargo` sets CARGO_TARGET_DIR in the build-script env
    // whenever the operator passes `--target-dir` or sets it via
    // `.cargo/config.toml` / the env directly. Honor it so the wasm
    // artifact is found in the actual target directory instead of
    // the hardcoded `<workspace>/target/`.
    let target_dir = wasm_target_dir(workspace_root);

    // F-608: also watch the wasm release directory so a freshly-appeared
    // artifact re-triggers build.rs even when no plugin source changed.
    // Without this, a CLI built before the wasm target was compiled would
    // embed the zero-byte placeholder; a later `cargo build --target
    // wasm32-wasip1 --release -p <plugin>` wouldn't touch any input
    // cargo was tracking, so the next `cargo build -p ark-cli` would
    // keep the stale placeholder. cargo tracks the mtime of the path we
    // name even if it doesn't exist yet — when the file appears cargo
    // will re-invoke us and `embed_plugin` picks up the real bytes.
    let wasm_release_dir = target_dir.join("wasm32-wasip1").join("release");
    println!("cargo:rerun-if-changed={}", wasm_release_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        wasm_release_dir.join("ark_plugin_status.wasm").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wasm_release_dir.join("ark_plugin_picker.wasm").display()
    );

    embed_plugin(
        &target_dir,
        &wasm_out_dir,
        "ark_plugin_status.wasm",
        "ark-plugin-status.wasm",
        "ark-plugin-status",
    );
    embed_plugin(
        &target_dir,
        &wasm_out_dir,
        "ark_plugin_picker.wasm",
        "ark-plugin-picker.wasm",
        "ark-plugin-picker",
    );
}

/// F-615: resolve the base directory containing `wasm32-wasip1/release/`.
/// When `CARGO_TARGET_DIR` is set we use it verbatim; otherwise we fall
/// back to the legacy `<workspace>/target/` location.
fn wasm_target_dir(workspace_root: &Path) -> PathBuf {
    if let Some(env_dir) = env::var_os("CARGO_TARGET_DIR") {
        let p = PathBuf::from(env_dir);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    workspace_root.join("target")
}

/// Copy the artifact from `<target>/wasm32-wasip1/release/<src_name>`
/// into `$OUT_DIR/wasm/<dest_name>`, or fall back to a zero-byte
/// placeholder. `display_name` is used for cargo:warning messages.
///
/// F-615: `target_dir` is the resolved `CARGO_TARGET_DIR` (or the
/// legacy `<workspace>/target/` fallback), not the workspace root.
fn embed_plugin(
    target_dir: &Path,
    wasm_out_dir: &Path,
    src_name: &str,
    dest_name: &str,
    display_name: &str,
) {
    let dest = wasm_out_dir.join(dest_name);
    let artifact = target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join(src_name);

    if artifact.is_file() {
        match fs::copy(&artifact, &dest) {
            Ok(bytes) => {
                println!(
                    "cargo:warning=embedded {} ({} bytes) from {}",
                    display_name,
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
            "cargo:warning={} wasm not found at {}; embedding empty placeholder. \
             Run `cargo build --target wasm32-wasip1 --release -p {}` before the \
             release build to ship a real plugin (see cavekit-distribution.md R3).",
            display_name,
            artifact.display(),
            display_name,
        );
        write_placeholder(&dest);
    }
}

fn write_placeholder(dest: &std::path::Path) {
    // Empty file — `<PLUGIN>_WASM_AVAILABLE` becomes false and doctor
    // skips the check when the build shipped without a real plugin.
    fs::write(dest, b"").expect("write placeholder wasm");
}
