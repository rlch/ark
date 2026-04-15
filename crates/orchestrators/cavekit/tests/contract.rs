//! Orchestrator contract suite integration test for
//! [`ark_orchestrators_cavekit::CavekitOrchestrator`] (T-115,
//! cavekit-architecture.md R1/R2).
//!
//! This file drives the portable
//! [`ark_core::orchestrator_contract_suite`] against the cavekit factory
//! and layers on cavekit-specific assertions (negative detect, stable
//! engine slug) that the portable suite already covers.
//!
//! Every scenario is a discrete `#[test]` so `cargo test -p
//! ark-orchestrators-cavekit --test contract` names the failing scenario
//! without needing to parse a composite test log.

use std::path::PathBuf;

use ark_core::orchestrator::Orchestrator;
use ark_core::{OrchestratorFixtures, orchestrator_contract_suite};
use ark_orchestrators_cavekit::CavekitOrchestrator;
use ark_test_fixtures::loaders::cavekit_fixture_dir;

fn fixtures() -> OrchestratorFixtures {
    OrchestratorFixtures {
        positive_cwd: cavekit_fixture_dir(),
        // Cavekit detection is cwd-based: an empty tempdir is a miss.
        negative_cwd_is_miss: true,
    }
}

fn make_factory() -> impl Fn() -> Box<dyn Orchestrator> {
    || Box::new(CavekitOrchestrator::new())
}

/// Portable trait-surface contract — same assertions every Orchestrator
/// impl must satisfy.
#[test]
fn cavekit_passes_orchestrator_contract() {
    let fx = fixtures();
    orchestrator_contract_suite(make_factory(), &fx);
}

/// Sanity: the positive fixture is the committed cavekit-project layout
/// (has `context/sites/`). If this moves, the contract suite would start
/// silently passing against an unrelated path.
#[test]
fn positive_fixture_is_the_committed_cavekit_project() {
    let fx = fixtures();
    let sites = fx.positive_cwd.join("context").join("sites");
    assert!(
        sites.is_dir(),
        "positive_cwd must expose the cavekit project sites/ dir at {}",
        sites.display()
    );
}

/// Cavekit-specific: `name()` returns the stable `"cavekit"` slug.
#[test]
fn name_is_cavekit_slug() {
    let orch = CavekitOrchestrator::new();
    assert_eq!(orch.name(), "cavekit");
}

/// Cavekit-specific: `engine()` pairs with claude-code by default.
#[test]
fn engine_pairs_with_claude_code() {
    let orch = CavekitOrchestrator::new();
    assert_eq!(orch.engine(), "claude-code");
}

/// Cavekit-specific: an empty tempdir does not match detect.
#[test]
fn detect_rejects_empty_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let orch = CavekitOrchestrator::new();
    assert!(
        !orch.detect(tmp.path()),
        "empty tempdir must not match cavekit detect ({})",
        tmp.path().display()
    );
}

/// Cavekit-specific: a cwd with `.cavekit/config` is a positive match.
#[test]
fn detect_accepts_dot_cavekit_config() {
    let tmp = tempfile::tempdir().unwrap();
    let cav = tmp.path().join(".cavekit");
    std::fs::create_dir_all(&cav).unwrap();
    std::fs::write(cav.join("config"), "").unwrap();
    let orch = CavekitOrchestrator::new();
    assert!(
        orch.detect(tmp.path()),
        "cavekit detect must accept .cavekit/config ({})",
        tmp.path().display()
    );
}

#[allow(dead_code)]
fn _typecheck_positive_cwd_is_pathbuf(fx: &OrchestratorFixtures) -> &PathBuf {
    &fx.positive_cwd
}
