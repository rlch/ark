//! `status_pipe` consumer task — supervisor-owned.
//!
//! Subscribes to the supervisor's broadcast bus and forwards every
//! progress-relevant [`ark_types::CoreEvent`] to the `ark-status` and
//! `ark-picker` mux pipes. Failures on either pipe are warn-logged but do
//! not short-circuit the sibling pipe.
//!
//! cavekit-soul Phase 1: the previous AgentEvent-keyed whitelist (Started,
//! ToolUse, PhaseTransition, etc.) collapsed with the deletion of the
//! `AgentEvent` enum. Under Phase 1 every CoreEvent variant other than
//! `Log` rides through to both pipes — `Log` is noisy by design and would
//! drown the status pane. Extensions emit their own progress signal via
//! `CoreEvent::Ext` envelopes; those flow through unchanged.

use std::sync::Arc;

use anyhow::Result;
use ark_mux_zellij::ZellijMux;
use ark_types::CoreEvent;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::event_kind_slug;

/// Long-running consumer task. See module docs.
pub async fn status_pipe(
    mut rx: Receiver<CoreEvent>,
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

/// Forward one event: filter out `Log`, serialise, push to both pipes.
async fn handle_event(event: &CoreEvent, mux: &ZellijMux) {
    if !is_progress_relevant(event) {
        debug!(
            kind = %event_kind_slug(event),
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
            warn!(target = target, error = %e, "status_pipe: pipe failed");
        }
    }
}

/// Whitelist for the status pipe. Under Phase 1 everything except the noisy
/// `Log` variant flows through.
fn is_progress_relevant(event: &CoreEvent) -> bool {
    !matches!(event, CoreEvent::Log { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_mux_zellij::ZellijMux;
    use ark_mux_zellij::executor::{CommandOutput, StubExecutor};
    use ark_types::{ExtEvent, channel};
    use chrono::Utc;
    use std::sync::Arc;

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

    fn summarize(stub: &StubExecutor) -> (usize, usize) {
        let mut pipes_status = 0;
        let mut pipes_picker = 0;
        for (_prog, args) in stub.recorded_calls() {
            if args.first().map(String::as_str) == Some("pipe") {
                if args.iter().any(|a| a == "ark-status") {
                    pipes_status += 1;
                } else if args.iter().any(|a| a == "ark-picker") {
                    pipes_picker += 1;
                }
            }
        }
        (pipes_status, pipes_picker)
    }

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

        tx.send(CoreEvent::Ext(ExtEvent {
            ext: "claude-code".to_string(),
            kind: "tool.use".to_string(),
            payload: serde_json::json!({"tool": "Edit"}),
        }))
        .unwrap();

        wait_until(&stub, |s| {
            let (a, b) = summarize(s);
            a >= 1 && b >= 1
        })
        .await;

        let (a, b) = summarize(&stub);
        assert_eq!(a, 1, "ark-status pipe must be attempted once");
        assert_eq!(b, 1, "ark-picker pipe must be attempted once");

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn log_events_are_dropped() {
        let (mux, stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        tx.send(CoreEvent::Log {
            level: "debug".into(),
            message: "noise".into(),
            target: None,
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
    async fn session_ended_flows_through() {
        let (mux, stub) = ZellijMux::for_test(vec![ok_output().await, ok_output().await]);
        let mux = Arc::new(mux);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(64);

        let task = tokio::spawn({
            let mux = mux.clone();
            let cancel = cancel.clone();
            async move { status_pipe(rx, mux, cancel).await }
        });

        tx.send(CoreEvent::SessionEnded {
            terminated_at: Utc::now(),
        })
        .unwrap();

        wait_until(&stub, |s| {
            let (a, b) = summarize(s);
            a >= 1 && b >= 1
        })
        .await;

        cancel.cancel();
        task.await.unwrap().unwrap();
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
}
