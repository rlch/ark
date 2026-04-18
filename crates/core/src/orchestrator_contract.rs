//! Orchestrator contract suite — trait-level conformance tests that
//! every [`crate::Orchestrator`] implementation must pass.
//!
//! Soul phase 1 T-020 narrowed the `Orchestrator` trait surface to
//! `name`, `detect`, and `run(&SessionSpec, World) -> Result<()>`.
//! There is no `engine()` slug and no `Outcome` on the new trait.
//! The suite accordingly asserts only what the new trait guarantees:
//!
//! - `name()` is a non-empty slug, stable across calls.
//! - `detect()` matches an orchestrator-supplied positive fixture and
//!   (optionally) rejects an empty tempdir.
//! - `run()` with a `SessionSpec` + minimal `World` returns without
//!   panic when the bus stays empty and the cancel token fires.
//!
//! Crate-local tests on each orchestrator add methodology-specific
//! scenarios on top (e.g. cavekit-only assertions in
//! `crates/orchestrators/cavekit/tests/contract.rs`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ark_mux_zellij::ZellijMux;
use ark_types::{CancellationToken, SessionId, SessionSpec, StateLayout, channel};

use crate::config::Config;
use crate::orchestrator::{Orchestrator, World};

/// Bundle of fixture inputs consumed by the Orchestrator contract suite.
#[derive(Debug, Clone)]
pub struct OrchestratorFixtures {
    /// Absolute path to a cwd the orchestrator under test is *expected*
    /// to match via `detect()`.
    pub positive_cwd: PathBuf,
    /// When `true`, the contract asserts `detect()` returns `false` on a
    /// fresh empty tempdir. Orchestrators with PATH-based (rather than
    /// cwd-based) detection should set this to `false`.
    pub negative_cwd_is_miss: bool,
}

/// Run the portable portion of the Orchestrator contract suite.
pub fn orchestrator_contract_suite<F>(factory: F, fixtures: &OrchestratorFixtures)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    factory_closure_produces_fresh_instance(&factory);
    name_is_stable_non_empty_slug(&factory);
    detect_positive_returns_true(&factory, fixtures);
    if fixtures.negative_cwd_is_miss {
        detect_negative_returns_false(&factory);
    }
    run_cancel_returns_ok(&factory);
}

fn factory_closure_produces_fresh_instance<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let a = factory();
    let b = factory();
    assert_eq!(
        a.name(),
        b.name(),
        "factory closure must produce orchestrators of the same kind \
         (got `{}` and `{}`)",
        a.name(),
        b.name()
    );
    drop(a);
    drop(b);
}

fn name_is_stable_non_empty_slug<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    let n = orch.name();
    assert!(
        !n.is_empty(),
        "Orchestrator::name must return a non-empty &'static str"
    );
    assert!(
        n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Orchestrator::name must be a slug (lowercase ascii + digits + dash), got {n:?}"
    );
    assert_eq!(
        orch.name(),
        n,
        "Orchestrator::name must be stable across calls on the same instance"
    );
}

fn detect_positive_returns_true<F>(factory: &F, fixtures: &OrchestratorFixtures)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    assert!(
        fixtures.positive_cwd.exists(),
        "positive_cwd fixture must exist on disk: {}",
        fixtures.positive_cwd.display()
    );
    assert!(
        orch.detect(&fixtures.positive_cwd),
        "Orchestrator::detect({}) must return true for the positive fixture",
        fixtures.positive_cwd.display()
    );
}

fn detect_negative_returns_false<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    let tmp = tempfile::tempdir().expect("tempdir for detect_negative");
    assert!(
        !orch.detect(tmp.path()),
        "Orchestrator::detect({}) on an empty tempdir must return false \
         for orchestrators that opt in via negative_cwd_is_miss",
        tmp.path().display()
    );
}

/// Drive `run` to completion by cancelling the world token and asserting
/// the orchestrator returns `Ok(())` within a bounded time window.
/// Deeper lifecycle assertions (tab creation, cascade event emission)
/// are orchestrator-specific and live in per-crate tests.
fn run_cancel_returns_ok<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime");

    rt.block_on(async {
        let orch = factory();
        let tmp = tempfile::tempdir().expect("tempdir for run smoke");
        let cwd = tmp.path().to_path_buf();

        let spec = SessionSpec {
            id: SessionId::new("contract-smoke"),
            name: "contract-smoke".to_string(),
            scene_path: None,
            cwd: cwd.clone(),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: BTreeMap::new(),
        };

        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux: Arc<ZellijMux> = Arc::new(mux);

        let (events, _rx) = channel(256);
        let cancel = CancellationToken::new();
        let hooks_dir = tmp.path().join(".ark-hooks");
        let state = Arc::new(StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("runtime"),
            tmp.path().join("cfg"),
        ));
        let config = Arc::new(Config::placeholder());

        let world = World::new(
            mux.clone(),
            events.clone(),
            cancel.clone(),
            hooks_dir,
            state,
            config,
        );

        // Cancel shortly after run subscribes so the orchestrator has an
        // unambiguous signal to return.
        let cancel_task = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                cancel.cancel();
            })
        };

        let res = tokio::time::timeout(Duration::from_secs(10), orch.run(&spec, world))
            .await
            .expect("Orchestrator::run must return within 10s of cancel");

        cancel_task.abort();

        res.expect("Orchestrator::run must return Ok(()) on a cancel-driven exit");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use std::path::Path;

    /// Minimal orchestrator that satisfies the contract.
    struct MockOrchestrator {
        name: &'static str,
    }

    #[async_trait]
    impl Orchestrator for MockOrchestrator {
        fn name(&self) -> &'static str {
            self.name
        }
        fn detect(&self, _cwd: &Path) -> bool {
            true
        }
        async fn run(&self, _spec: &SessionSpec, world: World) -> anyhow::Result<()> {
            // Wait for cancel, then return.
            world.cancel.cancelled().await;
            Ok(())
        }
    }

    fn mock_fixtures() -> OrchestratorFixtures {
        OrchestratorFixtures {
            positive_cwd: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            negative_cwd_is_miss: false,
        }
    }

    #[test]
    fn mock_orchestrator_passes_contract_suite() {
        let fx = mock_fixtures();
        orchestrator_contract_suite(
            || -> Box<dyn Orchestrator> { Box::new(MockOrchestrator { name: "mock" }) },
            &fx,
        );
    }

    #[test]
    fn contract_rejects_empty_name_slug() {
        let factory = || -> Box<dyn Orchestrator> { Box::new(MockOrchestrator { name: "" }) };
        let result = std::panic::catch_unwind(|| name_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected empty-name orchestrator to be rejected by name assertion"
        );
    }

    #[test]
    fn contract_rejects_uppercase_name_slug() {
        let factory =
            || -> Box<dyn Orchestrator> { Box::new(MockOrchestrator { name: "CAVEKIT" }) };
        let result = std::panic::catch_unwind(|| name_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected uppercase name slug to be rejected by name assertion"
        );
    }
}
