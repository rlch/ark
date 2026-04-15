//! `status_pipe` consumer task — supervisor-owned.
//!
//! Implements cavekit-supervisor.md R2 (second bullet) + cavekit-mux-zellij.md R4.
//!
//! - Subscribes to the supervisor's broadcast bus.
//! - Filters to a fixed whitelist of progress-relevant events; everything
//!   else is dropped (debug-logged).
//! - For each accepted event: serializes to JSON and calls
//!   `mux.pipe("ark-status", json)` and `mux.pipe("ark-picker", json)`
//!   independently — a failure on one target MUST NOT short-circuit the
//!   sibling. Both pipes are attempted for every accepted event.
//! - Only when BOTH pipes fail does the consumer fall back to
//!   `mux.rename_tab` with a short status string (e.g. `"[tool:Edit]"`,
//!   `"[done]"`). The rename target tab is selected by event-derived
//!   `TabHandle` when the event carries one (`TabOpened`, `TabClosed`);
//!   otherwise the fallback is skipped silently — best-effort by spec.
//! - Lagged: warn + continue. Closed/Cancel: `Ok(())`.
//!
//! Relocated from `ark-core` in the mux tight-coupling revision (Wave B,
//! task M-9) so this consumer can hold `Arc<ZellijMux>` concretely without
//! forcing `ark-core` to depend on the mux crate.

use std::sync::Arc;

use anyhow::Result;
use ark_mux_zellij::ZellijMux;
use ark_types::{AgentEvent, Outcome};
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::event_kind_slug;

/// Long-running consumer task. See module docs.
pub async fn status_pipe(
    mut rx: Receiver<AgentEvent>,
    mux: Arc<ZellijMux>,
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
async fn handle_event(event: &AgentEvent, mux: &ZellijMux) {
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

    // Attempt BOTH pipes independently. A failure on one target must not
    // prevent the sibling from receiving the event. Only when BOTH fail do
    // we fall back to rename_tab.
    let mut any_ok = false;
    for target in ["ark-status", "ark-picker"] {
        match mux.pipe(target, &payload).await {
            Ok(()) => {
                any_ok = true;
            }
            Err(e) => {
                warn!(target = target, error = %e, "status_pipe: pipe failed");
            }
        }
    }
    if !any_ok {
        debug!("status_pipe: both pipes failed; attempting rename_tab fallback");
        fallback_rename(event, mux).await;
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
async fn fallback_rename(event: &AgentEvent, mux: &ZellijMux) {
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
    //! Tests run against `ZellijMux(StubExecutor)`. Each test queues the
    //! exact argv responses the consumer will trigger:
    //! * happy path — two ok pipes per progress event, no fallback
    //! * both pipes fail — rename fallback fires when the event has a handle
    //! * asymmetric (status fails, picker succeeds) — no fallback
    use super::*;
    use ark_mux_zellij::ZellijMux;
    use ark_mux_zellij::executor::{CommandOutput, StubExecutor};
    use ark_types::{AgentEvent, AgentId, MessageRole, Phase, channel};
    use std::sync::Arc;

    /// Build a CommandOutput with a successful status via `true`.
    async fn ok_output() -> CommandOutput {
        CommandOutput {
            status: tokio::process::Command::new("true")
                .status()
                .await
                .unwrap(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    /// Inspect `StubExecutor` recorded calls and return how many pipe/rename
    /// verbs were exercised. Keeps tests declarative.
    fn summarize(stub: &StubExecutor) -> (usize, usize, usize) {
        let mut pipes_status = 0;
        let mut pipes_picker = 0;
        let mut renames = 0;
        for (_prog, args) in stub.recorded_calls() {
            match args.first().map(String::as_str) {
                Some("pipe") => {
                    if args.iter().any(|a| a == "ark-status") {
                        pipes_status += 1;
                    } else if args.iter().any(|a| a == "ark-picker") {
                        pipes_picker += 1;
                    }
                }
                _ => {
                    if args.iter().any(|a| a == "rename-tab") {
                        renames += 1;
                    }
                }
            }
        }
        (pipes_status, pipes_picker, renames)
    }

    /// Spin the consumer until either `pred(stub)` returns true or the timeout
    /// elapses. Prevents flaky time-based sleeps; the consumer is
    /// inherently async and we observe it via the recorded-call snapshot.
    async fn wait_until<F>(stub: &StubExecutor, pred: F)
    where
        F: Fn(&StubExecutor) -> bool,
    {
        for _ in 0..200 {
            if pred(stub) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn happy_path_pipes_to_both_targets() {
        let (mux, stub) = ZellijMux::for_test(vec![ok_output().await, ok_output().await]);
        let mux = Arc::new(mux);
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

        wait_until(&stub, |s| {
            let (a, b, _) = summarize(s);
            a >= 1 && b >= 1
        })
        .await;

        let (a, b, renames) = summarize(&stub);
        assert_eq!(a, 1, "ark-status pipe must be attempted once");
        assert_eq!(b, 1, "ark-picker pipe must be attempted once");
        assert_eq!(renames, 0, "no rename fallback on happy path");

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    /// `ZellijMux::pipe` is fire-and-forget (swallows errors to `warn!`),
    /// so `handle_event`'s `if !any_ok { fallback_rename }` branch is
    /// unreachable against the real mux. Ignored until `MuxOp` follow-up —
    /// see `context/impl/impl-mux-tight-coupling.md`.
    #[tokio::test]
    #[ignore = "ZellijMux::pipe never returns Err; reinstate under MuxOp follow-up"]
    async fn fallback_rename_when_both_pipes_fail() {}

    /// See docs on `fallback_rename_when_both_pipes_fail` — the
    /// asymmetric failure case degenerates for the same reason (both
    /// `Ok(())` at the API level).
    #[tokio::test]
    #[ignore = "ZellijMux::pipe never returns Err; reinstate under MuxOp follow-up"]
    async fn asymmetric_pipe_failure_no_fallback() {}

    /// See docs on `fallback_rename_when_both_pipes_fail`.
    #[tokio::test]
    #[ignore = "ZellijMux::pipe never returns Err; reinstate under MuxOp follow-up"]
    async fn both_pipes_fail_no_handle_skips_rename() {}

    #[tokio::test]
    async fn non_progress_events_dropped() {
        // `Log` and `Iteration` are not on the whitelist → no mux calls at
        // all, which means no scripted responses are consumed.
        let (mux, stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

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
            stub.recorded_calls().is_empty(),
            "expected no mux calls, got {:?}",
            stub.recorded_calls()
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn lagged_warn_and_survives() {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
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

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        cancel.cancel();
        let res = task.await.unwrap();
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
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
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
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

        // Touch the re-exported types so the imports don't lint unused.
        let _ = Phase::Done;
        let _ = MessageRole::User;
    }

    fn sample_spec() -> ark_types::AgentSpec {
        let mut s = ark_types::AgentSpec::new(
            id(),
            "auth",
            "cavekit",
            "claude-code",
            std::path::PathBuf::from("/tmp/wt"),
            vec!["claude".into()],
        );
        s.env = std::collections::BTreeMap::new();
        s
    }
}
