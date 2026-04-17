//! Soul phase 1 T-020 stub of the orchestrator-contract integration test.
//!
//! The pre-soul `orchestrator_contract_suite` (in `ark-core`) reaches
//! into the deleted `AgentSpec` / `Outcome` surface; the portable suite
//! is being rewritten in a later tier. Until then we keep the
//! claude-code-specific assertions (`name()`, PATH-walking `detect_with`)
//! that exercise the new trait surface end-to-end without the fixture
//! infrastructure.

use ark_core::orchestrator::Orchestrator;
use ark_orchestrators_claude_code::ClaudeCodeOrchestrator;

/// claude-code-specific: `name()` is the stable `"claude-code"` slug.
#[test]
fn name_is_claude_code_slug() {
    let orch = ClaudeCodeOrchestrator::new();
    assert_eq!(orch.name(), "claude-code");
}

/// claude-code-specific: `detect_with` returns `false` for an empty
/// synthetic PATH. Uses the test-friendly `detect_with` entry point so
/// the assertion is deterministic regardless of the developer's real
/// PATH.
#[test]
fn detect_with_empty_path_returns_false() {
    let empty_path = std::ffi::OsString::new();
    assert!(
        !ark_orchestrators_claude_code::detect_with(&empty_path),
        "empty PATH must not resolve `claude`"
    );
}
