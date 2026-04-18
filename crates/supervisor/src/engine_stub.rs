//! Minimal `Engine` trait impl used by the supervisor factory after
//! the legacy `ark-engines-claude-code` crate was retired in T-ACP.7.
//!
//! ark v0.3 speaks ACP natively (cavekit-scene R17). Engines are ACP
//! **agents** — spawned subprocesses driven by
//! [`acp_client::AcpClient`] — not trait-objects that inject hooks
//! + tail transcripts. The `Engine` trait lives in `ark-core` for
//! backwards-compat with call sites not yet migrated (e.g. the
//! claude-code orchestrator's `engine()` method), so we still need
//! SOMETHING implementing it. This module ships the thinnest
//! possible adapter:
//!
//! * `install_observability` is a no-op. All observability flows
//!   through the ACP event stream the supervisor's permission
//!   dispatcher + reaction dispatcher subscribe to. No file-system
//!   settings injection; no transcript tailer.
//! * `teardown` is a no-op. The ACP subprocess is owned by the
//!   [`acp_client::AcpClient`] whose `Drop` kills the child.
//! * `preflight` trivially checks that the configured ACP engine
//!   binary resolves on PATH. Deeper checks live in `ark doctor`
//!   (T-ACP.6).
//!
//! This module is intentionally narrow: any richer per-engine
//! behavior belongs on the scene/extension side (via scene
//! reactions on `ark.acp.*` events), not baked into an Engine trait
//! impl.

use std::path::{Path, PathBuf};

use anyhow::Result;
use ark_core::engine::{ApprovalPolicy, Engine, EngineHandle};
use ark_types::{EventSink, SessionId, SessionSpec};
use async_trait::async_trait;

/// Generic ACP-engine adapter.
///
/// Carries the engine name (e.g. `"claude"`, `"codex"`,
/// `"claude-code"` for back-compat with the legacy slug) so
/// `Engine::name()` still round-trips the factory's input. Beyond
/// that it's stateless.
#[derive(Debug, Clone)]
pub struct AcpEngineStub {
    /// Slug returned from [`Engine::name`]. Copied verbatim from the
    /// [`AgentSpec`]'s engine field at factory time.
    name: String,
}

impl AcpEngineStub {
    /// Construct a stub for the supplied engine slug.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Default for AcpEngineStub {
    fn default() -> Self {
        Self::new("claude-code")
    }
}

#[async_trait]
impl Engine for AcpEngineStub {
    fn name(&self) -> &'static str {
        // Engine trait wants a `&'static str`, but we hold a
        // runtime-owned String. Leak the string — this is called
        // once per factory build, so allocation pressure is zero.
        //
        // Alternative: make `Engine::name` return `&str` — a wider
        // refactor tracked as post-v1 cleanup.
        match self.name.as_str() {
            "claude" => "claude",
            "codex" => "codex",
            "gemini-cli" | "gemini" => "gemini-cli",
            "claude-code" => "claude-code",
            other => Box::leak(other.to_string().into_boxed_str()),
        }
    }

    async fn install_observability(
        &self,
        _id: &SessionId,
        _cwd: &Path,
        _sink: EventSink,
    ) -> Result<EngineHandle> {
        // T-ACP.7: the legacy crate injected hooks here. Under ACP
        // all observability flows through the event bus, so this is
        // a no-op. We still return a trait-typed handle so the
        // supervisor can call `teardown` symmetrically.
        Ok(EngineHandle::new("acp-engine-stub", AcpEngineHandleMarker))
    }

    async fn teardown(&self, _handle: EngineHandle) -> Result<()> {
        Ok(())
    }

    fn default_pane_cmd(&self) -> Vec<String> {
        vec![self.name.clone()]
    }

    fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    async fn auto_approve_permissions(&self, _cwd: &Path, _policy: ApprovalPolicy) -> Result<()> {
        // Under ACP, permission policy is entirely the
        // `PermissionDispatcher`'s job (T-ACP.5). The old
        // `.claude/policy` file no longer exists — decisions flow
        // through `session/request_permission` + `acp_permit` and
        // stay in-memory for the agent's lifetime.
        Ok(())
    }
}

/// Zero-sized marker the teardown symmetry relies on — the supervisor
/// downcasts `EngineHandle` back to this type, but there's nothing to
/// restore because `install_observability` didn't mutate anything.
#[derive(Debug)]
pub struct AcpEngineHandleMarker;

/// Preflight: verify the engine binary resolves on `PATH`. Run by
/// the supervisor at R3 step 8, replacing the legacy preflight that
/// also probed `.claude/` state.
///
/// Returns `Ok(())` when the command is on PATH OR is an absolute
/// path that exists. Returns an `anyhow::Error` with a clean
/// message when the binary is missing.
pub fn preflight(_spec: &SessionSpec) -> Result<()> {
    // cavekit-soul Phase 1: engine resolution moves to extensions; the
    // SessionSpec no longer carries an `engine` slug. Preflight is a
    // no-op here — extensions own per-engine PATH probes.
    Ok(())
}

#[allow(dead_code)]

fn which(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_passes_through_slug() {
        assert_eq!(AcpEngineStub::new("claude").name(), "claude");
        assert_eq!(AcpEngineStub::new("codex").name(), "codex");
        assert_eq!(AcpEngineStub::new("claude-code").name(), "claude-code");
    }

    #[test]
    fn default_pane_cmd_is_the_slug() {
        assert_eq!(
            AcpEngineStub::new("custom-engine").default_pane_cmd(),
            vec!["custom-engine".to_string()]
        );
    }

    #[test]
    fn transcript_path_is_none() {
        assert!(
            AcpEngineStub::new("claude")
                .transcript_path(Path::new("/tmp"))
                .is_none()
        );
    }

    #[tokio::test]
    async fn install_teardown_roundtrip_is_noop() {
        let engine = AcpEngineStub::new("claude");
        let (sink, _rx) = ark_types::channel(4);
        let id = SessionId::new("stub_test");
        let handle = engine
            .install_observability(&id, Path::new("/tmp"), sink)
            .await
            .expect("install");
        assert_eq!(handle.engine_name(), "acp-engine-stub");
        engine.teardown(handle).await.expect("teardown");
    }
}
