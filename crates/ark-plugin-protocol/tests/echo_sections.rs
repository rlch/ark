//! T-PP-022 (cavekit-plugin-protocol R2, R3, R9): end-to-end CI gate
//! that compiles the `examples/echo` plugin as `wasm32-wasip2` and
//! asserts the built `.wasm` carries BOTH the `ark-caps:v1` and the
//! `ark-meta:v1` custom sections.
//!
//! # Running
//!
//! This test is `#[ignore]` by default because it requires:
//!
//! 1. The `wasm32-wasip2` rustc target to be installed (`rustup target
//!    add wasm32-wasip2`).
//! 2. Cargo to be able to rebuild the echo example (`cargo build
//!    --manifest-path examples/echo/Cargo.toml --target wasm32-wasip2
//!    --release`).
//!
//! Neither is guaranteed on a clean workspace test run. CI opts in
//! explicitly with:
//!
//! ```sh
//! cargo test -p ark-plugin-protocol -- --ignored
//! ```
//!
//! If the `wasm32-wasip2` target is not installed, the test skips with
//! a printed reason rather than failing — it's still a signal when
//! someone runs `--ignored` locally.

use std::path::PathBuf;
use std::process::Command;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns true if `rustc --print target-list` knows about `wasm32-wasip2`.
/// Used to skip (not fail) when the target isn't installed locally.
fn wasm32_wasip2_available() -> bool {
    let out = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.lines().any(|l| l.trim() == "wasm32-wasip2")
        }
        _ => false,
    }
}

#[test]
#[ignore = "requires wasm32-wasip2 target + rebuilds the echo example; run with --ignored in CI"]
fn echo_wasm_has_caps_and_meta_sections() {
    if !wasm32_wasip2_available() {
        eprintln!(
            "SKIPPING: wasm32-wasip2 target not installed. Run \
             `rustup target add wasm32-wasip2` to enable this gate."
        );
        return;
    }

    let echo_manifest = crate_root()
        .join("examples")
        .join("echo")
        .join("Cargo.toml");

    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-wasip2")
        .arg("--manifest-path")
        .arg(&echo_manifest)
        .status()
        .expect("invoke cargo to build echo");
    assert!(
        status.success(),
        "cargo build of examples/echo for wasm32-wasip2 failed"
    );

    // The echo Cargo.toml isn't a workspace member, so its target
    // directory is adjacent to its own Cargo.toml: `examples/echo/target/wasm32-wasip2/release/echo.wasm`.
    let wasm_path = crate_root()
        .join("examples")
        .join("echo")
        .join("target")
        .join("wasm32-wasip2")
        .join("release")
        .join("echo.wasm");
    assert!(
        wasm_path.exists(),
        "expected echo.wasm at {}",
        wasm_path.display()
    );

    let bytes = std::fs::read(&wasm_path).expect("read built echo.wasm");
    let sections = collect_custom_sections(&bytes);
    assert!(
        sections.iter().any(|n| n == "ark-caps:v1"),
        "missing ark-caps:v1 custom section in {}; found: {sections:?}",
        wasm_path.display()
    );
    assert!(
        sections.iter().any(|n| n == "ark-meta:v1"),
        "missing ark-meta:v1 custom section in {}; found: {sections:?}",
        wasm_path.display()
    );
}

/// Collect every `wasmparser` custom-section name found in a component
/// or core-module payload. Uses the workspace's own `wasmparser` pin
/// (the protocol crate already depends on `postcard`; `wasmparser` is
/// declared as a dev-dep below).
fn collect_custom_sections(bytes: &[u8]) -> Vec<String> {
    use wasmparser_plugin_host::{Parser, Payload};

    let mut out = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        let payload = payload.expect("parse wasm payload");
        if let Payload::CustomSection(r) = payload {
            out.push(r.name().to_string());
        }
    }
    out
}
