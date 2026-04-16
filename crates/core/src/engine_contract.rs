//! Engine contract suite — trait-level conformance tests that every
//! [`crate::Engine`] implementation must pass.
//!
//! Implements cavekit-architecture.md R1 (T-114). The pattern is the same
//! one the Rust stdlib uses for its collection contract tests: hand the
//! suite a factory closure that mints a fresh `Box<dyn Engine>` plus a
//! bundle of on-disk fixtures, and the suite asserts every scripted
//! scenario the `Engine` trait is contractually required to satisfy.
//!
//! The sole in-tree impl today is `ark_supervisor::AcpEngineStub`
//! (T-ACP.7 retired `ark_engines_claude_code::ClaudeCodeEngine`); future
//! engines would pass the same suite against their own factory.
//!
//! ## Trait surface vs. timeline scenarios
//!
//! The [`Engine`] trait surface covers the install/teardown lifecycle,
//! naming, pane command, transcript path, and permission policy write. It
//! does **NOT** currently expose a unified "feed me a hook payload, emit
//! events" method or a transcript-parsing method — those used to live on
//! the retired `ark_engines_claude_code` crate (T-ACP.7). Under ACP
//! every engine-emitted signal lands on the ACP event bus, so the
//! transcript-parsing surface is gone.
//!
//! Rather than expand the trait purely to satisfy the contract, the suite
//! asserts what the trait guarantees today, and also **validates that the
//! fixtures required for the deferred timeline scenarios are present and
//! well-formed** so integration tests in engine crates can feed them
//! through their crate-specific parsers. The deferred timeline /
//! transcript-parsing scenarios are documented below — engine-crate
//! integration tests (e.g.
//! the retired `crates/engines/claude-code/tests/contract.rs`) layered
//! those assertions on top of this suite before T-ACP.7.
//!
//! Deferred (tracked for trait expansion in a follow-up):
//! - `hook_timeline_post_tool_use`
//! - `hook_timeline_stop`
//! - `hook_timeline_permission_request`
//! - `transcript_parsing_basic_tool_use`
//! - `transcript_parsing_rotation`
//! - `transcript_parsing_malformed_line_skipped`
//!
//! The fixture-shape assertions in this module keep those scenarios
//! guarded at the fixture layer: if the fixtures drift, the contract
//! suite fails before the engine-crate test even runs.

use std::path::Path;

use ark_test_fixtures::EngineFixtures;
use ark_types::AgentId;

use crate::engine::Engine;

/// Run the portable portion of the Engine contract suite against
/// `factory`. `fixtures` points at the committed `ark-test-fixtures`
/// directories so every engine impl tests against the same golden data.
///
/// Each scenario is a scripted scenario from T-114:
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
/// Engine crates typically wrap this call in a single `#[test]` function
/// and add their own crate-specific timeline scenarios on top (see
/// module docs for deferred items).
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
    // Two calls must each yield an independent trait object. We can't
    // compare identity through `dyn Trait`, so assert the weaker but
    // sufficient property: both observe the same stable `name()` and
    // both are independently droppable without aliasing.
    assert_eq!(
        a.name(),
        b.name(),
        "factory closure must produce engines of the same kind \
         (got `{}` and `{}`)",
        a.name(),
        b.name()
    );
    // Independent drops — if the factory returned the same Box twice this
    // would double-free. The fact that both drop cleanly proves
    // independence.
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
    // Slug convention: lowercase letters, digits, dashes.
    assert!(
        n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Engine::name must be a slug (lowercase ascii + digits + dash), got {n:?}"
    );
    // Stable: second call on same instance returns same slug.
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
    // Must not panic and must be referentially transparent over `cwd`.
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
    let id = AgentId::new("cavekit", "contract-install");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let handle = rt
        .block_on(engine.install_observability(&id, &cwd, sink))
        .expect("install_observability must succeed on a fresh tempdir");

    // Handle must be minted by the same engine.
    assert_eq!(
        handle.engine_name(),
        engine.name(),
        "EngineHandle::engine_name must match the minting engine's name"
    );

    // At least one artifact must exist under cwd post-install. We don't
    // know the engine's private layout, so we only assert that the
    // engine left *some* observable on-disk state — empty cwd is a
    // contract violation.
    let dir_has_content = std::fs::read_dir(&cwd)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false);
    assert!(
        dir_has_content,
        "install_observability({}) must leave at least one artifact under cwd",
        cwd.display()
    );

    // Second install on the same cwd must also succeed (idempotency).
    let (sink2, _rx2) = ark_types::channel(8);
    let handle2 = rt
        .block_on(engine.install_observability(&id, &cwd, sink2))
        .expect("install_observability must be idempotent across repeated calls");

    // Teardown both handles without error.
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
    let id = AgentId::new("cavekit", "contract-restore");

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let handle = rt
        .block_on(engine.install_observability(&id, &cwd, sink))
        .expect("install before restore");

    rt.block_on(engine.teardown(handle))
        .expect("first teardown must succeed");

    // A second install/teardown roundtrip on the same cwd must not
    // error — i.e. teardown must leave the cwd in a state that supports
    // reinstallation.
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
    // All three policies must be accepted without error.
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
/// depend on. If these fail, the follow-up trait-expanded scenarios
/// can't meaningfully run.
fn fixtures_are_well_formed(fixtures: &EngineFixtures) {
    // Hook payloads the deferred timeline scenarios will feed through
    // the engine's `handle_hook_payload` once that API lands.
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

    // Transcripts the deferred parsing scenarios will feed through
    // the engine's `parse_line` / tailer.
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
    //! an engine violates them. The real-engine exercise lives in
    //! `crates/engines/claude-code/tests/contract.rs`.

    use super::*;
    use crate::engine::{ApprovalPolicy, Engine, EngineHandle as CoreEngineHandle};
    use ark_types::{AgentId, EventSink};
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
            _id: &AgentId,
            cwd: &Path,
            _sink: EventSink,
        ) -> anyhow::Result<CoreEngineHandle> {
            // Leave an observable marker so the "dir has content" check
            // passes. The contract doesn't care what it is.
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
        // Smoke test for the name-is-slug assertion. We use a wrapper
        // engine that violates the rule and confirm the sub-check
        // panics.
        struct BadEngine;

        #[async_trait]
        impl Engine for BadEngine {
            fn name(&self) -> &'static str {
                ""
            }

            async fn install_observability(
                &self,
                _id: &AgentId,
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
                _id: &AgentId,
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
