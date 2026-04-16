//! Integration tests for the reconciler (T-041..T-046).
//!
//! Uses [`RecordingApplier`] so no real `zellij` process is spawned.
//! Each test authors a minimal scene, drives one or more reconciliation
//! passes, and asserts the observable side-effects (applier invocations,
//! predicate-truth snapshot, mode-switch CLI flags).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ark_scene::compile::compile_scene;
use ark_scene::parse::parse_scene;
use ark_scene::reconciler::{
    Debouncer, LayoutApplier, OverrideLayoutFlags, RecordingApplier, Reconciler,
};
use ark_scene::rhai::Engine;
use ark_scene::view::ViewRegistry;

fn registry() -> ViewRegistry {
    ViewRegistry::with_primitives()
}

/// Compile a scene from source and wrap it in a reconciler with a
/// recording applier. Returns `(reconciler, applier, tempdir)` — the
/// tempdir must outlive the reconciler so layouts can be written.
fn make_reconciler(src: &str) -> (Reconciler, Arc<RecordingApplier>, tempfile::TempDir) {
    let ir = parse_scene(src, "test.kdl").expect("parse");
    let compiled = compile_scene(&Engine::new(), ir).expect("compile");
    let applier = Arc::new(RecordingApplier::new());
    let typed: Arc<dyn LayoutApplier> = applier.clone();
    let tmp = tempfile::tempdir().expect("tempdir");
    let r = Reconciler::new(compiled, registry(), typed).with_layouts_dir(tmp.path());
    (r, applier, tmp)
}

#[tokio::test]
async fn reconcile_evaluates_predicates_and_emits_override_layout() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" when="true" {
                pane "@p" when="true"
            }
        }
    }"#;
    let (mut r, applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    let outcome = r.reconcile(&mut scope).await.expect("reconcile ok");
    assert!(outcome.layout_path.exists());

    let calls = applier.snapshot().await;
    assert_eq!(calls.len(), 1, "exactly one override-layout call");
    assert_eq!(calls[0].0, outcome.layout_path);
    let flags = &calls[0].1;
    assert!(flags.retain_existing_terminal_panes);
    assert!(flags.retain_existing_plugin_panes);
    assert!(!flags.apply_only_to_active_tab);
}

#[tokio::test]
async fn reconcile_with_false_when_omits_pane() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" {
                pane "@visible" when="true"
                pane "@hidden" when="false"
            }
        }
    }"#;
    let (mut r, _applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    let outcome = r.reconcile(&mut scope).await.expect("reconcile ok");
    let text = std::fs::read_to_string(&outcome.layout_path).unwrap();
    assert!(text.contains("visible"), "visible handle must be rendered: {text}");
    assert!(!text.contains("hidden"), "hidden handle must be elided: {text}");
}

#[tokio::test]
async fn render_desired_layout_kdl_reflects_predicates() {
    // render_desired_layout_kdl doesn't touch disk — good for pure
    // logic assertions about predicate evaluation.
    let src = r#"scene "dev" {
        layout {
            tab "@main" {
                pane "@a" when="true"
                pane "@b" when="false"
            }
        }
    }"#;
    let (r, _applier, _tmp) = make_reconciler(src);
    let mut scope = rhai::Scope::new();
    let doc = r.render_desired_layout_kdl(&mut scope).expect("render");
    let text = doc.to_string();
    assert!(text.contains("@a") || text.contains("name=a"));
    assert!(!text.contains("@b"));
}

#[tokio::test]
async fn debounce_coalesces_rapid_changes() {
    let deb = Debouncer::new(Duration::from_millis(40));
    for _ in 0..5 {
        deb.mark_dirty().await;
        tokio::time::sleep(Duration::from_millis(8)).await;
    }
    // Still within the trailing window.
    assert!(!deb.should_fire_now().await);
    tokio::time::sleep(Duration::from_millis(45)).await;
    assert!(deb.should_fire_now().await);
    // Already consumed.
    assert!(!deb.should_fire_now().await);
}

#[tokio::test]
async fn mode_switch_uses_apply_only_to_active_tab() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" {
                pane "@p"
            }
        }
        mode "review" {
            tab "@main" {
                pane "@p"
            }
        }
    }"#;
    let (mut r, applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    let outcome = r
        .reconcile_mode("review", &mut scope)
        .await
        .expect("mode reconcile ok");
    assert!(outcome.layout_path.exists());
    let calls = applier.snapshot().await;
    assert_eq!(calls.len(), 1);
    let flags = &calls[0].1;
    assert!(
        flags.apply_only_to_active_tab,
        "mode switches must use --apply-only-to-active-tab"
    );
    assert!(flags.retain_existing_terminal_panes);
    assert!(flags.retain_existing_plugin_panes);
}

#[tokio::test]
async fn mode_default_reverts_to_base_layout() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" { pane "@p" }
        }
    }"#;
    let (mut r, applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    let outcome = r
        .reconcile_mode("default", &mut scope)
        .await
        .expect("default reverts");
    assert!(outcome.layout_path.exists());
    let calls = applier.snapshot().await;
    assert_eq!(calls.len(), 1);
    let flags = &calls[0].1;
    assert!(
        !flags.apply_only_to_active_tab,
        "default must NOT use mode-switch flags — it's a full reconcile"
    );
}

#[tokio::test]
async fn unknown_mode_errors() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" { pane "@p" }
        }
    }"#;
    let (mut r, _applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    let err = r
        .reconcile_mode("nope", &mut scope)
        .await
        .expect_err("unknown mode must error");
    let msg = format!("{err}");
    assert!(msg.contains("unknown mode") || msg.contains("nope"));
}

#[tokio::test]
async fn override_flags_cli_serialisation() {
    // Guard the zellij CLI contract: `full_reconcile` renders exactly
    // two flags; `mode_switch` adds one more. Changes here indicate an
    // R9 contract drift.
    let full = OverrideLayoutFlags::full_reconcile().to_cli_flags();
    assert_eq!(full.len(), 2);
    assert!(full.iter().any(|f| f == "--retain-existing-terminal-panes"));
    assert!(full.iter().any(|f| f == "--retain-existing-plugin-panes"));

    let mode = OverrideLayoutFlags::mode_switch().to_cli_flags();
    assert_eq!(mode.len(), 3);
    assert!(mode.iter().any(|f| f == "--apply-only-to-active-tab"));
}

#[tokio::test]
async fn predicate_truth_snapshot_tracked_across_passes() {
    let src = r#"scene "dev" {
        layout {
            tab "@main" {
                pane "@a" when="true"
            }
        }
    }"#;
    let (mut r, _applier, _tmp) = make_reconciler(src);

    let mut scope = rhai::Scope::new();
    r.reconcile(&mut scope).await.expect("first pass");
    let snap = r.last_truth_snapshot().clone();
    assert_eq!(snap.len(), 1);
    assert_eq!(
        snap.values().copied().collect::<Vec<_>>(),
        vec![true],
        "single predicate is true"
    );

    let outcome2 = r.reconcile(&mut scope).await.expect("second pass");
    assert!(
        !outcome2.predicates_changed,
        "second identical pass must report predicates_changed=false"
    );
}

// Sanity-check that the integration tests link against the public API
// (i.e. the right things are `pub`). Keeping this as a compile-only smoke
// test — no runtime assertions.
#[test]
fn public_api_smoke() {
    let _: PathBuf = PathBuf::from("/tmp/ark-test.kdl");
    let _ = ViewRegistry::with_primitives();
}
