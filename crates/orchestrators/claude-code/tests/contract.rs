//! Orchestrator contract suite integration test for
//! [`ark_orchestrators_claude_code::ClaudeCodeOrchestrator`] (T-115,
//! cavekit-architecture.md R1/R2).
//!
//! This file drives the portable
//! [`ark_core::orchestrator_contract_suite`] against the claude-code
//! factory. claude-code uses PATH-based detection as a last-resort
//! match, so the negative-detect scenario (`detect_negative_returns_false`)
//! is opt-out via [`ark_core::OrchestratorFixtures::negative_cwd_is_miss`].
//!
//! The "does not steal from cavekit" rule is enforced at the CLI
//! selection layer (cavekit detect runs first) — not at this
//! orchestrator's `detect()` boundary — so the contract does not assert
//! a cavekit-project cwd misses the claude-code detect.

use ark_core::orchestrator::Orchestrator;
use ark_core::{OrchestratorFixtures, orchestrator_contract_suite};
use ark_orchestrators_claude_code::ClaudeCodeOrchestrator;

fn fixtures() -> OrchestratorFixtures {
    // PATH-based detect: any existing directory is fine as a positive
    // fixture. We reuse the crate dir for a stable, always-present path.
    OrchestratorFixtures {
        positive_cwd: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        // claude-code detect walks `PATH`, not the cwd — an empty
        // tempdir would still match when `claude` is installed on the
        // developer/CI machine, so skip the negative-detect scenario.
        negative_cwd_is_miss: false,
    }
}

fn make_factory() -> impl Fn() -> Box<dyn Orchestrator> {
    || Box::new(ClaudeCodeOrchestrator::new())
}

/// Portable trait-surface contract — same assertions every Orchestrator
/// impl must satisfy.
#[test]
fn claude_code_passes_orchestrator_contract() {
    // Only run the portable suite when `claude` is on PATH; without it
    // the positive-detect scenario would spuriously fail on developer
    // machines without the binary installed. Skip-with-message is
    // preferable to hard-failing CI that lacks a claude install.
    let orch = ClaudeCodeOrchestrator::new();
    if !orch.detect(&fixtures().positive_cwd) {
        eprintln!(
            "skipping claude_code_passes_orchestrator_contract: \
             `claude` not on PATH — contract requires PATH-based detect \
             to match"
        );
        return;
    }
    let fx = fixtures();
    orchestrator_contract_suite(make_factory(), &fx);
}

/// claude-code-specific: `name()` is the stable `"claude-code"` slug.
#[test]
fn name_is_claude_code_slug() {
    let orch = ClaudeCodeOrchestrator::new();
    assert_eq!(orch.name(), "claude-code");
}

/// claude-code-specific: `engine()` also returns `"claude-code"` — the
/// methodology-free passthrough orchestrator pairs 1:1 with its engine.
#[test]
fn engine_returns_claude_code() {
    let orch = ClaudeCodeOrchestrator::new();
    assert_eq!(orch.engine(), "claude-code");
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
