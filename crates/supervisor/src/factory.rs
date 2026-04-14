//! Factory helpers that mint Engine / Orchestrator / Multiplexer trait
//! objects from v1 scope slugs.
//!
//! Implements the "step 6" hand-off in cavekit-supervisor.md R3:
//!
//! > 6. Instantiates Engine, Orchestrator, Mux via a factory keyed on
//! >    `spec.engine` and `spec.orchestrator`.
//!
//! Each builder consults [`ark_types::scope`] for v1-lock validation (so a
//! typo in `spec.engine = "claud-code"` fails loudly instead of tripping a
//! generic "no matching slug"). Unknown but scope-compliant slugs still
//! return `Err` — v1 ships a fixed set (see the docs on
//! [`ark_types::ENGINES_V1`] / [`ark_types::ORCHESTRATORS_V1`] /
//! [`ark_types::MUX_V1`]).
//!
//! ## Cavekit orchestrator stub
//!
//! [`ark_orchestrators_cavekit`] currently exposes only a `detect` fn —
//! the full `Orchestrator` trait impl lands in T-076. For T-069 we ship a
//! thin stub ([`CavekitOrchestratorStub`]) so `build_orchestrator("cavekit")`
//! returns something that satisfies the trait. The stub delegates its
//! `run` to `ClaudeCodeOrchestrator::run` — a methodology-free passthrough
//! that does exactly what cavekit will do on the "builder" tab before
//! review is wired in. The stub's `name()` is `"cavekit"` so downstream
//! slug checks remain accurate. When T-076 introduces the real
//! `CavekitOrchestrator`, swap this stub for it here and delete the
//! placeholder struct.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ark_core::{Config, Engine, Multiplexer, Orchestrator, World};
use ark_engines_claude_code::engine::ClaudeCodeEngine;
use ark_mux_zellij::ZellijMux;
use ark_orchestrators_claude_code::ClaudeCodeOrchestrator;
use ark_types::{AgentSpec, Outcome, is_v1_engine, is_v1_mux, is_v1_orchestrator};
use async_trait::async_trait;

// ---------------------------------------------------------------- engines ----

/// Mint a concrete `Engine` trait object for `slug`.
pub fn build_engine(slug: &str, _config: &Config) -> Result<Box<dyn Engine>> {
    if !is_v1_engine(slug) {
        return Err(anyhow!(
            "unknown engine slug `{slug}` — v1 ships: {:?}. check spec.engine and ark config.",
            ark_types::ENGINES_V1
        ));
    }
    match slug {
        "claude-code" => Ok(Box::new(ClaudeCodeEngine::new())),
        other => Err(anyhow!(
            "engine slug `{other}` is v1-locked but has no factory branch — plumb it here"
        )),
    }
}

// ---------------------------------------------------------- orchestrators ----

/// Mint a concrete `Orchestrator` trait object for `slug`.
pub fn build_orchestrator(slug: &str, _config: &Config) -> Result<Box<dyn Orchestrator>> {
    if !is_v1_orchestrator(slug) {
        return Err(anyhow!(
            "unknown orchestrator slug `{slug}` — v1 ships: {:?}. check spec.orchestrator and ark config.",
            ark_types::ORCHESTRATORS_V1
        ));
    }
    match slug {
        "cavekit" => Ok(Box::new(CavekitOrchestratorStub::new())),
        "claude-code" => Ok(Box::new(ClaudeCodeOrchestrator::new())),
        other => Err(anyhow!(
            "orchestrator slug `{other}` is v1-locked but has no factory branch — plumb it here"
        )),
    }
}

// --------------------------------------------------------------- mux --------

/// Mint a concrete `Multiplexer` trait object for `slug`.
pub fn build_multiplexer(slug: &str, _config: &Config) -> Result<Arc<dyn Multiplexer>> {
    if !is_v1_mux(slug) {
        return Err(anyhow!(
            "unknown multiplexer slug `{slug}` — v1 ships: {:?}. check config.mux and ark config.",
            ark_types::MUX_V1
        ));
    }
    match slug {
        "zellij" => Ok(Arc::new(ZellijMux::new())),
        other => Err(anyhow!(
            "multiplexer slug `{other}` is v1-locked but has no factory branch — plumb it here"
        )),
    }
}

// ---------------------------------- cavekit orchestrator v1 stub ------------

/// Placeholder `Orchestrator` impl for the `"cavekit"` slug — delegates to
/// [`ClaudeCodeOrchestrator`]'s builder-only run loop. Replace when T-076
/// lands the real `CavekitOrchestrator`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CavekitOrchestratorStub;

impl CavekitOrchestratorStub {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Orchestrator for CavekitOrchestratorStub {
    fn name(&self) -> &'static str {
        "cavekit"
    }

    fn engine(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, cwd: &std::path::Path) -> bool {
        ark_orchestrators_cavekit::detect(cwd)
    }

    async fn run(&self, spec: AgentSpec, world: World) -> Result<Outcome> {
        // Delegate to the claude-code orchestrator's methodology-free
        // passthrough (single builder tab, waits on engine Done, honors
        // cancel). Matches the T-069 acceptance criterion that
        // `build_orchestrator("cavekit")` returns a usable trait object.
        ClaudeCodeOrchestrator::new().run(spec, world).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::placeholder()
    }

    #[test]
    fn build_engine_claude_code_returns_ok() {
        let c = cfg();
        let eng = build_engine("claude-code", &c).expect("claude-code engine");
        assert_eq!(eng.name(), "claude-code");
    }

    #[test]
    fn build_engine_unknown_slug_errors() {
        let c = cfg();
        let err = match build_engine("not-an-engine", &c) {
            Ok(_) => panic!("must error"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("unknown engine"), "got: {msg}");
        assert!(
            msg.contains("claude-code"),
            "remediation hint missing: {msg}"
        );
    }

    #[test]
    fn build_orchestrator_cavekit_returns_ok() {
        let c = cfg();
        let o = build_orchestrator("cavekit", &c).expect("cavekit orchestrator");
        assert_eq!(o.name(), "cavekit");
        assert_eq!(o.engine(), "claude-code");
    }

    #[test]
    fn build_orchestrator_claude_code_returns_ok() {
        let c = cfg();
        let o = build_orchestrator("claude-code", &c).expect("claude-code orchestrator");
        assert_eq!(o.name(), "claude-code");
    }

    #[test]
    fn build_orchestrator_unknown_slug_errors() {
        let c = cfg();
        let err = match build_orchestrator("ralph", &c) {
            Ok(_) => panic!("must error"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("unknown orchestrator"), "got: {msg}");
    }

    #[test]
    fn build_multiplexer_zellij_returns_ok() {
        let c = cfg();
        let m = build_multiplexer("zellij", &c).expect("zellij mux");
        assert_eq!(m.kind(), "zellij");
    }

    #[test]
    fn build_multiplexer_unknown_slug_errors() {
        let c = cfg();
        let err = match build_multiplexer("tmux", &c) {
            Ok(_) => panic!("must error"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("unknown multiplexer"), "got: {msg}");
    }

    #[test]
    fn cavekit_stub_engine_slug_is_claude_code() {
        // Concrete stub should agree with the Orchestrator trait surface.
        let stub = CavekitOrchestratorStub::new();
        assert_eq!(stub.name(), "cavekit");
        assert_eq!(stub.engine(), "claude-code");
    }
}
