//! Engine trait — abstract interface over an underlying agent CLI.
//!
//! Implements cavekit-architecture.md R1. The engine extracts structured signal
//! (hooks, transcripts, permissions) from a CLI like Claude Code, and emits
//! events into the shared supervisor event bus.
//!
//! A single engine instance serves multiple panes; each call to
//! `install_observability` returns a per-pane `EngineHandle` the supervisor
//! stores for teardown.

use std::any::Any;
use std::path::{Path, PathBuf};

use ark_types::EventSink;
use async_trait::async_trait;

/// Policy for auto-approving engine permission prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Always ask (default).
    Ask,
    /// Auto-approve read-only tool invocations only.
    AutoApproveRead,
    /// Auto-approve everything (DANGEROUS; for non-interactive runs).
    AutoApproveAll,
}

/// Opaque per-pane handle returned by `install_observability`.
///
/// Each engine stashes its own state (settings backup path, joinset,
/// tailer task) behind `Box<dyn Any + Send + Sync>`. The supervisor only
/// needs to round-trip it back to `teardown`. Fields are private so callers
/// cannot bypass the abstraction; accessors expose the engine name and
/// `downcast` consumes the handle to reclaim the state.
pub struct EngineHandle {
    engine_name: &'static str,
    state: Box<dyn Any + Send + Sync>,
}

impl EngineHandle {
    /// Wrap engine-specific state for storage.
    pub fn new<S: Any + Send + Sync>(engine_name: &'static str, state: S) -> Self {
        Self {
            engine_name,
            state: Box::new(state),
        }
    }

    /// Slug of the engine that minted this handle.
    pub fn engine_name(&self) -> &'static str {
        self.engine_name
    }

    /// Attempt to reclaim the engine-specific state as `S`. Returns the
    /// handle unchanged on type mismatch so callers can fall back.
    pub fn downcast<S: Any + Send + Sync>(self) -> Result<Box<S>, Self> {
        let Self { engine_name, state } = self;
        match state.downcast::<S>() {
            Ok(s) => Ok(s),
            Err(state) => Err(Self { engine_name, state }),
        }
    }
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("engine_name", &self.engine_name)
            .field("state", &"<opaque>")
            .finish()
    }
}

/// Abstract engine interface. See cavekit-architecture.md R1.
#[async_trait]
pub trait Engine: Send + Sync + 'static {
    /// Stable slug identifying this engine (e.g. `"claude-code"`).
    fn name(&self) -> &'static str;

    /// Install per-pane hooks, transcript watchers, etc. Must be idempotent —
    /// safe to invoke twice on the same `cwd`. Runs before the orchestrator
    /// launches the agent process so no early events are lost.
    async fn install_observability(
        &self,
        cwd: &Path,
        sink: EventSink,
    ) -> anyhow::Result<EngineHandle>;

    /// Tear down whatever `install_observability` set up. Must accept a
    /// handle minted by this engine; may error on foreign handles.
    async fn teardown(&self, handle: EngineHandle) -> anyhow::Result<()>;

    /// The argv to use when launching a pane for this engine's primary
    /// agent process. The orchestrator (or KDL layout) runs this.
    fn default_pane_cmd(&self) -> Vec<String>;

    /// Path to the engine's transcript/log file, if one exists under `cwd`.
    fn transcript_path(&self, cwd: &Path) -> Option<PathBuf>;

    /// Configure the engine's permission-prompt behavior for this `cwd`.
    async fn auto_approve_permissions(
        &self,
        cwd: &Path,
        policy: ApprovalPolicy,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEngine;

    struct MockState {
        installed_at: PathBuf,
    }

    #[async_trait]
    impl Engine for MockEngine {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn install_observability(
            &self,
            cwd: &Path,
            _sink: EventSink,
        ) -> anyhow::Result<EngineHandle> {
            Ok(EngineHandle::new(
                "mock",
                MockState {
                    installed_at: cwd.to_path_buf(),
                },
            ))
        }

        async fn teardown(&self, _handle: EngineHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn default_pane_cmd(&self) -> Vec<String> {
            vec!["mock-agent".to_string(), "--stdin".to_string()]
        }

        fn transcript_path(&self, _cwd: &Path) -> Option<PathBuf> {
            None
        }

        async fn auto_approve_permissions(
            &self,
            _cwd: &Path,
            _policy: ApprovalPolicy,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn mock_engine_trait_object_dispatch() {
        let engine: Box<dyn Engine> = Box::new(MockEngine);
        assert_eq!(engine.name(), "mock");
        assert_eq!(
            engine.default_pane_cmd(),
            vec!["mock-agent".to_string(), "--stdin".to_string()]
        );
        assert!(engine.transcript_path(Path::new("/tmp")).is_none());

        let (sink, _rx) = tokio::sync::broadcast::channel::<ark_types::AgentEvent>(8);
        let handle = engine
            .install_observability(Path::new("/tmp/cwd"), sink)
            .await
            .unwrap();
        assert_eq!(handle.engine_name(), "mock");

        // Downcast round-trip.
        let state = handle.downcast::<MockState>().expect("downcast mock state");
        assert_eq!(state.installed_at, PathBuf::from("/tmp/cwd"));
    }

    #[test]
    fn engine_handle_downcast_mismatch_returns_self() {
        let handle = EngineHandle::new("mock", 42u32);
        let err = handle.downcast::<String>().unwrap_err();
        assert_eq!(err.engine_name(), "mock");
    }

    #[test]
    fn approval_policy_is_copy_eq() {
        let a = ApprovalPolicy::AutoApproveRead;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(ApprovalPolicy::Ask, ApprovalPolicy::AutoApproveAll);
    }
}
