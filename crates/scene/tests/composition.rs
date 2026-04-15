//! Composition merge-semantics insta snapshots (T-9.4, R11).
//!
//! Each fixture under `tests/fixtures/composition/<name>/` is a
//! self-contained micro-scene bundle exercising one rule from R11:
//!
//! * `child.kdl` — the user-entry scene. Always present.
//! * `.ark/scenes/<parent>.kdl` — parent scene(s) referenced via
//!   `extends` (optional, depending on the fixture).
//! * `<frag>.kdl` — sibling include fragments (optional).
//!
//! The harness stages the bundle into a tempdir, points
//! [`SceneSearchCtx::cwd`] at the tempdir root, and drives the full
//! composition pipeline (`load_composition` + `merge_fragments`).
//! For success fixtures the merged shape is rendered into a stable
//! multi-line dump and pinned via `insta::assert_snapshot!`. For
//! error fixtures the `SceneError` is rendered through miette's
//! unicode-nocolor theme (same shape as `tests/diagnostics.rs`) and
//! pinned.
//!
//! New fixtures auto-materialise a snapshot on first run; review
//! changes with `cargo insta review` or `INSTA_UPDATE=1 cargo test`.
//!
//! The full fixture list is enumerated in [`FIXTURES`] below so a
//! single `cargo test` invocation touches every composition rule.

use std::fs;
use std::path::{Path, PathBuf};

use ark_scene::error::SceneError;
use ark_scene::extends::SceneSearchCtx;
use ark_scene::merge::{ComposedScene, load_composition, merge_fragments};
use ark_scene::parse::parse_scene;
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme};
use tempfile::TempDir;

/// Enumerated fixture list — one entry per R11 rule we snapshot.
///
/// Each tuple is `(fixture-name, expected-outcome)`. The fixture name
/// doubles as the directory name under `tests/fixtures/composition/`
/// and the insta snapshot key, so the mapping between test input and
/// output artefact is obvious in `git blame`.
const FIXTURES: &[(&str, Expected)] = &[
    // --- Success fixtures ---------------------------------------
    ("reactions_append_in_load_order", Expected::Ok),
    ("keybind_last_wins", Expected::Ok),
    ("plugin_override_wins", Expected::Ok),
    ("include_splices_fragment", Expected::Ok),
    ("extends_with_includes", Expected::Ok),
    ("clear_reactions_drops_inherited", Expected::Ok),
    ("clear_keybind_then_re_add", Expected::Ok),
    ("disable_plugin_drops_inherited", Expected::Ok),
    ("parent_clear_on_descendant_only_noop", Expected::Ok),
    // --- Error fixtures -----------------------------------------
    ("duplicate_plugin_without_override", Expected::Err),
    ("duplicate_tab_across_layouts", Expected::Err),
    ("extends_not_found", Expected::Err),
    ("include_cycle", Expected::Err),
    ("extends_cycle", Expected::Err),
];

/// Expected outcome of running the composition pipeline on a fixture.
///
/// `Ok` renders the merged [`ComposedScene`] as a deterministic dump;
/// `Err` renders the first error through miette's unicode-nocolor
/// theme. Keeping the two cases distinct makes the snapshot bodies
/// self-describing — a snapshot flipping from "composed scene dump"
/// to "miette error" signals a behavioural regression that an ad-hoc
/// assert would silently miss.
#[derive(Clone, Copy, Debug)]
enum Expected {
    Ok,
    Err,
}

/// Snapshot every fixture in [`FIXTURES`].
///
/// Runs as a single test so `cargo insta review` sees all fixtures
/// at once — reviewers can accept / reject changes in one sweep.
#[test]
fn snapshot_every_composition_fixture() {
    for (name, expected) in FIXTURES {
        run_fixture(name, *expected);
    }
}

fn run_fixture(name: &str, expected: Expected) {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/composition")
        .join(name);

    assert!(
        fixture_dir.exists(),
        "fixture `{name}` is missing at {}",
        fixture_dir.display()
    );

    // Stage the fixture into a tempdir so the composition loader
    // sees a real `<cwd>/.ark/scenes/…` layout. Doing the stage per
    // fixture keeps absolute paths fresh and canonicalised.
    let tmp = TempDir::new().expect("tmpdir");
    let staging = tmp.path().canonicalize().expect("canonicalise tmp");
    copy_dir_recursive(&fixture_dir, &staging);

    let child_path = staging.join("child.kdl");
    assert!(
        child_path.exists(),
        "fixture `{name}` must supply a child.kdl"
    );

    let src = fs::read_to_string(&child_path).expect("read child.kdl");

    let doc = match parse_scene(&src, &child_path) {
        Ok(d) => d,
        Err(e) => {
            let rendered = render_error(&e);
            insta::with_settings!({ snapshot_suffix => name.to_string() }, {
                insta::assert_snapshot!(format!("{name}_error"), rendered);
            });
            match expected {
                Expected::Ok => panic!("fixture `{name}` expected Ok but parse failed"),
                Expected::Err => return,
            }
        }
    };

    let ctx = SceneSearchCtx::new(&staging);
    let load_result = load_composition(doc, child_path, &ctx);
    let outcome = match load_result {
        Ok(frags) => merge_fragments(frags),
        Err(e) => Err(e),
    };

    // Normalise tempdir paths in the rendered output so per-run
    // tempdir names (`/var/folders/.../T/.tmpXXXX/...`) don't
    // destabilise the pinned snapshots. Replacing the full staging
    // path with `<staging>` keeps the relative portion visible
    // (`<staging>/frag.kdl`) while removing the host-specific prefix.
    let staging_str = staging.display().to_string();

    match (outcome, expected) {
        (Ok(merged), Expected::Ok) => {
            let rendered = render_composed(&merged).replace(&staging_str, "<staging>");
            insta::with_settings!({ snapshot_suffix => name.to_string() }, {
                insta::assert_snapshot!(format!("{name}_ok"), rendered);
            });
        }
        (Err(err), Expected::Err) => {
            let rendered = render_error(&err).replace(&staging_str, "<staging>");
            insta::with_settings!({ snapshot_suffix => name.to_string() }, {
                insta::assert_snapshot!(format!("{name}_error"), rendered);
            });
        }
        (Ok(_), Expected::Err) => {
            panic!("fixture `{name}` expected an error but composition succeeded");
        }
        (Err(err), Expected::Ok) => {
            panic!(
                "fixture `{name}` expected success but composition failed: {}",
                render_error(&err)
            );
        }
    }
}

/// Render a [`SceneError`] through miette's unicode-nocolor theme —
/// matches the shape used by `tests/diagnostics.rs` so composition
/// error snapshots are stylistically consistent with the scope
/// diagnostics.
fn render_error(err: &dyn Diagnostic) -> String {
    let handler = GraphicalReportHandler::new().with_theme(GraphicalTheme::unicode_nocolor());
    let mut out = String::new();
    handler
        .render_report(&mut out, err)
        .expect("miette renders error");
    out
}

/// Render a [`ComposedScene`] as a deterministic multi-line dump so
/// the snapshot is readable in a diff + regenerates identically
/// across runs. Renders only the load-bearing R11 fields; sidecar
/// state (absolute paths, tempdir names) is omitted to keep the
/// snapshot stable across hosts.
fn render_composed(merged: &ComposedScene) -> String {
    let mut s = String::new();
    s.push_str(&format!("name: {}\n", merged.name));
    if let Some(depth) = merged.max_cascade_depth {
        s.push_str(&format!("max_cascade_depth: {depth}\n"));
    }

    s.push_str("\nreactions:\n");
    for on in &merged.reactions {
        s.push_str(&format!(
            "  - selector: {}  if: {}\n",
            on.selector,
            on.if_.as_deref().unwrap_or("-")
        ));
    }

    s.push_str("\nkeybinds:\n");
    for kb in &merged.keybinds {
        s.push_str(&format!(
            "  - chord: {}  intent: {}\n",
            kb.chord,
            kb.intent.as_deref().unwrap_or("-")
        ));
    }

    s.push_str("\nplugins:\n");
    for p in &merged.plugins {
        let mount = p
            .mount
            .as_ref()
            .map(|m| m.target.as_str())
            .unwrap_or("-");
        let src_uri = p
            .source
            .as_ref()
            .map(|s| s.uri.as_str())
            .unwrap_or("-");
        s.push_str(&format!(
            "  - name: {}  mount: {}  source: {}  override: {}\n",
            p.name,
            mount,
            src_uri,
            p.override_.unwrap_or(false)
        ));
    }

    s.push_str("\nlayout tabs:\n");
    if let Some(layout) = merged.layout.as_ref() {
        for tab in &layout.tabs {
            s.push_str(&format!(
                "  - name: {}  panes: {}\n",
                tab.name.as_deref().unwrap_or("<unnamed>"),
                tab.panes.len()
            ));
        }
    }

    s
}

/// Recursively copy every file under `src` into `dst`. Ignores
/// symlinks (none expected in fixtures). Used to stage the fixture
/// bundle into a writable tempdir because `std::fs::copy` doesn't
/// recurse.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("entry");
        let file_type = entry.file_type().expect("file_type");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).expect("copy");
        }
    }
}
