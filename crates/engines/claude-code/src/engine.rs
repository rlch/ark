//! `ClaudeCodeEngine` — the `ark_core::Engine` trait impl composing this
//! crate's primitives (settings, permission, handle, preflight, transcript).
//!
//! This is a MINIMAL composition added as part of T-069 so the supervisor
//! factory (`ark_supervisor::factory::build_engine`) has a single concrete
//! engine it can mint. The richer integration (transcript tailing + stall
//! watcher + done watcher wired into the returned handle) is intentionally
//! narrow here — it lands fully in a later packet; for T-069 we need:
//!
//! 1. `install_observability(cwd, sink)` → inject hooks + write permission
//!    policy file + return a crate-level [`crate::EngineHandle`] wrapped in
//!    [`ark_core::EngineHandle`].
//! 2. `teardown(handle)` → downcast to our handle and delegate to its
//!    [`crate::EngineHandle::teardown`], which restores the settings file.
//! 3. `preflight(spec)` is exposed via the free fn
//!    [`crate::preflight::preflight`]; the trait surface does NOT include
//!    a `preflight` method (that call site is explicit at the supervisor —
//!    see `ark_supervisor::orchestration::run_supervisor` step 8), so this
//!    module provides only the trait-required methods.
//! 4. `default_pane_cmd()` returns `["claude"]` — the canonical launch
//!    command.
//! 5. `transcript_path(cwd)` returns `None` at this layer: we do not know
//!    the Claude session id until the agent process emits a transcript
//!    file; that's resolved separately via
//!    [`crate::transcript::transcript_path`] inside the tailer.
//! 6. `auto_approve_permissions(cwd, policy)` writes the policy file under
//!    the supervisor-managed policy dir via
//!    [`ark_types::permission::write_policy_file`]. In v1 the supervisor
//!    drives this once at install time using the spec's state dir; exposed
//!    here so the trait is complete.
//!
//! The crate version embedded in the hooks marker is read from
//! `CARGO_PKG_VERSION` at build time; callers that need to pin a specific
//! version string pass it via the richer `settings::inject_hooks` API
//! directly.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ark_core::engine::{ApprovalPolicy, Engine, EngineHandle as CoreEngineHandle};
use ark_types::{AgentId, EventSink, PermissionPolicy};
use async_trait::async_trait;

use crate::handle::EngineHandle as ClaudeHandle;
use crate::settings::inject_hooks;

/// The v1 event set ark-hook wires into Claude Code's `settings.local.json`.
const DEFAULT_HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Claude Code engine adapter. See module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeEngine;

impl ClaudeCodeEngine {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Engine for ClaudeCodeEngine {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    async fn install_observability(
        &self,
        id: &AgentId,
        cwd: &Path,
        _sink: EventSink,
    ) -> Result<CoreEngineHandle> {
        // The supervisor passes the authoritative `AgentId` from the spec
        // (see [`Engine::install_observability`] contract). We key every
        // hook command + the teardown handle on that id so events published
        // by `ark-hook` flow under the identity the supervisor is listening
        // for (cavekit-engines-claude-code R1 / F-085).
        inject_hooks(cwd, id, DEFAULT_HOOK_EVENTS, env!("CARGO_PKG_VERSION"))
            .with_context(|| format!("inject ark hooks into {}", cwd.display()))?;

        let handle = ClaudeHandle::new(cwd.to_path_buf(), id.clone());

        Ok(CoreEngineHandle::new("claude-code", handle))
    }

    async fn teardown(&self, handle: CoreEngineHandle) -> Result<()> {
        match handle.downcast::<ClaudeHandle>() {
            Ok(boxed) => (*boxed).teardown().await,
            Err(foreign) => Err(anyhow::anyhow!(
                "ClaudeCodeEngine::teardown: foreign EngineHandle (engine_name = {})",
                foreign.engine_name()
            )),
        }
    }

    fn default_pane_cmd(&self) -> Vec<String> {
        vec!["claude".to_string()]
    }

    fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
        // We don't know the Claude session id at this layer — the tailer
        // resolves it against the live `~/.claude/projects/{slug}/` dir.
        None
    }

    async fn auto_approve_permissions(&self, cwd: &Path, policy: ApprovalPolicy) -> Result<()> {
        let wire = match policy {
            ApprovalPolicy::Ask => PermissionPolicy::Ask,
            ApprovalPolicy::AutoApproveRead => PermissionPolicy::AutoApproveRead,
            ApprovalPolicy::AutoApproveAll => PermissionPolicy::AutoApproveAll,
        };
        ark_types::permission::write_policy_file(cwd, wire)
            .with_context(|| format!("write permission policy under {}", cwd.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn engine_name_is_claude_code() {
        assert_eq!(ClaudeCodeEngine::new().name(), "claude-code");
    }

    #[test]
    fn default_pane_cmd_is_claude() {
        assert_eq!(ClaudeCodeEngine::new().default_pane_cmd(), vec!["claude"]);
    }

    #[test]
    fn transcript_path_is_none_at_trait_layer() {
        assert!(
            ClaudeCodeEngine::new()
                .transcript_path(Path::new("/tmp"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn install_then_teardown_roundtrip_restores_settings() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();
        std::fs::write(
            cwd.join(".claude").join("settings.local.json"),
            br#"{"permissions":{"allow":["Read"]}}"#,
        )
        .unwrap();

        let engine = ClaudeCodeEngine::new();
        let (sink, _rx) = ark_types::channel(8);
        let id = AgentId::new("cavekit", "install-roundtrip");
        let handle = engine
            .install_observability(&id, &cwd, sink)
            .await
            .expect("install");
        assert_eq!(handle.engine_name(), "claude-code");

        // Backup must exist post-install.
        assert!(
            cwd.join(".claude")
                .join("settings.local.json.ark-backup")
                .exists()
        );

        engine.teardown(handle).await.expect("teardown");

        // Backup should be gone after teardown.
        assert!(
            !cwd.join(".claude")
                .join("settings.local.json.ark-backup")
                .exists()
        );
    }

    /// F-085 regression: the injected ark-hook command must contain the
    /// REAL agent id the supervisor passed in — not a synthetic fabricated
    /// one. Hook events emitted by the real Claude Code process must be
    /// keyed on the id the supervisor is subscribed to.
    #[tokio::test]
    async fn install_observability_injects_real_agent_id_into_hook_cmd() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        std::fs::create_dir_all(cwd.join(".claude")).unwrap();

        let engine = ClaudeCodeEngine::new();
        let (sink, _rx) = ark_types::channel(8);
        let real_id = AgentId::new("cavekit", "f085-regression");
        let real_id_str = real_id.as_str().to_string();

        let handle = engine
            .install_observability(&real_id, &cwd, sink)
            .await
            .expect("install");

        // Read back the injected settings.local.json and confirm the hook
        // command wires the REAL id.
        let raw = std::fs::read_to_string(cwd.join(".claude").join("settings.local.json"))
            .expect("settings.local.json written");
        assert!(
            raw.contains(&format!("ark-hook --id {real_id_str}")),
            "expected hook command keyed on real id `{real_id_str}`, got: {raw}"
        );

        // And confirm the returned handle remembers the same id (used at
        // teardown for log context + hook stripping).
        let inner = handle
            .downcast::<ClaudeHandle>()
            .expect("downcast claude handle");
        assert_eq!(inner.agent_id(), &real_id);
    }

    #[tokio::test]
    async fn auto_approve_permissions_writes_policy_file() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let engine = ClaudeCodeEngine::new();
        engine
            .auto_approve_permissions(&cwd, ApprovalPolicy::AutoApproveRead)
            .await
            .expect("write policy");
        let p = ark_types::permission::read_policy_file(&cwd);
        assert_eq!(p, PermissionPolicy::AutoApproveRead);
    }

    #[tokio::test]
    async fn teardown_foreign_handle_errors() {
        let engine = ClaudeCodeEngine::new();
        let foreign = CoreEngineHandle::new("not-claude", 42u32);
        let err = engine.teardown(foreign).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("foreign"),
            "expected foreign mention, got {msg}"
        );
    }
}
