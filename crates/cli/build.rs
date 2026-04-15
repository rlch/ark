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
//! ## Why inline `cargo build` is opt-in (T-130)
//!
//! Running `cargo build --target wasm32-wasip1 ...` from inside a
//! build.rs that is itself driven by a `cargo build --workspace`
//! introduces lock-file / target-dir contention (the inner and outer
//! cargo fight over `target/` and `Cargo.lock`), and on some hosts
//! deadlocks on the jobserver. T-130 therefore keeps the inline build
//! **opt-in** behind `ARK_BUILD_WASM=1`, and isolates the nested build
//! in `$OUT_DIR/wasm-target/` so the outer workspace `target/` is
//! untouched. Without the opt-in, build.rs behaves exactly like the
//! T-098/T-109 state: discover an already-built artifact or fall back
//! to a zero-byte placeholder plus a `cargo:warning`.
//!
//! The recommended path for most users is either the `just wasm`
//! convenience target at the repo root, or running `cargo build
//! --target wasm32-wasip1 --release -p ark-plugin-status -p
//! ark-plugin-picker` manually before `cargo build -p ark-cli`. CI
//! and `cargo-dist` release pipelines (T-133) pre-build the wasm
//! outside build.rs.
//!
//! ## T-131 — wasm size-reduction stack
//!
//! `[profile.release]` in the workspace `Cargo.toml` applies the
//! size-optimization stack mandated by `cavekit-distribution.md` R3:
//! `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `strip = true`,
//! `panic = "abort"`. The default-features audit for plugin deps lives
//! in `crates/plugins/{status,picker}/Cargo.toml`.
//!
//! After the wasm artifact is discovered, `embed_plugin` optionally
//! runs `wasm-opt -Oz --enable-bulk-memory` as a postprocess shrink
//! pass when the `binaryen` `wasm-opt` binary is on `PATH`. This is
//! pure icing on top of the `rustc` size stack — if `wasm-opt` is
//! absent we print a `cargo:warning` and embed the unoptimized
//! artifact as-is.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    // T-130: opt-in inline build toggle. Flipping the env var must
    // re-invoke build.rs so the nested `cargo build` runs.
    println!("cargo:rerun-if-env-changed=ARK_BUILD_WASM");

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

    // T-130: opt-in inline wasm build. When ARK_BUILD_WASM=1, run
    // `cargo build --target wasm32-wasip1 --release -p <plugin>` in an
    // isolated target dir under $OUT_DIR and redirect the artifact
    // lookup there. Otherwise, fall through to the discover-or-
    // placeholder path that has been in place since T-098/T-109.
    let inline_target_dir = out_dir.join("wasm-target");
    let effective_target_dir = if inline_build_enabled() {
        let ok_status = maybe_build_wasm(
            workspace_root,
            &inline_target_dir,
            "ark-plugin-status",
            "ark_plugin_status.wasm",
        );
        let ok_picker = maybe_build_wasm(
            workspace_root,
            &inline_target_dir,
            "ark-plugin-picker",
            "ark_plugin_picker.wasm",
        );
        if ok_status && ok_picker {
            // Both built cleanly — embed from the isolated target dir.
            inline_target_dir.clone()
        } else {
            // Partial/failed inline build: warn and fall back to the
            // caller-visible target dir so a previously-built artifact
            // (if any) still gets embedded.
            println!(
                "cargo:warning=ARK_BUILD_WASM=1 inline build failed for at least one plugin; \
                 falling back to discover-or-placeholder from {}",
                target_dir.display()
            );
            target_dir.clone()
        }
    } else {
        target_dir.clone()
    };

    embed_plugin(
        &effective_target_dir,
        &wasm_out_dir,
        "ark_plugin_status.wasm",
        "ark-plugin-status.wasm",
        "ark-plugin-status",
    );
    embed_plugin(
        &effective_target_dir,
        &wasm_out_dir,
        "ark_plugin_picker.wasm",
        "ark-plugin-picker.wasm",
        "ark-plugin-picker",
    );
}

/// T-130: `true` when the operator explicitly opted into the inline
/// wasm build via `ARK_BUILD_WASM=1`. Anything else (unset, `0`,
/// empty string, arbitrary text) leaves the nested `cargo build`
/// disabled so default `cargo build --workspace` invocations stay
/// safe and deadlock-free.
fn inline_build_enabled() -> bool {
    env::var("ARK_BUILD_WASM")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// T-130: opt-in nested `cargo build --target wasm32-wasip1 --release
/// -p <plugin_pkg>`. Returns `true` if the artifact is present at the
/// expected path after this function runs (either because it was
/// already fresh or because the nested build succeeded), `false`
/// otherwise. Never panics — a failure prints `cargo:warning` and
/// returns `false` so the caller can fall back to the placeholder.
///
/// Safeguards:
/// - Uses an isolated `CARGO_TARGET_DIR` (`$OUT_DIR/wasm-target`) so
///   the outer workspace `target/` is never clobbered.
/// - Skips if the artifact is already newer than every tracked
///   source file in the plugin crate (simple mtime check).
/// - Clears `CARGO_MAKEFLAGS` / `MAKEFLAGS` in the child env to avoid
///   inheriting a jobserver fd the nested cargo can't use.
fn maybe_build_wasm(
    workspace_root: &Path,
    inline_target_dir: &Path,
    plugin_pkg: &str,
    artifact_name: &str,
) -> bool {
    let artifact = inline_target_dir
        .join("wasm32-wasip1")
        .join("release")
        .join(artifact_name);

    // Resolve the plugin crate source dir. `ark-plugin-status` lives
    // at `crates/plugins/status`, `ark-plugin-picker` at
    // `crates/plugins/picker`.
    let subdir = plugin_pkg.trim_start_matches("ark-plugin-");
    let plugin_src = workspace_root.join("crates").join("plugins").join(subdir);

    if artifact_is_fresh(&artifact, &plugin_src) {
        println!(
            "cargo:warning=ARK_BUILD_WASM=1: {plugin_pkg} artifact already fresh at {}",
            artifact.display()
        );
        return true;
    }

    println!(
        "cargo:warning=ARK_BUILD_WASM=1: building {plugin_pkg} (target dir: {})",
        inline_target_dir.display()
    );

    let mut cmd = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(workspace_root)
        .arg("build")
        .arg("--target")
        .arg("wasm32-wasip1")
        .arg("--release")
        .arg("-p")
        .arg(plugin_pkg)
        .env("CARGO_TARGET_DIR", inline_target_dir)
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS");

    match cmd.status() {
        Ok(status) if status.success() => {
            if artifact.is_file() {
                true
            } else {
                println!(
                    "cargo:warning=ARK_BUILD_WASM=1: {plugin_pkg} build succeeded but artifact \
                     missing at {} — falling back to placeholder",
                    artifact.display()
                );
                false
            }
        }
        Ok(status) => {
            println!(
                "cargo:warning=ARK_BUILD_WASM=1: `cargo build -p {plugin_pkg} --target \
                 wasm32-wasip1 --release` exited {status}; falling back to placeholder"
            );
            false
        }
        Err(e) => {
            println!(
                "cargo:warning=ARK_BUILD_WASM=1: failed to spawn cargo for {plugin_pkg}: {e}; \
                 falling back to placeholder"
            );
            false
        }
    }
}

/// Cheap mtime-based freshness check. The nested `cargo build` itself
/// is incremental, so this guard mostly exists to avoid re-spawning
/// cargo on every ark-cli rebuild when nothing changed.
fn artifact_is_fresh(artifact: &Path, plugin_src: &Path) -> bool {
    let Ok(art_meta) = fs::metadata(artifact) else {
        return false;
    };
    let Ok(art_mtime) = art_meta.modified() else {
        return false;
    };

    let newest_src = walk_newest_mtime(plugin_src).unwrap_or(art_mtime);
    art_mtime >= newest_src
}

/// Return the newest `SystemTime` mtime among regular files under
/// `root`. Returns `None` if `root` doesn't exist or nothing is
/// readable — callers treat that as "can't prove staleness, assume
/// the artifact is fresh enough".
fn walk_newest_mtime(root: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            // Skip nested target dirs in case a contributor built inside
            // the plugin crate directly — they're artifacts, not sources.
            if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        } else if meta.is_file() {
            if let Ok(m) = meta.modified() {
                newest = Some(match newest {
                    Some(n) if n >= m => n,
                    _ => m,
                });
            }
        }
    }
    newest
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
                // T-131: optional wasm-opt postprocess on the embedded copy.
                // Leaves the source artifact in `target/` untouched.
                maybe_wasm_opt(&dest, display_name, bytes);
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

/// T-131: best-effort `wasm-opt -Oz --enable-bulk-memory` postprocess.
/// Runs only if `wasm-opt` is on `PATH`; otherwise emits a
/// `cargo:warning` and leaves the embedded artifact unmodified.
/// Failures never fail the build — we just embed the unoptimized bytes.
///
/// `before_bytes` is the size of `dest` as copied, used for the
/// shrink-report `cargo:warning`.
fn maybe_wasm_opt(dest: &Path, display_name: &str, before_bytes: u64) {
    // `Command::new("wasm-opt")` relies on PATH lookup. Probe with
    // `--version` first so we can emit a friendly skip message instead
    // of a cryptic spawn error when the binary is missing.
    let probe = Command::new("wasm-opt").arg("--version").output();
    match probe {
        Ok(p) if p.status.success() => {}
        _ => {
            println!(
                "cargo:warning=wasm-opt not found on PATH, skipping post-build optimization for \
                 {display_name} (install binaryen to shrink further)"
            );
            return;
        }
    }

    // wasm-opt rewrites in-place when input == output, but it's safer
    // to write to a sibling tempfile and rename on success so a failed
    // run can't leave a half-written wasm in place.
    let tmp = dest.with_extension("wasm.opt.tmp");
    let status = Command::new("wasm-opt")
        .arg("-Oz")
        .arg("--enable-bulk-memory")
        .arg(dest)
        .arg("-o")
        .arg(&tmp)
        .status();

    match status {
        Ok(s) if s.success() => {
            if let Err(e) = fs::rename(&tmp, dest) {
                println!(
                    "cargo:warning=wasm-opt succeeded but rename {} → {} failed: {e}; keeping \
                     unoptimized bytes",
                    tmp.display(),
                    dest.display()
                );
                let _ = fs::remove_file(&tmp);
                return;
            }
            let after = fs::metadata(dest).map(|m| m.len()).unwrap_or(before_bytes);
            let saved = before_bytes.saturating_sub(after);
            let pct = if before_bytes > 0 {
                (saved as f64 / before_bytes as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "cargo:warning=wasm-opt -Oz shrank {display_name}: {before_bytes} → {after} \
                 bytes (-{saved}, -{pct:.1}%)"
            );
        }
        Ok(s) => {
            println!(
                "cargo:warning=wasm-opt exited {s} on {}; keeping unoptimized bytes",
                dest.display()
            );
            let _ = fs::remove_file(&tmp);
        }
        Err(e) => {
            println!(
                "cargo:warning=failed to spawn wasm-opt for {}: {e}; keeping unoptimized bytes",
                dest.display()
            );
            let _ = fs::remove_file(&tmp);
        }
    }
}
