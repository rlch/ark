//! `status_pipe` consumer task.
//!
//! Implements cavekit-supervisor.md R2 (second bullet) + cavekit-mux-zellij.md R4.
//!
//! - Subscribes to the supervisor's broadcast bus.
//! - Filters to a fixed whitelist of progress-relevant events; everything
//!   else is dropped (debug-logged).
//! - For each accepted event: serializes to JSON and calls
//!   `mux.pipe("ark-status", json)` and `mux.pipe("ark-picker", json)`.
//! - On pipe error (plugin absent), falls back to `mux.rename_tab` with a
//!   short status string (e.g. `"[tool:Edit]"`, `"[done]"`). The rename
//!   target tab is selected by event-derived `TabHandle` when the event
//!   carries one (`TabOpened`, `TabClosed`); otherwise the fallback is
//!   skipped silently — best-effort by spec.
//! - Lagged: warn + continue. Closed/Cancel: `Ok(())`.

use std::sync::Arc;

use anyhow::Result;
use ark_types::{AgentEvent, Outcome};
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::multiplexer::Multiplexer;

use super::event_kind_slug;

/// Long-running consumer task. See module docs.
pub async fn status_pipe<M: Multiplexer + ?Sized>(
    mut rx: Receiver<AgentEvent>,
    mux: Arc<M>,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("status_pipe: cancel fired, exiting");
                return Ok(());
            }
            recv = rx.recv() => match recv {
                Ok(event) => handle_event(&event, mux.as_ref()).await,
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "status_pipe: broadcast lagged; continuing");
                    continue;
                }
                Err(RecvError::Closed) => {
                    debug!("status_pipe: broadcast closed, exiting");
                    return Ok(());
                }
            }
        }
    }
}

/// Process one event: filter, serialize, push to both pipes; fall back to
/// rename if a pipe call errors.
async fn handle_event<M: Multiplexer + ?Sized>(event: &AgentEvent, mux: &M) {
    if !is_progress_relevant(event) {
        debug!(
            kind = event_kind_slug(event),
            "status_pipe: dropping non-progress event"
        );
        return;
    }
    let payload = match serde_json::to_string(event) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "status_pipe: serialize failed");
            return;
        }
    };

    for target in ["ark-status", "ark-picker"] {
        if let Err(e) = mux.pipe(target, &payload).await {
            warn!(target = target, error = %e, "status_pipe: pipe failed; falling back to rename_tab");
            fallback_rename(event, mux).await;
            // Once we've fallen back for this event, we don't try the
            // sibling pipe — the same plugin set is typically missing for
            // both, and rename has already happened.
            return;
        }
    }
}

/// Whitelist of events whose JSON should land in `ark-status` / `ark-picker`.
/// Aligned with cavekit-supervisor.md R2 second-bullet + acceptance criteria
/// in T-060.
fn is_progress_relevant(event: &AgentEvent) -> bool {
    use AgentEvent::*;
    matches!(
        event,
        Started { .. }
            | PhaseTransition { .. }
            | ToolUse { .. }
            | Message { .. }
            | FileEdited { .. }
            | Stall { .. }
            | Progress { .. }
            | Done { .. }
            | ReviewComment { .. }
            | TabOpened { .. }
            | TabClosed { .. }
    )
}

/// Best-effort rename-tab fallback when pipes are unavailable.
///
/// Rename target: events that carry a `TabHandle` directly (`TabOpened`,
/// `TabClosed`) are renamed in-place. For other events we cannot guess a
/// handle without supervisor-level state — the fallback degrades to a debug
/// log so we never spuriously rename random tabs.
async fn fallback_rename<M: Multiplexer + ?Sized>(event: &AgentEvent, mux: &M) {
    let label = short_label(event);
    let handle = match event {
        AgentEvent::TabOpened { tab_handle, .. } | AgentEvent::TabClosed { tab_handle, .. } => {
            Some(tab_handle.clone())
        }
        _ => None,
    };
    let Some(handle) = handle else {
        debug!(
            kind = event_kind_slug(event),
            label = label,
            "status_pipe: rename fallback skipped (no tab handle available)"
        );
        return;
    };
    if let Err(e) = mux.rename_tab(&handle, &label).await {
        debug!(error = %e, "status_pipe: rename_tab fallback also failed; continuing");
    }
}

/// Short bracketed status string for `rename_tab` fallback.
fn short_label(event: &AgentEvent) -> String {
    use AgentEvent::*;
    match event {
        Started { .. } => "[started]".into(),
        PhaseTransition { to, .. } => format!("[phase:{to}]"),
        ToolUse { tool, .. } => format!("[tool:{tool}]"),
        Message { .. } => "[msg]".into(),
        FileEdited { .. } => "[edit]".into(),
        Stall { .. } => "[stall]".into(),
        Progress { done, total, .. } => format!("[{done}/{total}]"),
        Done { outcome, .. } => match outcome {
            Outcome::Success { .. } => "[done]".into(),
            Outcome::Failed { .. } => "[fail]".into(),
            Outcome::Killed => "[killed]".into(),
            Outcome::Timeout => "[timeout]".into(),
            Outcome::Crashed { .. } => "[crashed]".into(),
        },
        ReviewComment { severity, .. } => format!("[{severity:?}]"),
        TabOpened { label, .. } => format!("[+{label}]"),
        TabClosed { tab_handle, .. } => format!("[-{}]", tab_handle.name),
        _ => "[event]".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{
        AgentEvent, AgentId, AgentSpec, MessageRole, Phase, TabHandle, TabRole, channel,
    };
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Configurable mock multiplexer: pipe can be made to fail, and all
    /// calls are recorded.
    #[derive(Default)]
    struct MockMux {
        calls: Mutex<Vec<String>>,
        pipe_fails: bool,
        rename_fails: bool,
    }

    impl MockMux {
        fn record(&self, s: impl Into<String>) {
            self.calls.lock().unwrap().push(s.into());
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Multiplexer for MockMux {
        fn kind(&self) -> &'static str {
            "mock"
        }
        async fn ensure_session(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            _: &Path,
        ) -> anyhow::Result<TabHandle> {
            Ok(TabHandle::new(session, 1, name))
        }
        async fn close_tab(&self, _: &TabHandle) -> anyhow::Result<()> {
            Ok(())
        }
        async fn rename_tab(&self, h: &TabHandle, name: &str) -> anyhow::Result<()> {
            self.record(format!("rename:{}->{name}", h.name));
            if self.rename_fails {
                anyhow::bail!("rename intentionally fails");
            }
            Ok(())
        }
        async fn pipe(&self, target: &str, payload: &str) -> anyhow::Result<()> {
            self.record(format!("pipe:{target}:{}", payload.len()));
            if self.pipe_fails {
                anyhow::bail!("pipe intentionally fails");
            }
            Ok(())
        }
    }

    fn id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    fn sample_spec() -> AgentSpec {
        let mut s = AgentSpec::new(
            id(),
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/wt"),
            vec!["claude".into()],
        );
        s.env = BTreeMap::new();
        s
    }

    #[tokio::test]
    async fn happy_path_pipes_to_both_targets() {
        let mux = Arc::new(MockMux::default());
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        tx.send(AgentEvent::ToolUse {
            id: id(),
            tool: "Edit".into(),
            input_summary: "x".into(),
        })
        .unwrap();

        // Wait for both pipe calls.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if mux.calls().len() >= 2 {
                break;
            }
        }
        let calls = mux.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("pipe:ark-status:")),
            "expected ark-status pipe, got {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("pipe:ark-picker:")),
            "expected ark-picker pipe, got {calls:?}"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fallback_rename_when_pipe_fails() {
        let mux = Arc::new(MockMux {
            pipe_fails: true,
            ..MockMux::default()
        });
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        // Use TabOpened — it carries a TabHandle so the fallback can act.
        tx.send(AgentEvent::TabOpened {
            id: id(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: TabHandle::new("ark-cavekit-auth", 1, "builder"),
            label: "main".into(),
        })
        .unwrap();

        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if mux.calls().iter().any(|c| c.starts_with("rename:")) {
                break;
            }
        }

        let calls = mux.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("pipe:ark-status:")),
            "first pipe attempt should still happen: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("rename:builder->")),
            "fallback rename should fire: {calls:?}"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn non_progress_events_dropped() {
        let mux = Arc::new(MockMux::default());
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        // Log + Iteration are NOT in the whitelist.
        tx.send(AgentEvent::Log {
            id: id(),
            level: ark_types::LogLevel::Debug,
            line: "noise".into(),
        })
        .unwrap();
        tx.send(AgentEvent::Iteration {
            id: id(),
            n: 1,
            max: None,
        })
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(
            mux.calls().is_empty(),
            "expected no pipe calls, got {:?}",
            mux.calls()
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn lagged_warn_and_survives() {
        let mux = Arc::new(MockMux::default());
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(4);

        // Pre-flood beyond capacity so first recv yields Lagged.
        for _ in 0..50 {
            tx.send(AgentEvent::Started {
                spec: sample_spec(),
            })
            .unwrap();
        }

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        // Send another after lag report.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tx.send(AgentEvent::PhaseTransition {
            id: id(),
            from: Some("starting".into()),
            to: "running".into(),
        })
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        cancel.cancel();
        let res = task.await.unwrap();
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let mux = Arc::new(MockMux::default());
        let cancel = CancellationToken::new();
        let (_tx, rx) = channel(8);
        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });
        cancel.cancel();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("status_pipe didn't return on cancel");
        assert!(res.unwrap().is_ok());
    }

    #[tokio::test]
    async fn closed_returns_ok() {
        let mux = Arc::new(MockMux::default());
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(8);
        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });
        drop(tx);
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("status_pipe didn't return on Closed");
        assert!(res.unwrap().is_ok());

        // Use the imported Phase / MessageRole symbols so they don't lint
        // out unused — keeps the test file self-documenting for future
        // maintainers.
        let _ = Phase::Done;
        let _ = MessageRole::User;
    }
}
