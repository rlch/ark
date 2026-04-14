//! Per-pane engine handle for Claude Code (cavekit-engine-claude-code R6).
//!
//! The [`EngineHandle`] owns the per-pane state that the supervisor needs to
//! round-trip between `install_observability` and `teardown` (see
//! cavekit-architecture R1):
//!
//! - a [`tokio::task::JoinSet`] holding subtasks (transcript tailer, stall
//!   watcher, done watcher),
//! - the worktree path, used by teardown to restore
//!   `.claude/settings.local.json` via [`crate::settings::restore_settings`],
//! - the [`AgentId`] for routing / log context,
//! - a [`CancellationToken`] that propagates to every subtask.
//!
//! This is the CLAUDE-CODE-SPECIFIC handle. The [`ark_core::engine::EngineHandle`]
//! wrapper that the [`ark_core::engine::Engine`] trait returns wraps this
//! struct via `EngineHandle::new("claude-code", state)` at the supervisor
//! layer (Tier 3 wiring). Callers there reclaim it via `handle.downcast::<
//! ark_engines_claude_code::EngineHandle>()` before calling [`EngineHandle::
//! teardown`].
//!
//! ## Teardown contract
//!
//! [`EngineHandle::teardown`] guarantees:
//!
//! 1. the cancellation token is fired — every subtask observes the cancel,
//! 2. all subtasks are awaited to completion (failures are logged, never
//!    propagated — teardown is best-effort cleanup),
//! 3. `settings::restore_settings(&cwd)` is called, restoring the pre-install
//!    settings file and removing the `.ark-backup` companion,
//! 4. any restore error is logged and returned (but subtasks are always
//!    awaited first, so we don't leak tasks on a restore failure).
//!
//! Teardown is idempotent in the same sense [`restore_settings`] is:
//! restoring when no injection occurred is a harmless no-op.

use std::future::Future;
use std::path::PathBuf;

use anyhow::Result;
use ark_types::AgentId;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::settings;

/// Per-pane Claude Code engine state. Constructed by `install_observability`,
/// consumed by [`EngineHandle::teardown`].
///
/// Fields are `pub(crate)` so internal construction is unrestricted while
/// callers outside the crate treat the handle as opaque.
pub struct EngineHandle {
    pub(crate) joinset: JoinSet<Result<()>>,
    pub(crate) cwd: PathBuf,
    pub(crate) agent_id: AgentId,
    pub(crate) teardown_token: CancellationToken,
}

impl EngineHandle {
    /// Construct a fresh handle for a worktree + agent. The caller is
    /// expected to populate the [`JoinSet`] via [`EngineHandle::spawn`] for
    /// the subtasks it wants teardown to cancel & await.
    pub fn new(cwd: PathBuf, agent_id: AgentId) -> Self {
        Self {
            joinset: JoinSet::new(),
            cwd,
            agent_id,
            teardown_token: CancellationToken::new(),
        }
    }

    /// Worktree path this handle restores on teardown.
    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    /// Agent id this handle was minted for.
    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// Child cancellation token shared with subtasks. Clone freely.
    pub fn token(&self) -> CancellationToken {
        self.teardown_token.clone()
    }

    /// Attach a subtask to this handle's [`JoinSet`]. The returned join
    /// handle is ignored; subtasks must observe [`EngineHandle::token`]
    /// for cancellation.
    pub fn spawn<F>(&mut self, task: F)
    where
        F: Future<Output = Result<()>> + Send + 'static,
    {
        self.joinset.spawn(task);
    }

    /// Tear down:
    ///
    /// 1. cancel the shared token,
    /// 2. drain the [`JoinSet`] (logging subtask errors),
    /// 3. call [`settings::restore_settings`] on the worktree.
    ///
    /// Always awaits subtasks even if restore fails; errors from restore
    /// are surfaced, panics/errors from subtasks are logged but do not
    /// abort the teardown path.
    pub async fn teardown(self) -> Result<()> {
        let Self {
            mut joinset,
            cwd,
            agent_id,
            teardown_token,
        } = self;
        tracing::debug!(cwd = %cwd.display(), agent = %agent_id.as_str(), "engine handle teardown: cancelling");
        teardown_token.cancel();

        while let Some(joined) = joinset.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "engine subtask returned Err during teardown");
                }
                Err(join_err) if join_err.is_panic() => {
                    tracing::warn!(error = %join_err, "engine subtask panicked during teardown");
                }
                Err(join_err) => {
                    tracing::warn!(error = %join_err, "engine subtask join error during teardown");
                }
            }
        }

        if let Err(e) = settings::restore_settings(&cwd) {
            tracing::warn!(error = %e, cwd = %cwd.display(), "settings::restore_settings failed");
            return Err(anyhow::Error::new(e));
        }

        Ok(())
    }
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("cwd", &self.cwd)
            .field("agent_id", &self.agent_id)
            .field("teardown_token", &"<token>")
            .field("joinset_len", &self.joinset.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{inject_hooks, restore_settings};
    use ark_types::AgentId;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn sample_id() -> AgentId {
        AgentId::parse("cavekit-auth-01jx7z8k6x9y2zt4abcdef0123").expect("parse fixture")
    }

    const EVENTS: &[&str] = &["Stop", "SessionEnd"];

    #[tokio::test]
    async fn new_gives_usable_token_and_cwd() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let h = EngineHandle::new(cwd.clone(), sample_id());
        assert_eq!(h.cwd(), &cwd);
        assert_eq!(h.agent_id(), &sample_id());
        let tok = h.token();
        assert!(!tok.is_cancelled());
    }

    #[tokio::test]
    async fn spawn_plus_teardown_cancels_subtask() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let mut h = EngineHandle::new(cwd, sample_id());
        let tok = h.token();

        // Subtask: loops until cancelled.
        h.spawn(async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        });

        // Teardown should cancel + await promptly.
        tokio::time::timeout(Duration::from_secs(2), h.teardown())
            .await
            .expect("teardown prompt")
            .expect("teardown ok");
    }

    #[tokio::test]
    async fn teardown_restores_injected_settings() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let original = br#"{"permissions":{"allow":["Read"]}}"#.to_vec();
        fs::write(claude_dir.join("settings.local.json"), &original).unwrap();

        let id = sample_id();
        inject_hooks(&cwd, &id, EVENTS, "0.1.0").expect("inject");

        // After inject, the live file is NOT the original (it has ark hooks).
        let after_inject = fs::read(claude_dir.join("settings.local.json")).unwrap();
        assert_ne!(after_inject, original, "inject should have rewritten");
        assert!(
            claude_dir.join("settings.local.json.ark-backup").exists(),
            "backup must exist before teardown",
        );

        let h = EngineHandle::new(cwd.clone(), id);
        h.teardown().await.expect("teardown ok");

        // Original restored; backup removed.
        let restored = fs::read(claude_dir.join("settings.local.json")).unwrap();
        assert_eq!(restored, original);
        assert!(
            !claude_dir.join("settings.local.json.ark-backup").exists(),
            "backup must be removed on teardown",
        );
    }

    #[tokio::test]
    async fn teardown_without_injection_is_ok() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        // No .claude/ at all.
        let h = EngineHandle::new(cwd, sample_id());
        h.teardown().await.expect("idempotent teardown ok");
    }

    #[tokio::test]
    async fn teardown_with_failing_subtask_still_restores() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let original = br#"{"hooks":{"SomeOther":[{"command":"x"}]}}"#.to_vec();
        fs::write(claude_dir.join("settings.local.json"), &original).unwrap();

        let id = sample_id();
        inject_hooks(&cwd, &id, EVENTS, "0.1.0").expect("inject");

        let mut h = EngineHandle::new(cwd.clone(), id);
        // One subtask returns Err, one panics, one exits Ok.
        h.spawn(async { Err(anyhow::anyhow!("boom")) });
        h.spawn(async {
            panic!("intentional panic for test");
        });
        h.spawn(async { Ok(()) });

        // Restore still succeeds despite subtask failures.
        h.teardown().await.expect("teardown ok");

        let restored = fs::read(claude_dir.join("settings.local.json")).unwrap();
        assert_eq!(restored, original);
    }

    #[tokio::test]
    async fn teardown_is_idempotent_via_restore_settings() {
        // Run restore twice via a handle sequence: inject → teardown →
        // construct a second handle → teardown again → still Ok (no files
        // exist to restore, which is the noop path).
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().to_path_buf();
        let id = sample_id();

        inject_hooks(&cwd, &id, EVENTS, "0.1.0").expect("inject");
        EngineHandle::new(cwd.clone(), id.clone())
            .teardown()
            .await
            .expect("first teardown");
        // Second teardown on same cwd: should be a noop.
        EngineHandle::new(cwd.clone(), id)
            .teardown()
            .await
            .expect("second teardown noop");

        // And restore_settings directly is still a noop now.
        restore_settings(&cwd).expect("noop");
    }
}
