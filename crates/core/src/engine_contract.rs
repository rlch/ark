//! Engine contract suite — trait-level conformance tests that every
//! [`crate::Engine`] implementation must pass.
//!
//! Soul phase 1 T-020 rewrote the portable suite against the new
//! `Engine` trait surface (SessionId-keyed `install_observability`,
//! `CoreEvent` bus). Legacy `AgentId` / `AgentEvent` references are
//! gone.
//!
//! The pattern is the same one the Rust stdlib uses for its collection
//! contract tests: hand the suite a factory closure that mints a fresh
//! `Box<dyn Engine>` plus a bundle of on-disk fixtures, and the suite
//! asserts every scripted scenario the `Engine` trait is contractually
//! required to satisfy.

use std::path::Path;

use ark_test_fixtures::EngineFixtures;
use ark_types::SessionId;

use crate::engine::Engine;

/// Run the portable portion of the Engine contract suite against
/// `factory`. `fixtures` points at the committed `ark-test-fixtures`
/// directories so every engine impl tests against the same golden data.
///
/// Each scenario exercises a single trait method:
///
/// | Scenario                                    | Trait method exercised       |
/// |---------------------------------------------|------------------------------|
/// | `factory_closure_produces_fresh_instance`   | (factory closure)            |
/// | `install_observability_creates_hook_config` | `install_observability`      |
/// | `restore_settings_is_idempotent`            | `install_observability`+teardown |
/// | `auto_approve_permissions_accepts_policy`   | `auto_approve_permissions`   |
/// | `name_is_stable_non_empty_slug`             | `name`                       |
/// | `default_pane_cmd_non_empty`                | `default_pane_cmd`           |
/// | `transcript_path_is_pure`                   | `transcript_path`            |
/// | `fixtures_are_well_formed`                  | (fixture shape gate)         |
///
/// # Panics
/// Panics on the first violated assertion. Tests convert panics into
/// failures, so this is the intended failure mode.
pub fn engine_contract_suite<F>(factory: F, fixtures: &EngineFixtures)
where
    F: Fn() -> Box<dyn Engine>,
{
    factory_closure_produces_fresh_instance(&factory);
    name_is_stable_non_empty_slug(&factory);
    default_pane_cmd_non_empty(&factory);
    transcript_path_is_pure(&factory);
    install_observability_creates_hook_config(&factory);
    restore_settings_is_idempotent(&factory);
    auto_approve_permissions_accepts_policy(&factory);
    fixtures_are_well_formed(fixtures);
}

fn factory_closure_produces_fresh_instance<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let a = factory();
    let b = factory();
    assert_eq!(
        a.name(),
        b.name(),
        "factory closure must produce engines of the same kind \
         (got `{}` and `{}`)",
        a.name(),
        b.name()
    );
    drop(a);
    drop(b);
}

fn name_is_stable_non_empty_slug<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let eng = factory();
    let n = eng.name();
    assert!(
        !n.is_empty(),
        "Engine::name must return a non-empty &'static str"
    );
    assert!(
        n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Engine::name must be a slug (lowercase ascii + digits + dash), got {n:?}"
    );
    assert_eq!(
        eng.name(),
        n,
        "Engine::name must be stable across calls on the same instance"
    );
}

fn default_pane_cmd_non_empty<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let cmd = factory().default_pane_cmd();
    assert!(
        !cmd.is_empty(),
        "Engine::default_pane_cmd must return a non-empty argv"
    );
    assert!(
        cmd.iter().all(|a| !a.is_empty()),
        "Engine::default_pane_cmd argv entries must be non-empty"
    );
}

fn transcript_path_is_pure<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let eng = factory();
    let a = eng.transcript_path(Path::new("/tmp/ark-contract-cwd"));
    let b = eng.transcript_path(Path::new("/tmp/ark-contract-cwd"));
    assert_eq!(
        a, b,
        "Engine::transcript_path must be a pure function of its inputs"
    );
}

fn install_observability_creates_hook_config<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let tmp = tempfile::tempdir().expect("tempdir for install_observability");
    let cwd = tmp.path().to_path_buf();
    let engine = factory();
    let (sink, _rx) = ark_types::channel(8);
    let id = SessionId::new("contract-install");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let handle = rt
        .block_on(engine.install_observability(&id, &cwd, sink))
        .expect("install_observability must succeed on a fresh tempdir");

    assert_eq!(
        handle.engine_name(),
        engine.name(),
        "EngineHandle::engine_name must match the minting engine's name"
    );

    let dir_has_content = std::fs::read_dir(&cwd)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false);
    assert!(
        dir_has_content,
        "install_observability({}) must leave at least one artifact under cwd",
        cwd.display()
    );

    let (sink2, _rx2) = ark_types::channel(8);
    let handle2 = rt
        .block_on(engine.install_observability(&id, &cwd, sink2))
        .expect("install_observability must be idempotent across repeated calls");

    rt.block_on(engine.teardown(handle2))
        .expect("teardown of second handle must succeed");
    rt.block_on(engine.teardown(handle))
        .expect("teardown of first handle must succeed");
}

fn restore_settings_is_idempotent<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    let tmp = tempfile::tempdir().expect("tempdir for restore idempotent");
    let cwd = tmp.path().to_path_buf();
    let engine = factory();
    let (sink, _rx) = ark_types::channel(8);
    let id = SessionId::new("contract-restore");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let handle = rt
        .block_on(engine.install_observability(&id, &cwd, sink))
        .expect("install before restore");

    rt.block_on(engine.teardown(handle))
        .expect("first teardown must succeed");

    let (sink2, _rx2) = ark_types::channel(8);
    let handle2 = rt
        .block_on(engine.install_observability(&id, &cwd, sink2))
        .expect("reinstall after restore must succeed");
    rt.block_on(engine.teardown(handle2))
        .expect("second teardown must succeed");
}

fn auto_approve_permissions_accepts_policy<F>(factory: &F)
where
    F: Fn() -> Box<dyn Engine>,
{
    use crate::engine::ApprovalPolicy;

    let tmp = tempfile::tempdir().expect("tempdir for policy write");
    let cwd = tmp.path().to_path_buf();
    let engine = factory();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    for policy in [
        ApprovalPolicy::Ask,
        ApprovalPolicy::AutoApproveRead,
        ApprovalPolicy::AutoApproveAll,
    ] {
        rt.block_on(engine.auto_approve_permissions(&cwd, policy))
            .unwrap_or_else(|e| {
                panic!("auto_approve_permissions({policy:?}) must succeed, got: {e}")
            });
    }
}

/// Assert the shapes the deferred (engine-crate) timeline scenarios
/// depend on.
fn fixtures_are_well_formed(fixtures: &EngineFixtures) {
    for stem in ["post-tool-use", "stop", "permission-request"] {
        let path = fixtures.hook_payload(stem);
        assert!(
            path.is_file(),
            "hook payload fixture `{stem}` missing at {}",
            path.display()
        );
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read hook payload {}: {e}", path.display()));
        let v: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("hook payload {stem} must parse as JSON: {e}"));
        assert!(
            v.get("hook_event_name").and_then(|x| x.as_str()).is_some(),
            "hook payload {stem} must carry a string `hook_event_name`"
        );
    }

    for stem in ["basic-toolUse", "rotation-scenario", "malformed"] {
        let path = fixtures.transcript(stem);
        assert!(
            path.is_file(),
            "transcript fixture `{stem}` missing at {}",
            path.display()
        );
    }
}

#[cfg(test)]
mod tests {
    //! The contract suite is itself exercised against a minimal MockEngine
    //! so we prove the suite's assertions actually run and fail-loud when
    //! an engine violates them.

    use super::*;
    use crate::engine::{ApprovalPolicy, Engine, EngineHandle as CoreEngineHandle};
    use ark_types::{EventSink, SessionId};
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct MockEngine;

    #[async_trait]
    impl Engine for MockEngine {
        fn name(&self) -> &'static str {
            "mock-engine"
        }

        async fn install_observability(
            &self,
            _id: &SessionId,
            cwd: &Path,
            _sink: EventSink,
        ) -> anyhow::Result<CoreEngineHandle> {
            std::fs::write(cwd.join(".mock-engine-installed"), b"1")?;
            Ok(CoreEngineHandle::new("mock-engine", cwd.to_path_buf()))
        }

        async fn teardown(&self, handle: CoreEngineHandle) -> anyhow::Result<()> {
            if let Ok(cwd) = handle.downcast::<PathBuf>() {
                let marker = cwd.join(".mock-engine-installed");
                if marker.exists() {
                    std::fs::remove_file(&marker)?;
                }
            }
            Ok(())
        }

        fn default_pane_cmd(&self) -> Vec<String> {
            vec!["mock-agent".to_string()]
        }

        fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
            None
        }

        async fn auto_approve_permissions(
            &self,
            _cwd: &Path,
            _policy: ApprovalPolicy,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn mock_engine_passes_contract_suite() {
        let fx = ark_test_fixtures::engine_fixtures();
        engine_contract_suite(|| Box::new(MockEngine), &fx);
    }

    #[test]
    fn contract_rejects_empty_slug_name() {
        struct BadEngine;

        #[async_trait]
        impl Engine for BadEngine {
            fn name(&self) -> &'static str {
                ""
            }

            async fn install_observability(
                &self,
                _id: &SessionId,
                _cwd: &Path,
                _sink: EventSink,
            ) -> anyhow::Result<CoreEngineHandle> {
                unreachable!()
            }

            async fn teardown(&self, _h: CoreEngineHandle) -> anyhow::Result<()> {
                Ok(())
            }

            fn default_pane_cmd(&self) -> Vec<String> {
                vec!["x".into()]
            }

            fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
                None
            }

            async fn auto_approve_permissions(
                &self,
                _cwd: &Path,
                _policy: ApprovalPolicy,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let factory = || -> Box<dyn Engine> { Box::new(BadEngine) };
        let result = std::panic::catch_unwind(|| name_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected empty-name engine to be rejected by name assertion"
        );
    }

    #[test]
    fn contract_rejects_empty_default_pane_cmd() {
        struct NoCmdEngine;

        #[async_trait]
        impl Engine for NoCmdEngine {
            fn name(&self) -> &'static str {
                "no-cmd"
            }

            async fn install_observability(
                &self,
                _id: &SessionId,
                _cwd: &Path,
                _sink: EventSink,
            ) -> anyhow::Result<CoreEngineHandle> {
                unreachable!()
            }

            async fn teardown(&self, _h: CoreEngineHandle) -> anyhow::Result<()> {
                Ok(())
            }

            fn default_pane_cmd(&self) -> Vec<String> {
                vec![]
            }

            fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
                None
            }

            async fn auto_approve_permissions(
                &self,
                _cwd: &Path,
                _policy: ApprovalPolicy,
            ) -> anyhow::Result<()> {
                Ok(())
            }
        }

        let factory = || -> Box<dyn Engine> { Box::new(NoCmdEngine) };
        let result = std::panic::catch_unwind(|| default_pane_cmd_non_empty(&factory));
        assert!(
            result.is_err(),
            "expected empty-cmd engine to be rejected by default_pane_cmd assertion"
        );
    }

    #[test]
    fn fixtures_well_formed_against_committed_fixtures() {
        let fx = ark_test_fixtures::engine_fixtures();
        fixtures_are_well_formed(&fx);
    }
}
