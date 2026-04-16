//! Factory helpers that mint Engine / Orchestrator trait objects and the
//! concrete `ZellijMux` from v1 scope slugs.
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
//! ## Cavekit orchestrator
//!
//! T-083 wires the real [`ark_orchestrators_cavekit::CavekitOrchestrator`]
//! here (previously a stub delegating to `ClaudeCodeOrchestrator`). The
//! full orchestrator spawns the R4–R8 watchers and runs the R9 done-signal
//! resolver.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ark_core::{Config, Engine, Orchestrator};
use ark_mux_zellij::ZellijMux;
use ark_orchestrators_cavekit::CavekitOrchestrator;
use ark_orchestrators_claude_code::ClaudeCodeOrchestrator;
use ark_types::{is_v1_engine, is_v1_mux, is_v1_orchestrator};
use thiserror::Error;

use crate::engine_stub::AcpEngineStub;

// ---------------------------------------------------------------------------
// SupervisorError — typed-error surface shared by the supervisor crate.
// ---------------------------------------------------------------------------

/// Thin typed-error surface used by the supervisor's non-trait entry
/// points (engine resolution, future ACP wiring).
///
/// Kept as a lightweight `thiserror` enum rather than a miette
/// diagnostic because most supervisor-level errors already carry rich
/// underlying `anyhow::Error` context; this surface exists for the few
/// paths (engine resolution, permission dispatch) where the caller
/// wants a typed discriminant instead of a stringy error.
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// `--engine NAME` was supplied but `NAME` is not declared in
    /// `config.engines` and is not a shipped default
    /// (`claude` / `codex` / `gemini-cli`).
    ///
    /// T-ACP.4a: surfaced by
    /// [`crate::engine_resolution::resolve_engine`] when the CLI flag
    /// fails to resolve at rung 1.
    #[error(
        "unknown engine `{name}` in --engine flag — known engines: {known_list}",
        known_list = known.join(", ")
    )]
    UnknownEngine {
        /// The unknown engine name the caller asked for.
        name: String,
        /// Engines currently known to the supervisor (from both
        /// `config.engines` + the shipped defaults).
        known: Vec<String>,
    },

    /// Scene contains BOTH an inline `engine { }` block AND a
    /// `use "engine-*"` extension — R17 intra-scene mutual exclusion.
    ///
    /// T-ACP.4b: surfaced by
    /// [`crate::engine_resolution::resolve_engine`] whenever the scene
    /// document carries a `SceneNode.engine` AND `resolved_uses`
    /// contains an extension whose name matches the `engine-*`
    /// convention. The scene compile pipeline is the canonical home
    /// for this error (see
    /// [`ark_scene::error::SceneError::EngineConflict`]); surfacing it
    /// in the resolver is a defense-in-depth fallback for callers that
    /// skipped the compile-pass check.
    #[error(
        "scene declares both an inline `engine` block and a `use \"{use_name}\"` engine extension — pick one"
    )]
    EngineConflict {
        /// Name of the conflicting `use` target.
        use_name: String,
    },
}

// ---------------------------------------------------------------- engines ----

/// Mint a concrete `Engine` trait object for `slug`.
///
/// T-ACP.7 retired the legacy `ark-engines-claude-code` crate; the
/// factory now returns a thin [`AcpEngineStub`] for every slug
/// because the real engine lifecycle lives on the ACP client side
/// (spawned via
/// [`crate::engine_resolution::resolve_engine`] + `AcpClient::spawn`).
/// The Engine trait object is retained only for the pieces of the
/// boot sequence that haven't been migrated off it yet (orchestrator
/// `engine()` slug, install/teardown symmetry, `default_pane_cmd`).
pub fn build_engine(slug: &str, _config: &Config) -> Result<Box<dyn Engine>> {
    if !is_v1_engine(slug) {
        return Err(anyhow!(
            "unknown engine slug `{slug}` — v1 ships: {:?}. check spec.engine and ark config.",
            ark_types::ENGINES_V1
        ));
    }
    Ok(Box::new(AcpEngineStub::new(slug)))
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
        "cavekit" => Ok(Box::new(CavekitOrchestrator::new())),
        "claude-code" => Ok(Box::new(ClaudeCodeOrchestrator::new())),
        other => Err(anyhow!(
            "orchestrator slug `{other}` is v1-locked but has no factory branch — plumb it here"
        )),
    }
}

// --------------------------------------------------------------- mux --------

/// Mint a concrete `ZellijMux` for `slug`.
///
/// The slug is still validated against [`ark_types::MUX_V1`] so a typo
/// (`"zllij"`) fails with the same actionable error as the engine /
/// orchestrator factories. v1 only ships the `zellij` branch; adding a
/// second concrete multiplexer would require a refactor here (and the
/// deferred `MUX_V1` downscope noted in the tracking doc for this
/// revision).
pub fn build_multiplexer(slug: &str, _config: &Config) -> Result<Arc<ZellijMux>> {
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
    fn cavekit_orchestrator_engine_slug_is_claude_code() {
        // The real CavekitOrchestrator (T-083) declares the claude-code
        // engine, matching the factory's expectations.
        let o = CavekitOrchestrator::new();
        assert_eq!(o.name(), "cavekit");
        assert_eq!(o.engine(), "claude-code");
    }
}
