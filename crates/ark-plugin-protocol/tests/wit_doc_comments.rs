//! T-PP-024 (cavekit-plugin-protocol R7): WIT doc-comment lint.
//!
//! Scope: the plain-text `wit/*.wit` files are the source of truth for
//! the plugin lifecycle contract. R7 pins specific idempotency sentences
//! into the doc-comments on `load` and `on-install`; this test grep-
//! asserts those literals are present and that no "deactivate"/
//! "on-unload"/"pre-shutdown" hook has snuck back in.
//!
//! If a cavekit change wants to retire one of these sentences, update
//! the kit (R7) AND this test in the same commit.

use std::fs;
use std::path::PathBuf;

fn crate_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at `crates/ark-plugin-protocol/`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_plugin_wit() -> String {
    let p = crate_root().join("wit").join("plugin.wit");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("failed to read {}: {e}", p.display()))
}

/// Slice the raw WIT text down to the doc-comment block immediately
/// preceding `export <name>:`. Doc-comments in WIT use `///`; we walk
/// backwards from the export line collecting contiguous `///` lines.
fn doc_comment_before_export(source: &str, export_name: &str) -> String {
    let needle = format!("export {export_name}:");
    let lines: Vec<&str> = source.lines().collect();
    let idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&needle))
        .unwrap_or_else(|| panic!("no `export {export_name}:` line in wit/plugin.wit"));
    let mut out: Vec<&str> = Vec::new();
    // Walk upward collecting `///` lines until we hit something else.
    for i in (0..idx).rev() {
        let t = lines[i].trim_start();
        if let Some(body) = t.strip_prefix("///") {
            out.push(body);
        } else if t.is_empty() {
            // Empty line breaks the doc-block per WIT convention.
            break;
        } else {
            break;
        }
    }
    out.reverse();
    out.join("\n")
}

#[test]
fn load_doc_comment_contains_r7_literal() {
    let src = read_plugin_wit();
    let doc = doc_comment_before_export(&src, "load");
    assert!(
        doc.contains("Nothing in memory survives across calls"),
        "R7: `load` doc-comment must contain the literal \
         \"Nothing in memory survives across calls\" — got doc:\n{doc}"
    );
}

#[test]
fn on_install_doc_comment_contains_r7_literal_lowercase() {
    let src = read_plugin_wit();
    let doc = doc_comment_before_export(&src, "on-install");
    assert!(
        doc.contains("may run again on the next activation cycle"),
        "R7: `on-install` doc-comment must contain the literal \
         \"may run again on the next activation cycle\" (LOWERCASE) — got doc:\n{doc}"
    );
}

#[test]
fn no_deactivate_style_hooks_in_wit() {
    // R7 explicitly forbids `deactivate` / `on-unload` / `pre-shutdown`.
    // Scan every `wit/*.wit` file in this crate for those tokens and
    // fail if any show up.
    let wit_dir = crate_root().join("wit");
    let mut offenders: Vec<(String, String)> = Vec::new();
    for entry in fs::read_dir(&wit_dir).expect("read wit/ dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("wit") {
            continue;
        }
        let text =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for forbidden in ["deactivate", "on-unload", "pre-shutdown"] {
            if text.contains(forbidden) {
                offenders.push((path.display().to_string(), forbidden.into()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "R7 forbids `deactivate`/`on-unload`/`pre-shutdown` in any WIT file; offenders: {offenders:?}"
    );
}
