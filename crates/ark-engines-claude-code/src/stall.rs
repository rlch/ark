//! Stall detection for the Claude Code engine (cavekit-engine-claude-code R5).
//!
//! Subscribes to the shared event bus and tracks the time of the last
//! `ToolUse` or `Message` event for a given [`AgentId`]. Every 10 seconds it
//! checks whether the agent has been silent longer than the configured
//! threshold. If so, it emits a single [`AgentEvent::Stall`] onto the bus;
//! further silence does not re-emit. When activity resumes (a new
//! `ToolUse` / `Message` arrives after a stall was emitted) it emits an
//! `AgentEvent::Log { level: Info, line: "resumed after {N}s stall" }` and
//! arms the watcher so a later stall can fire again.
//!
//! The watcher is cancellable via a [`CancellationToken`] and returns
//! `Ok(())` when the broadcast channel is fully closed. Lagged receivers
//! (`RecvError::Lagged(n)`) log a warning and continue (F-037 pattern).
//!
//! ## What counts as "activity"
//!
//! Only `ToolUse` and `Message` update the last-activity timestamp. `Stop`
//! is explicitly a `Done` signal — it does not count as "still working", so
//! we deliberately ignore it (as well as every other variant) for stall
//! bookkeeping. This matches the R5 acceptance criteria.
//!
//! ## Unit testing
//!
//! Unit tests drive wall-clock behavior deterministically via
//! `tokio::time::pause()` + `advance()`. The watcher polls with a
//! `tokio::time::interval`, which is advanced by the runtime under `pause`.

use std::time::Duration;

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, EventSink, LogLevel};
use chrono::Utc;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, interval};
use tokio_util::sync::CancellationToken;

/// Poll cadence for stall checks. Independent of the stall threshold so
/// that tests can use short thresholds without having to redefine the
/// poll period.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Watch the bus for `ToolUse`/`Message` activity for `id`; emit
/// [`AgentEvent::Stall`] once per silence interval longer than `threshold`,
/// and an `AgentEvent::Log` on resume.
///
/// See the module docs for the detailed contract. Returns:
///
/// - `Ok(())` on cancellation,
/// - `Ok(())` on `RecvError::Closed` (all senders dropped).
pub async fn stall_watcher(
    mut rx: broadcast::Receiver<AgentEvent>,
    tx: EventSink,
    id: AgentId,
    threshold: Duration,
    cancel: CancellationToken,
) -> Result<()> {
    let mut last_activity = Instant::now();
    let mut stall_emitted = false;
    let mut ticker = interval(POLL_INTERVAL);
    // The first `ticker.tick()` fires immediately at t=0; on that tick
    // elapsed < threshold so we simply no-op. Not skipping lets tests
    // observe deterministic tick delivery under `tokio::time::pause()`.

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = ticker.tick() => {
                let elapsed = last_activity.elapsed();
                if elapsed > threshold && !stall_emitted {
                    tracing::debug!(?elapsed, "stall_watcher: emitting Stall");
                    let _ = tx.send(AgentEvent::Stall {
                        id: id.clone(),
                        since: Utc::now(),
                    });
                    stall_emitted = true;
                }
            }
            recv = rx.recv() => {
                match recv {
                    Ok(ev) => {
                        if is_activity_for(&ev, &id) {
                            let now = Instant::now();
                            let elapsed = now.duration_since(last_activity);
                            last_activity = now;
                            if stall_emitted {
                                let line = format!(
                                    "resumed after {}s stall",
                                    elapsed.as_secs(),
                                );
                                let _ = tx.send(AgentEvent::Log {
                                    id: id.clone(),
                                    level: LogLevel::Info,
                                    line,
                                });
                                stall_emitted = false;
                            }
                        }
                    }
                    Err(RecvError::Closed) => return Ok(()),
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "stall_watcher: broadcast lagged, continuing",
                        );
                    }
                }
            }
        }
    }
}

/// Does this event count as agent activity for stall purposes?
///
/// Only `ToolUse` and `Message` for the matching [`AgentId`]. Everything
/// else — including `Stop` via `Done`, `Log`, `Progress`, etc — is ignored.
fn is_activity_for(ev: &AgentEvent, id: &AgentId) -> bool {
    match ev {
        AgentEvent::ToolUse { id: eid, .. } | AgentEvent::Message { id: eid, .. } => eid == id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, MessageRole, channel};
    use std::time::Duration;

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "test")
    }

    fn tool_use(id: &AgentId) -> AgentEvent {
        AgentEvent::ToolUse {
            id: id.clone(),
            tool: "Read".into(),
            input_summary: "x".into(),
        }
    }

    fn message(id: &AgentId) -> AgentEvent {
        AgentEvent::Message {
            id: id.clone(),
            role: MessageRole::Assistant,
            summary: "hi".into(),
        }
    }

    /// Drain the receiver of events the watcher echoes back (our bus is a
    /// broadcast, so watcher outputs also come into our rx). Returns the
    /// first matching event or `None` within `wait`.
    async fn drain_until<F>(
        rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
        wait: Duration,
        pred: F,
    ) -> Option<AgentEvent>
    where
        F: Fn(&AgentEvent) -> bool,
    {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) if pred(&ev) => return Some(ev),
                Ok(Ok(_)) => continue,
                _ => return None,
            }
        }
    }

    /// Give the watcher task many scheduler slots so it can progress
    /// through multiple `select!` iterations under `tokio::time::pause()`.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn activity_within_threshold_emits_no_stall() {
        let (tx, mut watcher_rx_for_assertion) = channel(32);
        let worker_rx = tx.subscribe();
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let handle = tokio::spawn(stall_watcher(
            worker_rx,
            tx.clone(),
            id.clone(),
            Duration::from_secs(60),
            cancel2,
        ));

        // Feed ToolUse every 20s; threshold is 60s → should never stall.
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_secs(20)).await;
            tx.send(tool_use(&id)).unwrap();
            settle().await;
        }

        // Drain what we emitted — but no Stall should be present.
        let stall = drain_until(
            &mut watcher_rx_for_assertion,
            Duration::from_secs(2),
            |ev| matches!(ev, AgentEvent::Stall { .. }),
        )
        .await;
        assert!(stall.is_none(), "no stall expected, got {stall:?}");

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn silence_past_threshold_emits_one_stall() {
        let (tx, mut rx) = channel(32);
        let worker_rx = tx.subscribe();
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let handle = tokio::spawn(stall_watcher(
            worker_rx,
            tx.clone(),
            id.clone(),
            Duration::from_secs(30),
            cancel2,
        ));

        // Advance well past threshold (threshold = 30s, poll = 10s; the
        // 4th tick at t=40s fires after elapsed > threshold).
        tokio::time::sleep(Duration::from_secs(60)).await;
        settle().await;

        let first = drain_until(&mut rx, Duration::from_secs(1), |ev| {
            matches!(ev, AgentEvent::Stall { .. })
        })
        .await;
        assert!(first.is_some(), "expected one Stall");

        // Further silence: still only one Stall (dedupe).
        tokio::time::sleep(Duration::from_secs(120)).await;
        settle().await;
        let second = drain_until(&mut rx, Duration::from_secs(1), |ev| {
            matches!(ev, AgentEvent::Stall { .. })
        })
        .await;
        assert!(
            second.is_none(),
            "dedupe violated, got second Stall {second:?}",
        );

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn activity_after_stall_logs_resumed_and_rearms() {
        let (tx, mut rx) = channel(32);
        let worker_rx = tx.subscribe();
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let handle = tokio::spawn(stall_watcher(
            worker_rx,
            tx.clone(),
            id.clone(),
            Duration::from_secs(30),
            cancel2,
        ));

        // First stall.
        tokio::time::sleep(Duration::from_secs(60)).await;
        settle().await;
        let stall1 = drain_until(&mut rx, Duration::from_secs(1), |ev| {
            matches!(ev, AgentEvent::Stall { .. })
        })
        .await;
        assert!(stall1.is_some());

        // Resume: send a Message.
        tx.send(message(&id)).unwrap();
        settle().await;

        let log = drain_until(&mut rx, Duration::from_secs(1), |ev| {
            matches!(
                ev,
                AgentEvent::Log {
                    level: LogLevel::Info,
                    ..
                }
            )
        })
        .await;
        let line = match log.expect("resume log") {
            AgentEvent::Log { line, .. } => line,
            _ => unreachable!(),
        };
        assert!(
            line.starts_with("resumed after ") && line.ends_with("s stall"),
            "unexpected log line: {line}",
        );

        // Re-arm: another silence past threshold → another Stall.
        tokio::time::sleep(Duration::from_secs(60)).await;
        settle().await;
        let stall2 = drain_until(&mut rx, Duration::from_secs(1), |ev| {
            matches!(ev, AgentEvent::Stall { .. })
        })
        .await;
        assert!(stall2.is_some(), "watcher should re-arm after resume");

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let (tx, _rx) = channel(8);
        let worker_rx = tx.subscribe();
        let id = sample_id();
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let handle = tokio::spawn(stall_watcher(
            worker_rx,
            tx,
            id,
            Duration::from_secs(60),
            cancel2,
        ));

        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("prompt exit")
            .unwrap();
        assert!(res.is_ok());
    }

    // Note: we intentionally omit a "closed channel returns Ok" test.
    // The watcher holds a `tx: EventSink` clone so it can emit Stall /
    // Log events; that clone alone keeps the broadcast channel from
    // reaching the `RecvError::Closed` state. The branch is handled in
    // the code for defensive correctness, but cannot fire in practice
    // while the watcher is still running. Cancellation (tested above)
    // is the sanctioned shutdown path.

    #[tokio::test(start_paused = true)]
    async fn other_agent_events_do_not_count_as_activity() {
        let (tx, mut rx) = channel(32);
        let worker_rx = tx.subscribe();
        let id = sample_id();
        let other = AgentId::new("cavekit", "other");
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();

        let handle = tokio::spawn(stall_watcher(
            worker_rx,
            tx.clone(),
            id.clone(),
            Duration::from_secs(30),
            cancel2,
        ));

        // Feed activity for a DIFFERENT agent — should not prevent stall.
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(5)).await;
            tx.send(tool_use(&other)).unwrap();
        }
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;

        let stall = drain_until(&mut rx, Duration::from_millis(50), |ev| {
            matches!(ev, AgentEvent::Stall { .. })
        })
        .await;
        assert!(
            stall.is_some(),
            "stall should fire: other agent's events don't count"
        );

        cancel.cancel();
        handle.await.unwrap().unwrap();
    }
}
