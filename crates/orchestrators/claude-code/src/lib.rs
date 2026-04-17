//! ClaudeCodeOrchestrator — soul phase 1 stub adoption (T-020).
//!
//! Pre-soul this orchestrator owned a single "builder" tab, forwarded
//! engine events as-is via the shared bus, waited on the engine's
//! `Done` event or supervisor cancel, and returned an `Outcome`. The
//! impl depended on `AgentEvent`, `AgentSpec`, `Outcome`, and
//! `TabRole` pattern matching that no longer exists under
//! cavekit-soul Phase 1.
//!
//! T-020 adopts the new trait surface:
//!
//! * `run` takes `&SessionSpec` and returns `Result<(), anyhow::Error>`.
//! * Internal config that previously read off
//!   `AgentSpec.orchestrator / .engine / .runner_config` is now keyed
//!   off `SessionSpec.ext_config["claude-code"]`. The local
//!   [`ClaudeCodeConfig`] struct typechecks the JSON shape and falls
//!   back to defaults on absence.
//!
//! The methodology body — tab graph, event loop — is staged out
//! behind a `todo!()` to land in a Phase 2+ packet once the extension
//! event surface is in place. The stub still:
//!
//! * Implements [`Orchestrator`] cleanly (compiles + cargo-check
//!   passes for `ark-orchestrators-claude-code`).
//! * Preserves `detect()` so the CLI's orchestrator-selection chain
//!   (cavekit detect → claude-code detect → bare) keeps working.
//!
//! ## Orchestrator selection ordering
//!
//! `detect` is a last-resort match: it returns `true` if the `claude`
//! binary is on `PATH`. The rule "does not steal from cavekit" is
//! enforced by the orchestrator selection order at the CLI layer:
//! the CLI runs `CavekitOrchestrator::detect` first and only falls
//! back to `ClaudeCodeOrchestrator::detect` when cavekit does not
//! match.

use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;
use ark_core::{Orchestrator, World};
use ark_types::SessionSpec;
use async_trait::async_trait;
use serde::Deserialize;

// ------------------------------------------------------------------ detect --

/// Last-resort detect: returns `true` when a `claude` binary is on `PATH`.
pub fn detect(_cwd: &Path) -> bool {
    match std::env::var_os("PATH") {
        Some(p) => detect_with(&p),
        None => false,
    }
}

/// Test-friendly detection: walk the provided `PATH` env value, look for an
/// executable named `claude`.
pub fn detect_with(path_env: &OsStr) -> bool {
    let name = OsStr::new("claude");
    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let mut with_ext = candidate.clone();
                with_ext.set_extension(ext);
                if is_executable_file(&with_ext) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ------------------------------------------------------------------ config --

/// Claude-code's bucket inside `SessionSpec.ext_config["claude-code"]`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ClaudeCodeConfig {
    /// Layout stem to hand to the multiplexer for the builder tab.
    /// Defaults to `"builder"` per kit R2.
    pub layout: Option<String>,
}

impl ClaudeCodeConfig {
    /// Resolve the claude-code bucket from a [`SessionSpec`].
    pub fn from_spec(spec: &SessionSpec) -> Self {
        spec.ext_config
            .get("claude-code")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Layout stem with the kit default applied.
    pub fn layout_or_default(&self) -> &str {
        self.layout.as_deref().unwrap_or("builder")
    }
}

// -------------------------------------------------------------- orchestrator --

/// Methodology-free passthrough orchestrator. Soul phase 1 stub.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeOrchestrator;

impl ClaudeCodeOrchestrator {
    pub const fn new() -> Self {
        Self
    }

    /// Kit-default layout stem (R2).
    pub fn default_layout(&self) -> &'static str {
        "builder"
    }
}

#[async_trait]
impl Orchestrator for ClaudeCodeOrchestrator {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, cwd: &Path) -> bool {
        detect(cwd)
    }

    async fn run(&self, spec: &SessionSpec, _world: World) -> Result<()> {
        let cfg = ClaudeCodeConfig::from_spec(spec);
        let layout = cfg.layout_or_default().to_string();
        tracing::warn!(
            session = %spec.id.as_path_leaf(),
            layout = %layout,
            "claude-code orchestrator: soul phase 1 stub — methodology body \
             (tab graph + Done resolver) re-lands in Phase 2+"
        );
        // Phase 2+: spawn builder tab, await engine Done / cancel.
        Ok(())
    }
}

// ------------------------------------------------------------------ tests ----

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use ark_core::Config;
    use ark_mux_zellij::ZellijMux;
    use ark_types::{CancellationToken, SessionId, StateLayout};

    #[test]
    fn name_is_claude_code() {
        assert_eq!(ClaudeCodeOrchestrator::new().name(), "claude-code");
    }

    #[test]
    fn detect_with_empty_path_returns_false() {
        let empty = std::ffi::OsString::new();
        assert!(!detect_with(&empty));
    }

    #[test]
    fn config_falls_back_to_default_on_missing_bucket() {
        let spec = sample_spec(BTreeMap::new());
        let cfg = ClaudeCodeConfig::from_spec(&spec);
        assert_eq!(cfg.layout_or_default(), "builder");
    }

    #[test]
    fn config_reads_layout_override() {
        let mut ext = BTreeMap::new();
        ext.insert(
            "claude-code".to_string(),
            serde_json::json!({ "layout": "focused" }),
        );
        let spec = sample_spec(ext);
        let cfg = ClaudeCodeConfig::from_spec(&spec);
        assert_eq!(cfg.layout_or_default(), "focused");
    }

    #[tokio::test]
    async fn run_returns_ok_in_stub_phase() {
        let spec = sample_spec(BTreeMap::new());
        let world = make_world();
        let orch = ClaudeCodeOrchestrator::new();
        orch.run(&spec, world).await.expect("stub returns Ok");
    }

    fn sample_spec(
        ext: BTreeMap<String, serde_json::Value>,
    ) -> SessionSpec {
        SessionSpec {
            id: SessionId::new("claude"),
            name: "claude".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: ext,
        }
    }

    fn make_world() -> World {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let (events, _rx) = ark_types::channel(8);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        World::new(mux, events, cancel, hooks_dir, state, config)
    }
}
