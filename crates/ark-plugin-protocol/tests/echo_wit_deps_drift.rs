// F-441/F-443: echo's wit/deps/ark-plugin/ vendors a copy of the canonical
// ark:plugin WIT. Drift here means host + reference plugin diverge silently.
// This test asserts byte-identical match against the source of truth.
// When you edit crates/ark-plugin-protocol/wit/, also copy into
// crates/ark-plugin-protocol/examples/echo/wit/deps/ark-plugin/.

use std::fs;
use std::path::PathBuf;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn assert_match(name: &str) {
    let canonical = crate_root().join("wit").join(name);
    let vendored = crate_root()
        .join("examples")
        .join("echo")
        .join("wit")
        .join("deps")
        .join("ark-plugin")
        .join(name);

    let canonical_bytes = fs::read(&canonical)
        .unwrap_or_else(|e| panic!("canonical wit missing: {}: {}", canonical.display(), e));
    let vendored_bytes = fs::read(&vendored)
        .unwrap_or_else(|e| panic!("vendored wit missing: {}: {}", vendored.display(), e));

    assert_eq!(
        canonical_bytes,
        vendored_bytes,
        "\n\nDRIFT: {} diverges from canonical.\n  canonical: {}\n  vendored:  {}\n  Fix: cp {} {}\n",
        name,
        canonical.display(),
        vendored.display(),
        canonical.display(),
        vendored.display(),
    );
}

#[test]
fn echo_vendored_plugin_wit_matches_canonical() {
    assert_match("plugin.wit");
}

#[test]
fn echo_vendored_widget_tree_wit_matches_canonical() {
    assert_match("widget-tree.wit");
}
