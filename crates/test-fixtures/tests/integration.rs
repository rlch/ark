//! T-111 integration tests: assert the cavekit-project fixture tree exists
//! with the shapes downstream contract suites (T-114/T-115) expect.
//!
//! These tests exercise the published path constants in `ark_test_fixtures`
//! against the on-disk fixture files committed under
//! `tests/fixtures/cavekit-project/`.

use std::fs;
use std::path::{Path, PathBuf};

use ark_test_fixtures::{loaders, paths};

fn cavekit_path(rel: &str) -> PathBuf {
    Path::new(paths::CAVEKIT_PROJECT).join(rel)
}

fn read_cavekit(rel: &str) -> String {
    let path = cavekit_path(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {} failed: {err}", path.display()))
}

#[test]
fn cavekit_fixture_has_build_site() {
    let contents = read_cavekit("context/sites/TEST-001/build-site.md");
    assert!(
        contents.contains("```mermaid"),
        "build-site.md must contain a ```mermaid block, got:\n{contents}"
    );
    assert!(
        contents.contains("Total tasks:"),
        "build-site.md must carry a 'Total tasks:' summary line"
    );
}

#[test]
fn cavekit_fixture_has_impl_overview() {
    let contents = read_cavekit("context/impl/impl-overview.md");
    assert!(
        contents.contains("## Tier Progress"),
        "impl-overview.md must contain a '## Tier Progress' header"
    );
    assert!(
        contents.contains("## Activity Log"),
        "impl-overview.md must contain an '## Activity Log' section"
    );
}

#[test]
fn cavekit_fixture_has_findings() {
    let contents = read_cavekit("context/impl/impl-review-findings.md");
    // At least one F-NNN row in the markdown table.
    let has_finding = contents
        .lines()
        .any(|l| l.trim_start().starts_with("| F-") || l.contains("### F-"));
    assert!(
        has_finding,
        "impl-review-findings.md must contain at least one 'F-' finding row"
    );
}

#[test]
fn cavekit_fixture_has_ralph_loop() {
    let contents = read_cavekit("ralph-loop.md");
    assert!(
        !contents.trim().is_empty(),
        "ralph-loop.md must be non-empty"
    );
    assert!(
        contents.contains("iteration:"),
        "ralph-loop.md must carry an 'iteration:' field"
    );
    // Watchers read .claude/ralph-loop.local.md — make sure the local mirror
    // also exists so ralph_loop_watcher-style tests have something to read.
    let local = cavekit_path(".claude/ralph-loop.local.md");
    assert!(
        local.exists(),
        ".claude/ralph-loop.local.md must exist at {}",
        local.display()
    );
}

#[test]
fn cavekit_fixture_dir_helper_resolves_to_fixture_root() {
    let dir = loaders::cavekit_fixture_dir();
    assert!(dir.is_absolute());
    assert!(dir.exists(), "cavekit fixture dir must exist: {dir:?}");
    assert!(
        dir.join("ralph-loop.md").exists(),
        "helper must point at the populated fixture root"
    );
}
