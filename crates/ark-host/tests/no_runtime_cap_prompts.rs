//! T-PP-040 (cavekit-plugin-protocol R5): forbidden runtime-grant lint.
//!
//! R5 is unambiguous: all capability grants live in `ark.kdl`. There is
//! NO interactive prompt path, NO `ark:host/request-capability` host
//! function, NO `ask_user` modal, NO TTY/socket grant-request channel.
//! If a plugin's required caps (R3 import scan) are not a subset of the
//! user's granted caps, the host refuses to load — the user edits
//! `ark.kdl`, restarts, retries.
//!
//! This test walks every `.rs` file under `crates/ark-host/src/` and
//! every `.wit` file under `crates/ark-plugin-protocol/wit/` and
//! fails on the first forbidden substring it finds. The identifier
//! list is derived directly from the R5 acceptance criterion:
//!
//! > No interactive prompt path exists in the code. `ark-host` has no
//! > TTY-grant code, no socket-grant code, no future-grant-promise
//! > code. Verified by grep of source for "prompt"/"grant_request"/
//! > "ask_user".
//!
//! # Escape hatch
//!
//! A source line tagged with `// lint-allow: no-runtime-prompt` is
//! ignored. Intended for test fixtures that need to reference the
//! banned names (this file is already self-exempt because tests live
//! in `tests/`, not `src/`).
//!
//! Adding a new forbidden substring here requires a kit revision; the
//! set is the backstop for R5's "no runtime elevation" posture, so
//! removing an entry needs explicit acknowledgement.

use std::fs;
use std::path::{Path, PathBuf};

/// Substrings banned in `crates/ark-host/src/`. Matched case-
/// insensitively against each source line.
const FORBIDDEN_IN_RS: &[&str] = &[
    "prompt",
    "grant_request",
    "ask_user",
    "request-capability",
    "runtime_grant",
    "runtime_cap_grant",
];

/// Substrings banned in `crates/ark-plugin-protocol/wit/*.wit`. The
/// WIT surface must not declare either a `request-capability` or
/// `prompt-user` interface.
const FORBIDDEN_IN_WIT: &[&str] = &[
    "interface request-capability",
    "interface prompt-user",
    "request-capability",
    "prompt-user",
];

/// Per-line opt-out marker.
const ALLOW_MARKER: &str = "// lint-allow: no-runtime-prompt";

#[test]
fn no_runtime_capability_prompts_in_ark_host_src() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest_dir).join("src");
    let mut violations: Vec<String> = Vec::new();

    for file in walk_files(&src, "rs") {
        let contents = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", file.display(), e));
        for (line_no, line) in contents.lines().enumerate() {
            if line.contains(ALLOW_MARKER) {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            for needle in FORBIDDEN_IN_RS {
                if lower.contains(&needle.to_ascii_lowercase()) {
                    violations.push(format!(
                        "{}:{}: forbidden substring {:?} — R5 forbids runtime cap elevation",
                        file.strip_prefix(manifest_dir).unwrap_or(&file).display(),
                        line_no + 1,
                        needle
                    ));
                    break;
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "T-PP-040 (R5): {} violation(s) under ark-host/src/:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

#[test]
fn no_runtime_capability_prompts_in_wit_surface() {
    // Locate the sibling crate's wit/ directory: ark-host lives at
    // `crates/ark-host`; WIT files live at
    // `crates/ark-plugin-protocol/wit/`. Walk up one level from
    // CARGO_MANIFEST_DIR and resolve from there so the test survives
    // a rename of either crate directory.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_crates = manifest_dir.parent().expect("crates/ parent resolvable");
    let wit_dir = workspace_crates.join("ark-plugin-protocol").join("wit");
    if !wit_dir.is_dir() {
        panic!(
            "expected WIT dir at {} — the R5 forbidden-interface lint needs access to the plugin surface",
            wit_dir.display()
        );
    }

    let mut violations: Vec<String> = Vec::new();
    for file in walk_files(&wit_dir, "wit") {
        let contents = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", file.display(), e));
        for (line_no, line) in contents.lines().enumerate() {
            if line.contains(ALLOW_MARKER) {
                continue;
            }
            let lower = line.to_ascii_lowercase();
            for needle in FORBIDDEN_IN_WIT {
                if lower.contains(&needle.to_ascii_lowercase()) {
                    violations.push(format!(
                        "{}:{}: forbidden WIT reference {:?} — R5 forbids runtime cap elevation",
                        file.display(),
                        line_no + 1,
                        needle
                    ));
                    break;
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "T-PP-040 (R5): {} violation(s) under ark-plugin-protocol/wit/:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// Recursive walk collecting every file with the given extension.
fn walk_files(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some(ext) {
                out.push(path);
            }
        }
    }
    out
}
