//! Composition merge tests (T-080).
//!
//! Integration tests that exercise the full composition pipeline:
//! `parse_scene` → `compose_scene` → `apply_namespacing` → `enforce_load_order`.
//! Each fixture creates a tempdir with scene + fragment files and verifies
//! the composed + merged output via `insta::assert_snapshot!`.
//!
//! Regenerate snapshots: `cargo insta test --accept -p ark-scene --test composition`

use std::fs;
use std::path::Path;

use ark_scene::compose::compose_scene;
use ark_scene::error::SceneError;
use ark_scene::load_order::{enforce_load_order, LoadOrderResult};
use ark_scene::namespace::{apply_namespacing, NamespaceContext};
use ark_scene::parse::parse_scene;
use tempfile::TempDir;

fn write_file(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

fn full_pipeline(dir: &Path, scene_file: &str) -> Result<LoadOrderResult, SceneError> {
    let scene_path = dir.join(scene_file);
    let src = fs::read_to_string(&scene_path).unwrap();
    let ir = parse_scene(src, &scene_path)?;
    let mut composed = compose_scene(ir)?;
    apply_namespacing(&mut composed.scene, &NamespaceContext::User)?;
    Ok(enforce_load_order(&composed.scene.body))
}

fn format_result(r: &LoadOrderResult) -> String {
    let mut out = String::new();

    out.push_str(&format!("layouts: {}\n", r.layouts.len()));
    for l in &r.layouts {
        out.push_str(&format!("  tabs: {}\n", l.tabs.len()));
        for t in &l.tabs {
            out.push_str(&format!("    tab {}\n", t.handle));
        }
    }

    out.push_str(&format!("modes: {}\n", r.modes.len()));
    for m in &r.modes {
        out.push_str(&format!("  mode \"{}\"\n", m.name));
    }

    out.push_str(&format!("reactions: {}\n", r.reactions.len()));
    for on in &r.reactions {
        let kind = on
            .selector
            .as_ref()
            .map(|s| s.kind.as_str())
            .unwrap_or("<none>");
        out.push_str(&format!("  on \"{kind}\"\n"));
    }

    out.push_str(&format!("binds: {}\n", r.binds.len()));
    for b in &r.binds {
        out.push_str(&format!("  bind \"{}\"\n", b.chord));
    }

    out.push_str(&format!("uses: {}\n", r.uses.len()));
    for u in &r.uses {
        out.push_str(&format!("  use \"{}\"\n", u.name));
    }

    if !r.disabled_extensions.is_empty() {
        out.push_str(&format!(
            "disabled: {}\n",
            r.disabled_extensions.join(", ")
        ));
    }

    out
}

// =========================================================================
// Fixture 1: Basic include splicing
// =========================================================================

#[test]
fn compose_basic_include() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "editor.kdl",
        r#"layout {
    tab "@editor" {
        pane "@code"
    }
}
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "basic" {
    include "editor.kdl"
    on "user.save" {
        emit "user.saved"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_basic_include", format_result(&result));
}

// =========================================================================
// Fixture 2: clear-reactions within included fragments
// =========================================================================

#[test]
fn compose_clear_reactions() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "reactions.kdl",
        r#"on "user.FileEdited" {
    emit "user.lint"
}
on "user.FileSaved" {
    emit "user.format"
}
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "clear-test" {
    include "reactions.kdl"
    clear-reactions event="user.FileEdited"
    on "user.BuildDone" {
        emit "user.notify"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_clear_reactions", format_result(&result));
}

// =========================================================================
// Fixture 3: clear-bind within included fragments
// =========================================================================

#[test]
fn compose_clear_bind() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "binds.kdl",
        r#"bind "Alt d" {
    close "@p1"
}
bind "Alt n" {
    close "@p2"
}
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "clearbind" {
    include "binds.kdl"
    clear-bind "Alt d"
    bind "Alt x" {
        close "@p1"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_clear_bind", format_result(&result));
}

// =========================================================================
// Fixture 4: Cycle detection error
// =========================================================================

#[test]
fn compose_cycle_error() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(dir, "a.kdl", r#"include "b.kdl""#);
    write_file(dir, "b.kdl", r#"include "a.kdl""#);

    write_file(
        dir,
        "main.kdl",
        r#"scene "cyclic" {
    include "a.kdl"
}"#,
    );

    let err = full_pipeline(dir, "main.kdl").unwrap_err();
    assert!(
        matches!(err, SceneError::IncludeCycle { .. }),
        "expected IncludeCycle, got: {err}"
    );
}

// =========================================================================
// Fixture 5: Handle conflict across fragments
// =========================================================================

#[test]
fn compose_handle_conflict() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "frag_a.kdl",
        r#"layout { tab "@shared" { pane "@p1" } }"#,
    );
    write_file(
        dir,
        "frag_b.kdl",
        r#"layout { tab "@shared" { pane "@p2" } }"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "conflict" {
    include "frag_a.kdl"
    include "frag_b.kdl"
}"#,
    );

    let err = full_pipeline(dir, "main.kdl").unwrap_err();
    match &err {
        SceneError::IncludeHandleClash { handle, .. } => {
            assert_eq!(handle, "@shared");
        }
        other => panic!("expected IncludeHandleClash, got: {other}"),
    }
}

// =========================================================================
// Fixture 6: Load-order precedence — keybind last-wins
// =========================================================================

#[test]
fn compose_bind_last_wins() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "defaults.kdl",
        r#"bind "Alt d" {
    close "@editor"
}
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "lastwin" {
    include "defaults.kdl"
    bind "Alt d" {
        close "@term"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_bind_last_wins", format_result(&result));
}

// =========================================================================
// Fixture 7: disable-extension
// =========================================================================

#[test]
fn compose_disable_extension() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "with_use.kdl",
        r#"use "git-status"
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "noext" {
    include "with_use.kdl"
    disable-extension "git-status"
    layout { tab "@main" { pane "@p" } }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_disable_extension", format_result(&result));
}

// =========================================================================
// Fixture 8: Namespace rewrite — user context
// =========================================================================

#[test]
fn compose_namespace_user_rewrite() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "reactions.kdl",
        r#"on "user.save" {
    emit "user.saved"
}
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "ns" {
    include "reactions.kdl"
    on "user.build" {
        emit "user.done"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_namespace_user_rewrite", format_result(&result));
}

// =========================================================================
// Fixture 9: Kitchen sink — multiple fragments with mixed directives
// =========================================================================

#[test]
fn compose_kitchen_sink() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "layout.kdl",
        r#"layout {
    tab "@editor" {
        pane "@code"
        pane "@term"
    }
}
"#,
    );

    write_file(
        dir,
        "reactions.kdl",
        r#"on "user.FileEdited" {
    emit "user.lint"
}
on "user.FileSaved" {
    emit "user.format"
}
"#,
    );

    write_file(
        dir,
        "binds.kdl",
        r#"bind "Alt d" { close "@term" }
bind "Alt n" { close "@code" }
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "kitchen-sink" {
    use "lsp"
    include "layout.kdl"
    include "reactions.kdl"
    include "binds.kdl"
    clear-reactions event="user.FileEdited"
    clear-bind "Alt d"
    bind "Ctrl s" {
        emit "user.save"
    }
    on "user.BuildComplete" {
        emit "user.notify"
    }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_kitchen_sink", format_result(&result));
}

// =========================================================================
// Fixture 10: Nested includes — transitive splicing
// =========================================================================

#[test]
fn compose_nested_includes() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();

    write_file(
        dir,
        "deep.kdl",
        r#"bind "Alt z" { emit "user.undo" }"#,
    );

    write_file(
        dir,
        "mid.kdl",
        r#"include "deep.kdl"
on "user.change" { emit "user.autosave" }
"#,
    );

    write_file(
        dir,
        "main.kdl",
        r#"scene "nested" {
    include "mid.kdl"
    layout { tab "@main" { pane "@p" } }
}"#,
    );

    let result = full_pipeline(dir, "main.kdl").unwrap();
    insta::assert_snapshot!("compose_nested_includes", format_result(&result));
}
