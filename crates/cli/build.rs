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
//! ## Why inline `cargo build` is default-on (F-709 / T-130)
//!
//! Running `cargo build --target wasm32-wasip1 ...` from inside a
//! build.rs that is itself driven by a `cargo build --workspace`
//! introduces lock-file / target-dir contention (the inner and outer
//! cargo fight over `target/` and `Cargo.lock`), and on some hosts
//! deadlocks on the jobserver. T-130 originally kept the inline build
//! opt-in behind `ARK_BUILD_WASM=1`, on the theory that `cargo install
//! ark-cli` users would have the wasm target pre-built.
//!
//! F-709 inverted that default: `cargo install ark-cli` has no notion
//! of pre-staging wasm artifacts, so the opt-in default produced a
//! binary with zero-byte placeholder wasm, and `ark doctor` reported
//! plugins unavailable. The documented install path was silently broken.
//!
//! The inline build is therefore now **default-on** with two safety
//! rails that keep `cargo build --workspace` deadlock-free:
//!
//! - The nested build uses an isolated `CARGO_TARGET_DIR`
//!   (`$OUT_DIR/wasm-target/`), so the outer workspace `target/` is
//!   never touched — eliminating the jobserver / lock-file contention
//!   that motivated the original opt-in.
//! - If the `wasm32-wasip1` rustup target is **not installed** we skip
//!   the nested build entirely and fall back to the discover-or-
//!   placeholder path, emitting a `cargo:warning` that tells the user
//!   how to enable real plugins (`rustup target add wasm32-wasip1`).
//!
//! Operators who want the legacy "never spawn nested cargo" behaviour
//! can opt OUT by setting `ARK_BUILD_WASM=0`.
//!
//! CI and `cargo-dist` release pipelines (T-133) install the wasm
//! target up front, so this path works cleanly there. The `just wasm`
//! convenience target at the repo root still works for local devs who
//! prefer an explicit pre-build step.
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

/// F-704: source roots the plugin crates transitively depend on.
///
/// The freshness check in [`artifact_is_fresh`] walks every path in this
/// list (plus the plugin's own `src/`) and compares the newest mtime
/// against the embedded artifact's mtime. Without this, edits to a
/// workspace crate the plugin depends on leave a stale wasm embedded
/// on the next `cargo build -p ark-cli` because cargo has no reason to
/// re-run build.rs when only a transitive-source file changed.
///
/// Hard-coded rather than parsed from `Cargo.toml` for simplicity: the
/// plugin dep graph is small and shifts deliberately. Extend this list
/// whenever a plugin picks up a new `path = "..."` workspace dep.
const STATUS_PLUGIN_DEP_ROOTS: &[&str] = &["crates/types/src"];
const PICKER_PLUGIN_DEP_ROOTS: &[&str] = &["crates/types/src"];

fn main() {
    // Re-run when either plugin source changes so developers iterating
    // on a plugin crate pick up new bytes on the next `cargo build`
    // of the CLI.
    println!("cargo:rerun-if-changed=../plugins/status/src");
    println!("cargo:rerun-if-changed=../plugins/status/Cargo.toml");
    println!("cargo:rerun-if-changed=../plugins/picker/src");
    println!("cargo:rerun-if-changed=../plugins/picker/Cargo.toml");
    // F-704: also re-run when any transitively-depended workspace
    // crate's source changes. `cargo:rerun-if-changed` wants absolute
    // paths relative to CARGO_MANIFEST_DIR — `crates/cli/build.rs`
    // lives at the workspace root's `crates/cli/`, so `../../<path>`
    // resolves the workspace-relative source root.
    for root in STATUS_PLUGIN_DEP_ROOTS
        .iter()
        .chain(PICKER_PLUGIN_DEP_ROOTS.iter())
    {
        println!("cargo:rerun-if-changed=../../{root}");
    }
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

    // F-709 / T-130: inline wasm build is default-on with a rustup
    // target precheck. When the operator has NOT opted out
    // (`ARK_BUILD_WASM=0`) AND the `wasm32-wasip1` rustup target is
    // installed, run `cargo build --target wasm32-wasip1 --release -p
    // <plugin>` in an isolated target dir under $OUT_DIR and redirect
    // the artifact lookup there. Otherwise fall through to the
    // discover-or-placeholder path that has been in place since
    // T-098/T-109.
    let inline_target_dir = out_dir.join("wasm-target");
    let effective_target_dir = if inline_build_enabled() {
        if !wasm_target_installed() {
            // F-709: wasm32-wasip1 rustup target missing. Don't try to
            // spawn nested cargo — the build would fail with a target-
            // not-found error. Warn loudly so `cargo install ark-cli`
            // users know exactly how to enable real plugins.
            println!(
                "cargo:warning=ark-cli build.rs: wasm32-wasip1 rustup target not installed; \
                 embedding zero-byte placeholders. To ship real plugins: \
                 `rustup target add wasm32-wasip1` and rebuild. \
                 (Set ARK_BUILD_WASM=0 to silence this message.)"
            );
            target_dir.clone()
        } else {
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
                    "cargo:warning=inline wasm build failed for at least one plugin; \
                     falling back to discover-or-placeholder from {}",
                    target_dir.display()
                );
                target_dir.clone()
            }
        }
    } else {
        // Operator opted out via ARK_BUILD_WASM=0. Legacy path:
        // discover a pre-built artifact or embed a placeholder.
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

/// F-709 / T-130: `true` when the inline wasm build should run. The
/// default is **on** — `cargo install ark-cli` needs this so it ships
/// real plugins. Operators can opt OUT explicitly by setting
/// `ARK_BUILD_WASM=0` (legacy placeholder-only behaviour). Any other
/// value (unset, `1`, empty, arbitrary text) keeps the default-on path.
///
/// The deadlock / target-dir contention concerns that originally kept
/// this opt-in are mitigated by:
/// - the nested build using an isolated `CARGO_TARGET_DIR`
///   (`$OUT_DIR/wasm-target/`),
/// - the `wasm32-wasip1` rustup-target precheck which skips the
///   nested cargo invocation entirely when the target is missing
///   (the common `cargo install ark-cli` bare-machine case).
fn inline_build_enabled() -> bool {
    match env::var("ARK_BUILD_WASM") {
        Ok(v) if v == "0" => false,
        _ => true,
    }
}

/// F-709: true when the `wasm32-wasip1` rustup target is installed on
/// the host toolchain. Detection parses `rustup target list --installed`
/// (fast — just reads rustup's local state, no network, no compile).
///
/// Returns `false` when:
/// - `rustup` is not on PATH (user is on a pinned toolchain outside
///   rustup, e.g. system rustc — we can't verify and must not risk
///   the nested cargo spawn),
/// - the command errors, or
/// - `wasm32-wasip1` does not appear in the installed list.
///
/// The `false` return causes build.rs to skip the nested cargo and
/// embed placeholders, with a `cargo:warning` telling the user how to
/// enable real plugins.
fn wasm_target_installed() -> bool {
    let output = Command::new("rustup")
        .arg("target")
        .arg("list")
        .arg("--installed")
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().any(|line| line.trim() == "wasm32-wasip1")
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

    // F-704: freshness must also consider transitively-depended
    // workspace crates. Without this an edit to e.g. `crates/types/src`
    // leaves the previous wasm artifact embedded until the plugin's
    // own source changes.
    let dep_roots = plugin_dep_roots(workspace_root, plugin_pkg);
    if artifact_is_fresh(&artifact, &plugin_src, &dep_roots) {
        println!(
            "cargo:warning=inline wasm build: {plugin_pkg} artifact already fresh at {}",
            artifact.display()
        );
        return true;
    }

    println!(
        "cargo:warning=inline wasm build: building {plugin_pkg} (target dir: {})",
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
                    "cargo:warning=inline wasm build: {plugin_pkg} build succeeded but artifact \
                     missing at {} — falling back to placeholder",
                    artifact.display()
                );
                false
            }
        }
        Ok(status) => {
            println!(
                "cargo:warning=inline wasm build: `cargo build -p {plugin_pkg} --target \
                 wasm32-wasip1 --release` exited {status}; falling back to placeholder"
            );
            false
        }
        Err(e) => {
            println!(
                "cargo:warning=inline wasm build: failed to spawn cargo for {plugin_pkg}: {e}; \
                 falling back to placeholder"
            );
            false
        }
    }
}

/// Cheap mtime-based freshness check. The nested `cargo build` itself
/// is incremental, so this guard mostly exists to avoid re-spawning
/// cargo on every ark-cli rebuild when nothing changed.
///
/// F-704: walks the plugin's own `src/` *and* each transitive dep root
/// listed in [`STATUS_PLUGIN_DEP_ROOTS`] / [`PICKER_PLUGIN_DEP_ROOTS`].
/// The artifact is considered fresh only when its mtime is at least as
/// recent as the newest source file across the entire set.
fn artifact_is_fresh(artifact: &Path, plugin_src: &Path, dep_roots: &[PathBuf]) -> bool {
    let Ok(art_meta) = fs::metadata(artifact) else {
        return false;
    };
    let Ok(art_mtime) = art_meta.modified() else {
        return false;
    };

    // Start with the plugin's own src/ tree.
    let mut newest: Option<std::time::SystemTime> = walk_newest_mtime(plugin_src);
    // Fold in each transitively-depended source root. `None` from a
    // missing root is harmless — it means there's nothing newer there
    // to argue about.
    for root in dep_roots {
        if let Some(m) = walk_newest_mtime(root) {
            newest = Some(match newest {
                Some(n) if n >= m => n,
                _ => m,
            });
        }
    }
    let newest_src = newest.unwrap_or(art_mtime);
    art_mtime >= newest_src
}

/// F-704: resolve the transitive source roots a given plugin package
/// should watch for freshness, as absolute paths under `workspace_root`.
fn plugin_dep_roots(workspace_root: &Path, plugin_pkg: &str) -> Vec<PathBuf> {
    let rels: &[&str] = match plugin_pkg {
        "ark-plugin-status" => STATUS_PLUGIN_DEP_ROOTS,
        "ark-plugin-picker" => PICKER_PLUGIN_DEP_ROOTS,
        _ => &[],
    };
    rels.iter().map(|p| workspace_root.join(p)).collect()
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
