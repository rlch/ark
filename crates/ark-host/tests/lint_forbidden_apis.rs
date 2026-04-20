//! T-PP-030 (cavekit-plugin-protocol R1, R4): forbidden-API lint.
//!
//! ark-host uses the wasmtime **component model** exclusively. The core
//! `wasmtime::Module` / `wasmtime::Instance` / `wasmtime::Linker<T>`
//! types, plus `define_unknown_imports_as_traps` (cluster 3 §3.2
//! approach C — the wrong granularity for capability gating), must
//! never appear in production code under `crates/ark-host/src/`.
//!
//! This test walks every `.rs` file under `src/` and fails loudly on
//! the first forbidden substring it finds. Component-model siblings —
//! `wasmtime::component::Module`, `wasmtime::component::Instance`,
//! `wasmtime::component::Linker` — are explicitly allowed because the
//! grep matches the literal substring `wasmtime::Module` etc. and skips
//! over `wasmtime::component::Module`.
//!
//! # Escape hatch
//!
//! A source line containing `// lint-allow: test-fixture` is ignored —
//! this lets future test fixtures keep living under `src/` if they
//! need to touch core wasmtime types for legitimate reasons. The
//! escape hatch is deliberately test-fixture-only; production code
//! paths never have a legitimate need for the banned APIs.

use std::fs;
use std::path::{Path, PathBuf};

/// Source-literal patterns that must not appear in `ark-host/src/`.
///
/// Each entry is `(needle, remediation)`. The needle is matched as a
/// literal substring — regex characters have no special meaning.
const FORBIDDEN: &[(&str, &str)] = &[
    (
        "wasmtime::Module",
        "use wasmtime::component::Component instead (plugins are components, not core modules)",
    ),
    (
        "wasmtime::Instance",
        "use wasmtime::component::Instance instead (component-model only)",
    ),
    (
        "wasmtime::Linker",
        "use wasmtime::component::Linker instead (core Linker<T> is forbidden by cluster 3 §3.2)",
    ),
    (
        "define_unknown_imports_as_traps",
        "cluster 3 §3.2 approach C is forbidden — cap gating uses per-profile Linker variants (R4)",
    ),
];

/// Substrings that, when present on the same line, neutralise a
/// forbidden-pattern hit. Needed because `wasmtime::Module` is a
/// substring of `wasmtime::component::Module` — when the component-path
/// is the thing being referenced we must not fire.
const NEUTRALISERS: &[(&str, &[&str])] = &[
    ("wasmtime::Module", &["wasmtime::component::Module"]),
    ("wasmtime::Instance", &["wasmtime::component::Instance"]),
    ("wasmtime::Linker", &["wasmtime::component::Linker"]),
];

/// Per-line opt-out comment.
const ALLOW_MARKER: &str = "// lint-allow: test-fixture";

#[test]
fn no_forbidden_core_wasmtime_apis_in_src() {
    // Walk src/ relative to CARGO_MANIFEST_DIR (which is ark-host's
    // crate root when running `cargo test -p ark-host`).
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest_dir).join("src");
    let mut violations: Vec<String> = Vec::new();

    for file in walk_rs_files(&src) {
        let contents = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("failed to read {}: {}", file.display(), e));
        for (line_no, line) in contents.lines().enumerate() {
            if line.contains(ALLOW_MARKER) {
                continue;
            }
            for (needle, remediation) in FORBIDDEN {
                if !line.contains(needle) {
                    continue;
                }
                // Check neutralisers: if the line contains a
                // component-path equivalent, skip the hit for this
                // needle.
                let neutralised = NEUTRALISERS
                    .iter()
                    .find(|(n, _)| n == needle)
                    .map(|(_, allowed)| allowed.iter().any(|a| line.contains(a)))
                    .unwrap_or(false);
                if neutralised {
                    // Also require the LINE contains ONLY the
                    // component form — if both bare and component
                    // appear we still want to flag the bare one.
                    // Detect "bare" = `wasmtime::Module` NOT preceded
                    // by `component::`. Do a simple char-by-char scan.
                    if line_has_bare_occurrence(line, needle) {
                        violations.push(format!(
                            "forbidden API in {}:{}: {:?} — {}",
                            file.strip_prefix(manifest_dir).unwrap_or(&file).display(),
                            line_no + 1,
                            needle,
                            remediation
                        ));
                    }
                    continue;
                }
                violations.push(format!(
                    "forbidden API in {}:{}: {:?} — {}",
                    file.strip_prefix(manifest_dir).unwrap_or(&file).display(),
                    line_no + 1,
                    needle,
                    remediation
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "found {} forbidden-API violation(s) under ark-host/src/:\n  {}",
        violations.len(),
        violations.join("\n  ")
    );
}

/// Returns `true` iff `line` contains `needle` at a position NOT
/// immediately preceded by `component::`. Used as a second-pass check
/// for lines that mix component-path references with bare-path
/// references.
fn line_has_bare_occurrence(line: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(needle) {
        let abs = search_from + rel;
        // Look at the ~12 chars immediately before the hit. If they
        // end with `component::`, treat this hit as the component
        // form; otherwise it's bare.
        let prefix_start = abs.saturating_sub(12);
        let prefix = &line[prefix_start..abs];
        if !prefix.ends_with("component::") {
            return true;
        }
        search_from = abs + needle.len();
    }
    false
}

/// Recursive `.rs` file walk. No globs / external deps — keeps the lint
/// test zero-cost to pull in.
fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
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
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

#[test]
fn lint_detects_bare_core_references_in_synthetic_input() {
    // Smoke test the line scanner against the exact patterns we want
    // to catch / skip. Guards against regressions in the substring
    // neutraliser logic.
    assert!(line_has_bare_occurrence(
        "let m: wasmtime::Module = ...;",
        "wasmtime::Module"
    ));
    assert!(!line_has_bare_occurrence(
        "let m: wasmtime::component::Module = ...;",
        "wasmtime::Module"
    ));
    // Mixed-line case — both forms present. We flag the bare one.
    assert!(line_has_bare_occurrence(
        "// see wasmtime::component::Module vs wasmtime::Module",
        "wasmtime::Module"
    ));
    // Linker
    assert!(line_has_bare_occurrence(
        "use wasmtime::Linker;",
        "wasmtime::Linker"
    ));
    assert!(!line_has_bare_occurrence(
        "use wasmtime::component::Linker;",
        "wasmtime::Linker"
    ));
}
