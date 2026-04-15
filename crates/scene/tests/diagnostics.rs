//! Snapshot tests pinning the rendered miette output for every
//! `scene/*` diagnostic scenario enumerated in cavekit-scene.md R1,
//! R2, and R12.
//!
//! Each `.kdl` fixture in `tests/fixtures/` is a minimal, single-
//! scenario scene file. The integration test walks every fixture,
//! runs the same two-pass pipeline the CLI uses (parse → scope), and
//! renders the first error through miette's `unicode_nocolor` theme
//! — which produces ANSI-free output, so no separate strip-ansi
//! dependency is needed.
//!
//! The rendered string is pinned as an `insta` snapshot keyed by the
//! fixture filename. New fixtures auto-create a snapshot on first
//! run; review changes with `cargo insta review` (or
//! `INSTA_UPDATE=1 cargo test`).
//!
//! ## Why snapshots vs ad-hoc assertions
//!
//! R12 demands "at least one unit test per error code verifying the
//! diagnostic output matches a snapshot." That acceptance criterion
//! specifically calls for snapshot-style assertions because the
//! rendered form (code + help + caret + labels) is the user-facing
//! contract — pinning it prevents accidental regressions when later
//! tiers refactor help text or span computation.
//!
//! ## Pipeline order
//!
//! 1. `parse_scene` — if it fails, render and snapshot the
//!    `SceneError::Parse` directly. Scope pass is skipped because
//!    operating on malformed KDL is undefined.
//! 2. `check_scope` — run the R2 scope walker; if it produces any
//!    errors, render and snapshot the first one. Snapshotting only
//!    the first keeps per-fixture output compact and lets the
//!    `multiple_violations.kdl` fixture exercise the aggregation
//!    surface separately (via its own dedicated entry below).
//!
//! Fixtures that cleanly parse AND pass scope are rejected: every
//! fixture is expected to demonstrate at least one error, so a green
//! fixture is a test-harness bug.

use std::fs;
use std::path::{Path, PathBuf};

use ark_scene::error::SceneError;
use ark_scene::parse::parse_scene;
use ark_scene::scope::check_scope;
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme};

/// Render a `SceneError` through miette's unicode-nocolor theme so
/// the resulting string has no ANSI escapes. Uses the same handler
/// shape as `miette::set_hook` production setup, only with the
/// colorless theme.
fn render(err: &dyn Diagnostic) -> String {
    let handler = GraphicalReportHandler::new().with_theme(GraphicalTheme::unicode_nocolor());
    let mut out = String::new();
    handler
        .render_report(&mut out, err)
        .expect("miette renders error");
    out
}

/// Run parse + scope on a fixture's contents. Returns the first
/// error — preferring scope-layer diagnostics over parse-layer
/// ones because scope covers R1/R2 structural rules with better
/// hints (the "did you mean …?" path is only on scope errors), and
/// it re-parses through the raw `kdl` crate which is more lenient
/// than `facet-kdl`'s typed shape. Fall back to parse errors only
/// when scope finds nothing — that covers tokenizer-level failures
/// that scope can't surface.
fn first_error(src: &str, path: &Path) -> Option<SceneError> {
    if let Some(scope_err) = check_scope(src, path).into_iter().next() {
        return Some(scope_err);
    }
    parse_scene(src, path).err()
}

/// Load a fixture by stem (no `.kdl` extension) and return its
/// source text + synthetic path. The path uses only the stem +
/// extension so snapshots don't drift when the repo moves.
fn load_fixture(stem: &str) -> (String, PathBuf) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stem}.kdl"));
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {stem}.kdl: {e}"));
    // Snapshot-friendly path: strip the absolute prefix so the
    // rendered output matches across machines.
    let snap_path = PathBuf::from(format!("{stem}.kdl"));
    (src, snap_path)
}

/// Assert a fixture produces exactly one rendered diagnostic, which
/// then gets snapshotted under its fixture-stem name.
fn assert_fixture_snapshot(stem: &str) {
    let (src, path) = load_fixture(stem);
    let err = first_error(&src, &path)
        .unwrap_or_else(|| panic!("fixture {stem}.kdl produced no diagnostic"));
    let rendered = render(&err);
    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_snapshot!(stem, rendered);
    });
}

// ---------------------------------------------------------------------------
// R1 — file-root grammar
// ---------------------------------------------------------------------------

/// Unterminated string literal produces `scene/parse`. Covers the
/// KDL 2.0 tokenizer-layer diagnostic path (first pipeline stage).
#[test]
fn parse_unterminated_string() {
    assert_fixture_snapshot("unterminated_string");
}

/// `scene` without a name argument fails at facet-kdl's
/// reflection layer — surfaces as `scene/parse` per T-1.1's
/// catch-all wrapper. Grammar-refinement (→ `scene/grammar`) is a
/// later-tier improvement; the current snapshot pins what T-1.1
/// actually produces.
#[test]
fn parse_scene_without_name() {
    assert_fixture_snapshot("scene_without_name");
}

/// Stray non-`scene` top-level node fires `scene/unknown-node`
/// with a `scene`-adjacent did-you-mean suggestion (T-1.3 wiring).
#[test]
fn stray_top_level_node_suggests_scene() {
    assert_fixture_snapshot("stray_top_level_node");
}

/// Two top-level `scene { }` nodes trigger
/// `scene/duplicate-node` on the second.
#[test]
fn duplicate_scene_top_level() {
    assert_fixture_snapshot("duplicate_scene_top_level");
}

// ---------------------------------------------------------------------------
// R2 clause 1 — scene-root scope rules
// ---------------------------------------------------------------------------

/// `on "..."` inside `layout { }` — classic misplaced-node case.
#[test]
fn misplaced_on_inside_layout() {
    assert_fixture_snapshot("misplaced_on_inside_layout");
}

/// A `plugin`-body node (`source`) at scene root = unknown root node
/// (not recognised at R1; typo-suggester may or may not hint).
#[test]
fn plugin_body_at_scene_root() {
    assert_fixture_snapshot("plugin_body_at_scene_root");
}

// ---------------------------------------------------------------------------
// R2 clause 2 — layout-only node positions
// ---------------------------------------------------------------------------

/// `tab` at scene root — `tab` is not a scene-root node, so it
/// surfaces as `scene/unknown-node` (with a typo-suggester hint
/// toward any close R1 root-node name).
#[test]
fn tab_at_scene_root() {
    assert_fixture_snapshot("tab_at_scene_root");
}

/// Same shape for `pane`.
#[test]
fn pane_at_scene_root() {
    assert_fixture_snapshot("pane_at_scene_root");
}

// ---------------------------------------------------------------------------
// R2 clause 3 — `when=` attribute scope
// ---------------------------------------------------------------------------

/// `when=` on `on` block = misplaced-node (attribute form).
#[test]
fn when_attribute_on_on() {
    assert_fixture_snapshot("when_attribute_on_on");
}

/// `when=` on `keybind` = misplaced (legal only on tab/pane).
#[test]
fn when_attribute_on_keybind() {
    assert_fixture_snapshot("when_attribute_on_keybind");
}

/// `when=` on `plugin` = misplaced.
#[test]
fn when_attribute_on_plugin() {
    assert_fixture_snapshot("when_attribute_on_plugin");
}

// ---------------------------------------------------------------------------
// R2 clause 5 — `if=` attribute scope
// ---------------------------------------------------------------------------

/// `if=` on `keybind` = misplaced (legal only on `on`).
#[test]
fn if_attribute_on_keybind() {
    assert_fixture_snapshot("if_attribute_on_keybind");
}

// ---------------------------------------------------------------------------
// R2 clause 6 — `intent=` attribute scope
// ---------------------------------------------------------------------------

/// `intent=` on `on` block = misplaced (legal only on `keybind`).
#[test]
fn intent_attribute_on_on() {
    assert_fixture_snapshot("intent_attribute_on_on");
}

/// `intent=` on `extends` = misplaced.
#[test]
fn intent_attribute_on_extends() {
    assert_fixture_snapshot("intent_attribute_on_extends");
}

// ---------------------------------------------------------------------------
// R2 clause 4 — plugin-body scope
// ---------------------------------------------------------------------------

/// Bogus child inside `plugin { }` — `keybind` doesn't belong
/// inside a plugin body.
#[test]
fn bogus_node_inside_plugin() {
    assert_fixture_snapshot("bogus_node_inside_plugin");
}

// ---------------------------------------------------------------------------
// R1 — unknown node surface + did-you-mean
// ---------------------------------------------------------------------------

/// `reaction "…"` at scene root — arbitrary unknown name that
/// isn't close to any root node. Snapshot pins the rendered
/// UnknownNode with whatever suggestion (if any) the Jaro-Winkler
/// threshold produces for this input.
#[test]
fn unknown_scene_root_node() {
    assert_fixture_snapshot("unknown_scene_root_node");
}

/// Close typo: `keybnd` should yield a suggestion pointing at
/// `keybind`. Snapshot pins the rendered output so the hint
/// surfaces exactly once through miette's help text.
#[test]
fn scene_root_typo_keybind() {
    assert_fixture_snapshot("scene_root_typo_keybind");
}

/// Close typo: `layuot` (transposed) should suggest `layout`.
#[test]
fn scene_root_typo_layout() {
    assert_fixture_snapshot("scene_root_typo_layout");
}

// ---------------------------------------------------------------------------
// Misc scope shapes
// ---------------------------------------------------------------------------

/// Leaf-shape scene-root node carrying a body = stray child
/// reported with the leaf name as the parent.
#[test]
fn leaf_root_with_body() {
    assert_fixture_snapshot("leaf_root_with_body");
}

/// Multiple distinct violations in one file — we snapshot only the
/// first error (per `first_error` contract); the separate
/// `scope.rs` unit tests verify that multiple errors surface in
/// one pass.
#[test]
fn multiple_violations() {
    assert_fixture_snapshot("multiple_violations");
}
