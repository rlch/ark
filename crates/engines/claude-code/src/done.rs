//! Done detection for the Claude Code engine (cavekit-engine-claude-code R4).
//!
//! The Claude Code hooks `Stop` and `SessionEnd` are authoritative signals
//! that an agent has finished its turn. Hook payloads land in the sidecar's
//! per-agent JSONL file via `ark-hook` (T-048); a hook-pipeline consumer
//! (T-050/T-054) translates those into [`DoneSignal`] values and feeds them
//! into [`done_watcher`], which broadcasts a single
//! `AgentEvent::Done { outcome: Outcome::Success { artifacts: vec![] } }` to
//! the shared event bus.
//!
//! ## Orchestrator short-circuit
//!
//! The engine emits `Done` to the bus only — it does not signal the
//! supervisor directly. Orchestrators that wish to delay or upgrade the
//! outcome (e.g. cavekit may want to wait for review-pass before declaring
//! success) can subscribe to the bus *before* the engine emits and then
//! propagate or replace the event downstream.
//!
//! ## Dedupe semantics
//!
//! Each `done_watcher` task emits at most one `Done` per lifetime; subsequent
//! `DoneSignal` values are dropped silently. A new run requires a new
//! watcher.

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, EventSink, Outcome};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Hook signals that mean "the agent is done".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneSignal {
    /// Claude Code's `Stop` hook fired. Indicates the assistant's turn ended
    /// (which usually means the run has finished but, in CLI usage, can
    /// repeat per turn).
    Stop,
    /// Claude Code's `SessionEnd` hook fired — the CLI is exiting.
    SessionEnd,
}

/// Watch for [`DoneSignal`] values and broadcast a single
/// `AgentEvent::Done { outcome: Success { artifacts: [] } }` on the first one.
///
/// Returns `Ok(())` when the cancel token fires, when the input channel
/// closes, or after a `Done` has been emitted and acknowledged. Subsequent
/// signals are dropped.
pub async fn done_watcher(
    mut rx: mpsc::Receiver<DoneSignal>,
    tx: EventSink,
    id: AgentId,
    cancel: CancellationToken,
) -> Result<()> {
    let mut emitted = false;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            maybe = rx.recv() => {
                let Some(signal) = maybe else {
                    // Sender dropped — clean exit.
                    return Ok(());
                };
                if emitted {
                    tracing::debug!(?signal, "done_watcher: already emitted, ignoring");
                    continue;
                }
                tracing::debug!(?signal, "done_watcher: emitting Done Success");
                let ev = AgentEvent::Done {
                    id: id.clone(),
                    outcome: Outcome::Success { artifacts: Vec::new() },
                };
                // Bus may be closed during teardown — ignore SendError.
                let _ = tx.send(ev);
                emitted = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, channel};
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "test")
    }

    async fn next_event(
        rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
    ) -> Option<AgentEvent> {
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .and_then(|r| r.ok())
    }

    #[tokio::test]
    async fn stop_emits_one_done_success() {
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, mut rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        sig_tx.send(DoneSignal::Stop).await.unwrap();
        let ev = next_event(&mut rx).await.expect("done event");
        match ev {
            AgentEvent::Done {
                outcome: Outcome::Success { artifacts },
                ..
            } => {
                assert!(artifacts.is_empty());
            }
            other => panic!("expected Done Success, got {other:?}"),
        }
        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn session_end_emits_one_done_success() {
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, mut rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        sig_tx.send(DoneSignal::SessionEnd).await.unwrap();
        let ev = next_event(&mut rx).await.expect("done event");
        assert!(matches!(
            ev,
            AgentEvent::Done {
                outcome: Outcome::Success { .. },
                ..
            }
        ));
        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stop_then_session_end_dedupes() {
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, mut rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        sig_tx.send(DoneSignal::Stop).await.unwrap();
        sig_tx.send(DoneSignal::SessionEnd).await.unwrap();

        let first = next_event(&mut rx).await.expect("first done");
        assert!(matches!(first, AgentEvent::Done { .. }));
        // No second Done.
        let second = next_event(&mut rx).await;
        assert!(second.is_none(), "expected dedupe, got {second:?}");

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancel_returns_ok_promptly() {
        let (_sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, _rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("watcher exits promptly")
            .unwrap();
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn channel_closed_returns_ok() {
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, _rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel));

        drop(sig_tx);
        let res = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("watcher exits on closed channel")
            .unwrap();
        assert!(res.is_ok());
    }

    // -----------------------------------------------------------------
    // T-120: stronger dedupe coverage. Existing
    // `stop_then_session_end_dedupes` only exercises two signals; the
    // spec requires "multiple markers in sequence — only first triggers".
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn three_stop_signals_emit_only_one_done() {
        let (sig_tx, sig_rx) = mpsc::channel(8);
        let (tx, mut rx) = channel(16);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        // Fire the same Stop signal three times in quick succession.
        sig_tx.send(DoneSignal::Stop).await.unwrap();
        sig_tx.send(DoneSignal::Stop).await.unwrap();
        sig_tx.send(DoneSignal::Stop).await.unwrap();

        // First Done lands on the bus.
        let first = next_event(&mut rx).await.expect("first done");
        assert!(matches!(first, AgentEvent::Done { .. }));

        // Drain for a generous window and assert no further Done landed.
        let mut extras = 0usize;
        while let Some(ev) = next_event(&mut rx).await {
            if matches!(ev, AgentEvent::Done { .. }) {
                extras += 1;
                if extras > 3 {
                    break;
                }
            }
        }
        assert_eq!(extras, 0, "repeated Stop signals must dedupe to one Done");

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn session_end_then_stop_dedupes() {
        // Mirror of `stop_then_session_end_dedupes` but with the ordering
        // flipped — guards against any accidental dependence on which
        // signal arrives first.
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, mut rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        sig_tx.send(DoneSignal::SessionEnd).await.unwrap();
        sig_tx.send(DoneSignal::Stop).await.unwrap();

        let first = next_event(&mut rx).await.expect("first done");
        assert!(matches!(first, AgentEvent::Done { .. }));
        let second = next_event(&mut rx).await;
        assert!(
            second.is_none(),
            "expected dedupe with SessionEnd-first ordering, got {second:?}"
        );

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn signal_after_emit_is_silently_dropped() {
        // After the watcher has emitted, additional signals arriving
        // later (not immediately back-to-back) must still be dropped.
        let (sig_tx, sig_rx) = mpsc::channel(4);
        let (tx, mut rx) = channel(8);
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let handle = tokio::spawn(done_watcher(sig_rx, tx, id, cancel2));

        sig_tx.send(DoneSignal::Stop).await.unwrap();
        let first = next_event(&mut rx).await.expect("first done");
        assert!(matches!(first, AgentEvent::Done { .. }));

        // Introduce a gap, then send another signal. Must NOT re-emit.
        tokio::time::sleep(Duration::from_millis(50)).await;
        sig_tx.send(DoneSignal::SessionEnd).await.unwrap();
        let second = next_event(&mut rx).await;
        assert!(
            second.is_none(),
            "post-emit signal must be dropped, got {second:?}"
        );

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }
}
