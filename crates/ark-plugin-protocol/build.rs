//! T-PP-015 (cavekit-plugin-protocol R2): WIT lint.
//!
//! Runs at build time over every `wit/*.wit` file in this crate and
//! enforces the two acceptance criteria from R2 that live at the file
//! level:
//!
//!   1. Every top-level `interface` declared in an `ark:plugin` package
//!      must belong to the `ark:host/*` or `ark:cap/*` conceptual
//!      bucket. The WIT source does NOT spell out the prefix on the
//!      interface line itself (WIT syntax is `interface <name> { … }`),
//!      so the lint maps each `interface <name>` to a bucket via a
//!      closed set of names. Any interface name outside the closed set
//!      = lint failure. The closed set is the one enumerated in kit R2
//!      acceptance: `log`, `clock`, `plugin-id` (ark:host), `fs-read`,
//!      `fs-write`, `network`, `spawn-process`, `bus-send`,
//!      `bus-receive` (ark:cap), plus implementation-detail helper
//!      interfaces `types` and `widget-tree-types` that carry the
//!      shared-record surface but are NOT imported as host services.
//!
//!   2. No line in any `wit/*.wit` file may re-export
//!      `wasi:cli/environment` from the `ark:plugin` package. WASI
//!      integration lives exclusively on the host linker; re-exporting
//!      it through the plugin world would drag WASI's version churn
//!      into the plugin contract.
//!
//! On failure the lint prints `cargo:warning=…` lines (so the failure
//! is visible in a plain `cargo check` run) and exits non-zero so the
//! workspace build aborts.
//!
//! NOTE: this is plain-text parsing — no `wit-parser` dependency. R2
//! acceptance does not require a full parse; adding a heavy
//! build-dependency just for the lint was explicitly rejected in the
//! T-PP-015 packet.

use std::fs;
use std::path::Path;
use std::process;

/// Closed set of interface names permitted inside the `ark:plugin`
/// package. Kit R2 acceptance enumerates the user-visible host + cap
/// interfaces; `types` and `widget-tree-types` are internal helper
/// interfaces that carry the shared-record surface and are NOT
/// imported as services.
const ALLOWED_HOST_INTERFACES: &[&str] = &["log", "clock", "plugin-id"];
const ALLOWED_CAP_INTERFACES: &[&str] = &[
    "fs-read",
    "fs-write",
    "network",
    "spawn-process",
    "bus-send",
    "bus-receive",
];
const ALLOWED_HELPER_INTERFACES: &[&str] = &["types", "widget-tree-types"];

/// The WASI interface the plugin world must NEVER re-export.
const FORBIDDEN_REEXPORT_MARKER: &str = "wasi:cli/environment";

fn main() {
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let wit_dir = Path::new(crate_dir).join("wit");

    let files = [wit_dir.join("plugin.wit"), wit_dir.join("widget-tree.wit")];

    // Rerun triggers — any touch to either .wit file re-runs the lint.
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    let mut failed = false;

    for path in &files {
        let source = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                println!(
                    "cargo:warning=ark-plugin-protocol WIT lint: failed to read {}: {}",
                    path.display(),
                    e
                );
                failed = true;
                continue;
            }
        };

        // Strip line comments so `//` mentions of reserved words don't
        // trigger false positives.
        let mut active_lines = String::new();
        for line in source.lines() {
            let stripped = match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            };
            active_lines.push_str(stripped);
            active_lines.push('\n');
        }

        // Lint 1: partition check on top-level `interface <name>`
        // declarations.
        for (lineno, raw_line) in active_lines.lines().enumerate() {
            let line = raw_line.trim_start();
            let Some(rest) = line.strip_prefix("interface ") else {
                continue;
            };
            // `interface <name> {` or `interface <name>{` — grab the
            // contiguous identifier up to the first whitespace or
            // opening brace.
            let name = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == '{')
                .next()
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                continue;
            }
            let is_host = ALLOWED_HOST_INTERFACES.contains(&name);
            let is_cap = ALLOWED_CAP_INTERFACES.contains(&name);
            let is_helper = ALLOWED_HELPER_INTERFACES.contains(&name);
            if !(is_host || is_cap || is_helper) {
                println!(
                    "cargo:warning=ark-plugin-protocol WIT lint: {}:{} interface `{}` is not in the ark:host/* or ark:cap/* closed set (kit R2). Add it to ALLOWED_HOST_INTERFACES or ALLOWED_CAP_INTERFACES in build.rs if it is intended, or rename it.",
                    path.display(),
                    lineno + 1,
                    name
                );
                failed = true;
            }
        }

        // Lint 2: forbidden WASI re-export.
        for (lineno, raw_line) in active_lines.lines().enumerate() {
            if raw_line.contains(FORBIDDEN_REEXPORT_MARKER) {
                println!(
                    "cargo:warning=ark-plugin-protocol WIT lint: {}:{} re-exports forbidden interface `{}` (kit R2 — WASI lives on the host linker, not in the plugin world).",
                    path.display(),
                    lineno + 1,
                    FORBIDDEN_REEXPORT_MARKER
                );
                failed = true;
            }
        }
    }

    if failed {
        println!("cargo:warning=ark-plugin-protocol WIT lint FAILED — see the preceding warnings.");
        process::exit(1);
    }
}
