//! Soul phase 1 T-020 stub of the orchestrator-contract integration test.
//!
//! The pre-soul `orchestrator_contract_suite` (in `ark-core`) and the
//! `ark-test-fixtures::loaders::cavekit_fixture_dir` helper both reach
//! into the deleted `AgentSpec` / `Outcome` surface; the portable suite
//! is being rewritten in a later tier. Until then we keep the
//! cavekit-specific assertions (`name()`, `detect()` heuristics) that
//! exercise the new trait surface end-to-end without the fixture
//! infrastructure.

use ark_core::orchestrator::Orchestrator;
use ark_orchestrators_cavekit::CavekitOrchestrator;

/// Cavekit-specific: `name()` returns the stable `"cavekit"` slug.
#[test]
fn name_is_cavekit_slug() {
    let orch = CavekitOrchestrator::new();
    assert_eq!(orch.name(), "cavekit");
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
