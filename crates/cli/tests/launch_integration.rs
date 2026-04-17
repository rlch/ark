//! Integration tests for the bare-`ark` launch pipeline.
//!
//! These tests drive `commands::launch::run_with` against injected
//! [`MockMultiplexer`] + [`InlineSupervisor`] impls. They run on
//! every `cargo test` (no `ARK_E2E=1` gate, no zellij required) and
//! guard the bug class that keeps biting: the launch pipeline
//! compiling scenes + wiring supervisor + dispatching zellij with the
//! right shape, end-to-end, without ever touching a real terminal.
//!
//! ## What these tests catch
//!
//! - Missing preflight → CLI mutates state before realising zellij
//!   isn't installed.
//! - Supervisor spawned AFTER zellij → `ark list` / `ark kill` fail
//!   for the session that just came up.
//! - Shipped views (`status`, `picker`) not registered → scene
//!   compile errors out at runtime.
//! - Scene flag not plumbed through → `--scene myproject` silently
//!   falls back to the built-in default.
//! - Supervisor ready-timeout not surfaced cleanly → CLI hangs or
//!   panics instead of returning a clean error.
//!
//! All scenarios are per-test-ephemeral: fresh tempdir for state /
//! config / runtime, no shared env. `ARK_SCENE` and friends are
//! scrubbed behind a module-local mutex because cargo runs tests in
//! the same binary in parallel threads.

use std::path::PathBuf;
use std::sync::Mutex;

use ark_cli::commands::launch::{
    self,
    mock::{InlineSupervisor, MockMultiplexer, MultiplexerCall},
};
use ark_cli::ctx::Ctx;
use ark_cli::error::CliError;
use tempfile::TempDir;

// Env mutation is not safe in parallel; this mutex serialises every
// test that touches `ARK_SCENE` / `ARK_APPNAME` / `XDG_*` / `HOME` /
// `ZELLIJ*`. We can't use the crate-private `ark_cli::test_lock` from
// here because integration tests see only the public surface.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Names of env vars that every scenario scrubs before execution so
/// scene-resolution rungs behave deterministically regardless of
/// whether the enclosing shell has any of these set.
const SCRUBBED_VARS: &[&str] = &[
    "ARK_SCENE",
    "ARK_APPNAME",
    "ARK_STATE_DIR",
    "ARK_CONFIG_DIR",
    "ARK_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
];

/// RAII guard that scrubs `SCRUBBED_VARS` on `new` and restores them
/// on drop. Holds the [`ENV_LOCK`] mutex for its entire lifetime so
/// sibling tests serialize. Always create via [`TestEnv::new`] — it
/// pairs the lock with the tempdir so Drop order is correct.
struct TestEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    tmp: TempDir,
    prior: Vec<(&'static str, Option<String>)>,
}

impl TestEnv {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let prior: Vec<_> = SCRUBBED_VARS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in SCRUBBED_VARS {
            // SAFETY: covered by ENV_LOCK — no concurrent reader in
            // this test binary while the guard is held.
            unsafe { std::env::remove_var(k) };
        }
        TestEnv {
            _lock: lock,
            tmp,
            prior,
        }
    }

    fn ctx(&self) -> Ctx {
        let state = self.tmp.path().join("state");
        let cfg = self.tmp.path().join("config");
        let rt = self.tmp.path().join("runtime");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&rt).unwrap();
        Ctx {
            no_color: true,
            log_level: "info".to_string(),
            state_dir: state,
            config_dir: cfg,
            runtime_dir: rt,
        }
    }

    /// Write a valid scene file at `<config_dir>/scenes/<name>.kdl`
    /// for the `--scene NAME` resolution rung. Returns the path.
    fn write_named_scene(&self, ctx: &Ctx, name: &str) -> PathBuf {
        let dir = ctx.config_dir.join("scenes");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.kdl"));
        std::fs::write(&path, minimal_scene_kdl()).unwrap();
        path
    }

    /// Write a valid scene file at an arbitrary path for the
    /// `--scene /explicit/path.kdl` rung. Returns the path.
    fn write_scene_at(&self, relative: &str) -> PathBuf {
        let path = self.tmp.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, minimal_scene_kdl()).unwrap();
        path
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (k, v) in self.prior.drain(..) {
            // SAFETY: covered by the same ENV_LOCK still held by
            // `self._lock`.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// Minimal scene KDL that compiles through the whole pipeline
/// (shape detection → parse → compose → rhai → layout lowering). A
/// single pane running `bash` keeps the surface small while
/// exercising the shipped-view registry path if the scene adds a
/// view reference (pure layout is enough for most assertions).
fn minimal_scene_kdl() -> &'static str {
    r#"scene "test" {
    layout {
        tab "@main" {
            pane "@shell" {
                shell
            }
        }
    }
}
"#
}

/// Scene with references to shipped views (`status`, `picker`) —
/// exercises the registry-registration code path that a prior bug
/// missed.
fn scene_with_shipped_views_kdl() -> &'static str {
    r#"scene "test" {
    layout {
        tab "@main" {
            col {
                pane "@shell" {
                    shell
                }
                pane "@status" cells="1" {
                    status
                }
                pane "@picker" cells="1" {
                    picker
                }
            }
        }
    }
}
"#
}

// ---------------------------------------------------------------- tests ----

#[test]
fn preflight_failure_bails_before_supervisor_or_mux() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    let mux = MockMultiplexer::new().fail_preflight(CliError::PreflightFail {
        reason: "zellij not on PATH".to_string(),
    });
    let spawner = InlineSupervisor::new();

    let err = launch::run_with(&mux, &spawner, None, None, &ctx).expect_err("must fail");
    assert!(matches!(err, CliError::PreflightFail { .. }));

    // Supervisor must not have been spawned, session must not have
    // been run — preflight short-circuits.
    assert!(
        spawner.calls().is_empty(),
        "supervisor must not spawn when preflight fails"
    );
    let mux_calls = mux.calls();
    assert_eq!(mux_calls.len(), 1, "only preflight should have run");
    assert!(matches!(mux_calls[0], MultiplexerCall::Preflight));
}

#[test]
fn supervisor_failure_prevents_zellij_invocation() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    // No scene file on disk → the launch path materializes the
    // embedded default scene, which we never want touching the
    // user's real XDG dir. TestEnv already redirected XDG_RUNTIME_DIR
    // via its scrub (unset → std::env::temp_dir fallback).
    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new().fail(CliError::Internal {
        reason: "supervisor exited before signalling ready".to_string(),
    });

    let err = launch::run_with(&mux, &spawner, None, None, &ctx).expect_err("must fail");
    assert!(matches!(err, CliError::Internal { .. }));

    // Critical invariant: if the supervisor handshake fails, zellij
    // is NEVER invoked. That's what prevents orphan sessions.
    let mux_calls = mux.calls();
    assert!(
        !mux_calls
            .iter()
            .any(|c| matches!(c, MultiplexerCall::RunSession { .. })),
        "zellij must not be invoked when supervisor fails to ready, got calls: {mux_calls:?}"
    );
}

#[test]
fn happy_path_no_flags_uses_default_session_name() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(&mux, &spawner, None, None, &ctx).expect("launch ok");

    let run_session = mux
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MultiplexerCall::RunSession { session, layout } => Some((session, layout)),
            _ => None,
        })
        .expect("mux.run_session must have been called");
    assert_eq!(run_session.0, "ark", "default session name is `ark`");
}

#[test]
fn session_flag_propagates_to_mux() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(&mux, &spawner, None, Some("work"), &ctx).expect("launch ok");

    let sess = mux
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MultiplexerCall::RunSession { session, .. } => Some(session),
            _ => None,
        })
        .expect("run_session");
    assert_eq!(sess, "work");
}

#[test]
fn scene_flag_explicit_path_resolves_verbatim() {
    let env = TestEnv::new();
    let ctx = env.ctx();
    let scene_path = env.write_scene_at("custom/my-scene.kdl");

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(
        &mux,
        &spawner,
        Some(scene_path.to_str().unwrap()),
        None,
        &ctx,
    )
    .expect("launch ok");

    // AgentSpec passed to the supervisor must carry the verbatim
    // scene path — that's what lets hot-reload pick up changes.
    let calls = spawner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].spec.scene_path.as_ref(), Some(&scene_path));
}

#[test]
fn scene_flag_bare_name_resolves_under_config_dir() {
    let env = TestEnv::new();
    let ctx = env.ctx();
    let expected = env.write_named_scene(&ctx, "myproject");

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(&mux, &spawner, Some("myproject"), None, &ctx).expect("launch ok");

    let calls = spawner.calls();
    assert_eq!(calls[0].spec.scene_path.as_ref(), Some(&expected));
}

#[test]
fn scene_flag_missing_file_is_not_found() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    let err = launch::run_with(
        &mux,
        &spawner,
        Some("/nonexistent/nowhere.kdl"),
        None,
        &ctx,
    )
    .expect_err("missing scene must error");
    assert!(matches!(err, CliError::NotFound { .. }));

    // Supervisor must not be spawned on scene-resolution failure —
    // we want no orphan state on bad input.
    assert!(spawner.calls().is_empty());
}

#[test]
fn supervisor_receives_spec_with_matching_session_and_scene() {
    let env = TestEnv::new();
    let ctx = env.ctx();
    let scene = env.write_scene_at("project/scene.kdl");

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(
        &mux,
        &spawner,
        Some(scene.to_str().unwrap()),
        Some("mywork"),
        &ctx,
    )
    .expect("launch ok");

    let calls = spawner.calls();
    let spec = &calls[0].spec;
    assert_eq!(spec.name, "mywork");
    assert_eq!(spec.scene_path.as_ref(), Some(&scene));
    assert_eq!(spec.id.name, "mywork");
    assert!(spec.env.is_empty());
    assert!(spec.ext_config.is_empty());
}

#[test]
fn supervisor_called_before_mux_run_session() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(&mux, &spawner, None, None, &ctx).expect("launch ok");

    // Both were called. The run_session call must be AFTER the
    // supervisor was successfully spawned. We proved the ordering
    // property via the supervisor_failure_prevents_zellij_invocation
    // test above; here we just guard that both do get called on the
    // happy path so someone doesn't "fix" the ordering by deleting
    // one of them.
    assert_eq!(spawner.calls().len(), 1, "supervisor spawned");
    assert!(
        mux.calls()
            .iter()
            .any(|c| matches!(c, MultiplexerCall::RunSession { .. })),
        "zellij invoked"
    );
}

#[test]
fn inside_zellij_still_runs_session_with_correct_name() {
    let env = TestEnv::new();
    let ctx = env.ctx();

    // `inside()` flips is_inside to true; the production
    // ZellijMultiplexer dispatches switch-session in that case.
    // The MockMultiplexer records the call regardless of which
    // branch the real impl would pick — the assertion is that the
    // session name is propagated unchanged.
    let mux = MockMultiplexer::new().inside();
    let spawner = InlineSupervisor::new();

    launch::run_with(&mux, &spawner, None, Some("attach-me"), &ctx).expect("launch ok");

    let sess = mux
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MultiplexerCall::RunSession { session, .. } => Some(session),
            _ => None,
        })
        .expect("run_session");
    assert_eq!(sess, "attach-me");
}

#[test]
fn explicit_scene_produces_layout_artifact_path() {
    let env = TestEnv::new();
    let ctx = env.ctx();
    let scene = env.write_scene_at("proj/scene.kdl");

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(
        &mux,
        &spawner,
        Some(scene.to_str().unwrap()),
        None,
        &ctx,
    )
    .expect("launch ok");

    // mux.run_session receives the compiled layout artifact path,
    // not the scene source path. The artifact must exist on disk —
    // zellij itself reads that file when invoked with --layout.
    let layout = mux
        .calls()
        .into_iter()
        .find_map(|c| match c {
            MultiplexerCall::RunSession { layout, .. } => layout,
            _ => None,
        })
        .expect("layout path must be in run_session call");
    assert!(
        layout.exists(),
        "compiled layout artifact must exist on disk at {}",
        layout.display()
    );
    assert!(
        layout.extension().map(|e| e == "kdl").unwrap_or(false),
        "layout artifact must end in .kdl (zellij #4994)"
    );
}

#[test]
fn scene_with_shipped_views_compiles_without_unknown_view_error() {
    // Regression guard on the bug class that T-115 fix addressed:
    // scenes referencing shipped views (`status`, `picker`) failed
    // to compile because the view registry was built without them.
    // The registry builder in `compile.rs` now pre-registers the
    // two shipped views; this test ensures a scene that uses them
    // compiles end-to-end.
    let env = TestEnv::new();
    let ctx = env.ctx();
    let scene_path = env.tmp.path().join("shipped-views.kdl");
    std::fs::write(&scene_path, scene_with_shipped_views_kdl()).unwrap();

    let mux = MockMultiplexer::new();
    let spawner = InlineSupervisor::new();

    launch::run_with(
        &mux,
        &spawner,
        Some(scene_path.to_str().unwrap()),
        None,
        &ctx,
    )
    .expect("shipped-view scene must compile");
}

#[test]
fn preflight_runs_before_any_filesystem_mutation() {
    // When preflight fails, no scene compile is attempted (no
    // layout artifact on disk), no supervisor spec is persisted.
    // This test simulates preflight failure and asserts the scene
    // pipeline was never reached (no state files).
    let env = TestEnv::new();
    let ctx = env.ctx();
    let scene = env.write_scene_at("proj/scene.kdl");

    let mux = MockMultiplexer::new().fail_preflight(CliError::PreflightFail {
        reason: "zellij absent".to_string(),
    });
    let spawner = InlineSupervisor::new();

    let _ = launch::run_with(
        &mux,
        &spawner,
        Some(scene.to_str().unwrap()),
        None,
        &ctx,
    )
    .expect_err("preflight should fail");

    // No layout artifact should have been written. The compile
    // pipeline writes to `$STATE/layouts/<scene_id>.kdl` (or
    // equivalent). If preflight ran first, nothing is there.
    // We conservatively assert the state_dir has no children —
    // launch hasn't mutated it yet.
    let state_entries: Vec<_> = std::fs::read_dir(&ctx.state_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        state_entries.is_empty(),
        "state_dir must be empty when preflight short-circuits, found: {state_entries:?}"
    );
}
