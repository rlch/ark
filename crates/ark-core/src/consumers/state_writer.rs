//! `state_writer` consumer task.
//!
//! Implements cavekit-supervisor.md R2 (first bullet) + cavekit-types-state-events.md R6:
//!
//! - Subscribes to the supervisor's broadcast bus.
//! - Appends every received event to `events.jsonl` via [`crate::EventLogWriter`].
//! - Rolls up `status.json` atomically via [`crate::write_status_atomic`] —
//!   updates `phase`, `last_event_at`, `last_event_summary`, `progress`,
//!   `tab_handles`, `findings`, and `stalled_since`.
//! - Detects phase changes between successive events; on an actual change,
//!   re-broadcasts a [`AgentEvent::PhaseTransition`] back onto the bus via the
//!   supplied `EventSink`, suppressed when the incoming event is itself a
//!   `PhaseTransition` (to avoid loops) or when the rolled-up phase did not
//!   change vs the cached previous phase.
//! - Lagged: `tracing::warn!` and continue. Closed: `Ok(())`. Cancel: `Ok(())`.

use std::sync::Arc;

use anyhow::Result;
use ark_types::{
    AgentEvent, AgentId, AgentStatus, EventSink, Outcome, Phase, StateLayout, TabHandle,
};
use chrono::Utc;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::events_log::EventLogWriter;
use crate::status_writer::{read_status, write_status_atomic};

/// Long-running consumer task. Returns once the bus closes or the cancel
/// token fires. Per-event IO failures are logged and do not terminate the
/// loop.
///
/// `tx` is the **same** `EventSink` the supervisor cloned to all producers;
/// `state_writer` uses it solely to re-broadcast `PhaseTransition` events
/// when its rollup detects an actual phase change. Pass `None` to disable
/// re-broadcast (used by tests that want to observe the `RecvError::Closed`
/// path — `state_writer` holding a `Sender` clone would otherwise keep the
/// channel open indefinitely).
pub async fn state_writer(
    mut rx: Receiver<AgentEvent>,
    tx: Option<EventSink>,
    layout: Arc<StateLayout>,
    id: AgentId,
    supervisor_pid: u32,
    cancel: CancellationToken,
) -> Result<()> {
    // Set up the disk writer for events.jsonl. We own the handle for the
    // life of the loop and drop it (closing the channel) before returning so
    // the writer task drains and exits cleanly.
    let events_path = layout.events_path(&id);
    StateLayout::ensure_dir_0700(&layout.agent_dir(&id))?;
    let log_handle = EventLogWriter::spawn(events_path)?;

    // Cached previous phase for transition dedupe. `None` until the first
    // event lands or until status.json already exists from the supervisor's
    // initial bootstrap (R3 step 1).
    let mut prev_phase: Option<Phase> = match read_status(&layout, &id) {
        Ok(Some(s)) => Some(s.phase),
        _ => None,
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!(agent_id = %id.as_str(), "state_writer: cancel fired, exiting");
                break;
            }
            recv = rx.recv() => match recv {
                Ok(event) => {
                    // 1. Append to events.jsonl. Failures inside the writer
                    //    task are warn-logged there; here we just enqueue.
                    if let Err(e) = log_handle.sender.send(event.clone()) {
                        warn!(error = %e, "state_writer: events.jsonl writer channel closed");
                    }

                    // 2. Roll up status.json.
                    let new_phase = match update_status(&layout, &id, supervisor_pid, &event) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(error = %e, "state_writer: status.json rollup failed");
                            continue;
                        }
                    };

                    // 3. Phase change emit — only when phase actually changed
                    //    vs a *known* prior phase AND the incoming event
                    //    isn't itself PhaseTransition. Suppressing when
                    //    `prev_phase` is None avoids spurious emits on the
                    //    bootstrap event (Starting→Starting is a non-event).
                    let is_phase_event = matches!(event, AgentEvent::PhaseTransition { .. });
                    if !is_phase_event
                        && let Some(prev) = prev_phase
                        && prev != new_phase
                        && let Some(tx) = tx.as_ref()
                    {
                        let transition = AgentEvent::PhaseTransition {
                            id: id.clone(),
                            from: Some(phase_slug(prev).to_string()),
                            to: phase_slug(new_phase).to_string(),
                        };
                        // Send is best-effort — if no receivers remain, the
                        // bus is shutting down and we'll see Closed soon.
                        let _ = tx.send(transition);
                    }
                    prev_phase = Some(new_phase);
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "state_writer: broadcast lagged; continuing");
                    continue;
                }
                Err(RecvError::Closed) => {
                    debug!("state_writer: broadcast closed, exiting");
                    break;
                }
            }
        }
    }

    // Drop the writer-task sender so it flushes and exits.
    drop(log_handle.sender);
    if let Err(e) = log_handle.task.await {
        warn!(error = %e, "state_writer: events_log task join failed");
    }
    Ok(())
}

/// Read-modify-write the agent's `status.json`, returning the new `phase`.
fn update_status(
    layout: &StateLayout,
    id: &AgentId,
    supervisor_pid: u32,
    event: &AgentEvent,
) -> std::io::Result<Phase> {
    // Load current status; if missing, materialize from the event's spec
    // (only Started carries a spec). If neither path applies, we cannot roll
    // up a status snapshot — bail with a NotFound error so the caller logs.
    let mut status = match read_status(layout, id)? {
        Some(s) => s,
        None => match event {
            AgentEvent::Started { spec } => AgentStatus::new(spec.clone(), supervisor_pid),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "status.json missing and event has no spec to bootstrap from",
                ));
            }
        },
    };

    // Always-applied fields.
    status.last_event_at = Utc::now();
    status.last_event_summary = summarize(event);

    // Per-variant rollups.
    match event {
        AgentEvent::Started { .. } => {
            status.phase = Phase::Starting;
        }
        AgentEvent::TabOpened { tab_handle, .. } => {
            push_unique_tab(&mut status.tab_handles, tab_handle.clone());
            // First tab opened typically means we're past Starting.
            if status.phase == Phase::Starting {
                status.phase = Phase::Running;
            }
        }
        AgentEvent::TabClosed { tab_handle, .. } => {
            status.tab_handles.retain(|h| h != tab_handle);
        }
        AgentEvent::Progress { done, total, .. } => {
            status.progress = Some((*done, *total));
            if status.phase == Phase::Starting {
                status.phase = Phase::Running;
            }
        }
        AgentEvent::ToolUse { .. } => {
            if matches!(status.phase, Phase::Starting | Phase::Idle) {
                status.phase = Phase::Running;
            }
            status.stalled_since = None;
        }
        AgentEvent::Message { .. } | AgentEvent::FileEdited { .. } => {
            if matches!(status.phase, Phase::Starting | Phase::Idle) {
                status.phase = Phase::Running;
            }
            status.stalled_since = None;
        }
        AgentEvent::ReviewComment { severity, .. } => {
            status.findings.record(severity.clone());
            status.phase = Phase::Reviewing;
        }
        AgentEvent::PermissionAsked { .. } => {
            status.phase = Phase::Prompting;
        }
        AgentEvent::PermissionResolved { .. } => {
            // Back to running unless a later event re-prompts.
            if status.phase == Phase::Prompting {
                status.phase = Phase::Running;
            }
        }
        AgentEvent::Stall { since, .. } => {
            status.stalled_since = Some(*since);
            status.phase = Phase::Idle;
        }
        AgentEvent::PhaseTransition { to, .. } => {
            if let Some(p) = parse_phase(to) {
                status.phase = p;
            }
        }
        AgentEvent::Done { outcome, .. } => {
            status.phase = match outcome {
                Outcome::Success { .. } => Phase::Done,
                Outcome::Failed { .. } => Phase::Failed,
                Outcome::Killed | Outcome::Timeout => Phase::Done,
                Outcome::Crashed { .. } => Phase::Crashed,
            };
        }
        // No-op for Iteration / TaskDone / Log / Error — they update
        // last_event_* but don't change phase.
        _ => {}
    }

    let phase = status.phase;
    write_status_atomic(layout, id, &status)?;
    Ok(phase)
}

fn push_unique_tab(handles: &mut Vec<TabHandle>, h: TabHandle) {
    if !handles.iter().any(|x| x == &h) {
        handles.push(h);
    }
}

fn phase_slug(p: Phase) -> &'static str {
    match p {
        Phase::Starting => "starting",
        Phase::Running => "running",
        Phase::Idle => "idle",
        Phase::Prompting => "prompting",
        Phase::Reviewing => "reviewing",
        Phase::Done => "done",
        Phase::Failed => "failed",
        Phase::Crashed => "crashed",
    }
}

fn parse_phase(slug: &str) -> Option<Phase> {
    Some(match slug {
        "starting" => Phase::Starting,
        "running" => Phase::Running,
        "idle" => Phase::Idle,
        "prompting" => Phase::Prompting,
        "reviewing" => Phase::Reviewing,
        "done" => Phase::Done,
        "failed" => Phase::Failed,
        "crashed" => Phase::Crashed,
        _ => return None,
    })
}

fn summarize(event: &AgentEvent) -> String {
    use AgentEvent::*;
    match event {
        Started { spec } => format!("started {}", spec.name),
        TabOpened { label, .. } => format!("tab opened: {label}"),
        TabClosed { tab_handle, .. } => format!("tab closed: {}", tab_handle.name),
        Progress {
            done, total, label, ..
        } => match label {
            Some(l) => format!("{done}/{total} {l}"),
            None => format!("{done}/{total}"),
        },
        TaskDone { task_id, .. } => format!("task done: {task_id}"),
        Iteration { n, max, .. } => match max {
            Some(m) => format!("iteration {n}/{m}"),
            None => format!("iteration {n}"),
        },
        PhaseTransition { to, .. } => format!("phase: {to}"),
        ToolUse { tool, .. } => format!("tool: {tool}"),
        Message { role, .. } => format!("message: {role:?}"),
        FileEdited { path, .. } => format!("edit: {}", path.display()),
        ReviewComment { severity, .. } => format!("finding: {severity:?}"),
        PermissionAsked { tool, .. } => format!("perm asked: {tool}"),
        PermissionResolved { tool, decision, .. } => format!("perm {tool}: {decision:?}"),
        Stall { .. } => "stalled".into(),
        Log { line, .. } => format!("log: {line}"),
        Error { message, .. } => format!("error: {message}"),
        Done { outcome, .. } => format!("done: {outcome:?}"),
        _ => "event".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentSpec, MessageRole, Severity, channel};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn layout_in(base: PathBuf) -> Arc<StateLayout> {
        Arc::new(StateLayout::new(
            base.clone(),
            base.join("rt"),
            base.join("cfg"),
        ))
    }

    fn sample_spec(id: &AgentId) -> AgentSpec {
        let mut spec = AgentSpec::new(
            id.clone(),
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/wt"),
            vec!["claude".into()],
        );
        spec.env = BTreeMap::new();
        spec
    }

    #[tokio::test]
    async fn happy_path_writes_jsonl_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(64);
        let _keepalive = tx.subscribe();

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, Some(tx), layout, id, 4242, cancel).await }
        });

        // Bootstrap: Started → TabOpened → ToolUse → Done.
        tx.send(AgentEvent::Started {
            spec: sample_spec(&id),
        })
        .unwrap();
        tx.send(AgentEvent::ToolUse {
            id: id.clone(),
            tool: "Read".into(),
            input_summary: "foo.rs".into(),
        })
        .unwrap();
        tx.send(AgentEvent::Done {
            id: id.clone(),
            outcome: Outcome::Success { artifacts: vec![] },
        })
        .unwrap();

        // Wait for processing: poll status.json for Done.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(s)) = read_status(&layout, &id)
                && s.phase == Phase::Done
            {
                break;
            }
        }

        cancel.cancel();
        let _ = writer.await.unwrap();

        let s = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Done);
        assert!(s.last_event_summary.contains("done"));

        // events.jsonl: should contain at least the 3 input events plus
        // PhaseTransition events emitted by the writer.
        let mut reader = crate::EventLogReader::open(&layout.events_path(&id)).unwrap();
        let records = reader.read_all();
        assert!(
            records.len() >= 3,
            "expected ≥3 records, got {}",
            records.len()
        );
        assert!(
            records
                .iter()
                .any(|r| matches!(r.event, AgentEvent::PhaseTransition { .. })),
            "expected at least one PhaseTransition emitted by state_writer"
        );
    }

    #[tokio::test]
    async fn phase_transition_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(64);
        // Separate subscriber to count emitted PhaseTransition events.
        let mut spy = tx.subscribe();

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, Some(tx), layout, id, 1, cancel).await }
        });

        // Two events that map to the *same* phase (Running) should produce
        // exactly one PhaseTransition (Starting → Running).
        tx.send(AgentEvent::Started {
            spec: sample_spec(&id),
        })
        .unwrap();
        tx.send(AgentEvent::ToolUse {
            id: id.clone(),
            tool: "Read".into(),
            input_summary: "x".into(),
        })
        .unwrap();
        tx.send(AgentEvent::Message {
            id: id.clone(),
            role: MessageRole::Assistant,
            summary: "hi".into(),
        })
        .unwrap();

        // Poll until status reports Running.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(s)) = read_status(&layout, &id)
                && s.phase == Phase::Running
            {
                break;
            }
        }

        cancel.cancel();
        let _ = writer.await.unwrap();

        // Drain and count PhaseTransitions on the spy receiver.
        let mut transitions = 0;
        while let Ok(ev) = spy.try_recv() {
            if matches!(ev, AgentEvent::PhaseTransition { .. }) {
                transitions += 1;
            }
        }
        // Exactly one transition emitted: Starting -> Running. ToolUse and
        // Message do NOT re-emit because the cached prev_phase already
        // matches.
        assert_eq!(
            transitions, 1,
            "expected exactly 1 PhaseTransition, got {transitions}"
        );
    }

    #[tokio::test]
    async fn lagged_warn_and_survives() {
        // capacity 4 + flood 50 → guaranteed Lagged on first recv.
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(4);

        // Pre-flood BEFORE spawning the consumer so it sees Lagged on first
        // recv.
        for _ in 0..50 {
            // Use Started on every send: it's spec-bearing so the consumer
            // can bootstrap status.json from the first non-skipped event.
            tx.send(AgentEvent::Started {
                spec: sample_spec(&id),
            })
            .unwrap();
        }

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, Some(tx), layout, id, 1, cancel).await }
        });

        // Give the consumer time to drain past the Lagged report.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Send one more after the Lagged is consumed.
        tx.send(AgentEvent::Done {
            id: id.clone(),
            outcome: Outcome::Killed,
        })
        .unwrap();

        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(s)) = read_status(&layout, &id)
                && s.phase == Phase::Done
            {
                break;
            }
        }

        cancel.cancel();
        // Must NOT panic, must return Ok.
        let res = writer.await.unwrap();
        assert!(res.is_ok(), "state_writer should survive Lagged: {res:?}");
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(8);
        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let tx = tx.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, Some(tx), layout, id, 1, cancel).await }
        });

        cancel.cancel();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .expect("state_writer didn't return promptly on cancel");
        assert!(res.unwrap().is_ok());
    }

    #[tokio::test]
    async fn closed_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(8);
        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, None, layout, id, 1, cancel).await }
        });

        // Drop all senders → broadcast closes. state_writer holds no tx
        // clone (None passed above), so the channel actually closes.
        drop(tx);
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .expect("state_writer didn't return on Closed");
        assert!(res.unwrap().is_ok());

        // Severity import suppresses unused-import warning while keeping
        // the file's intent self-documenting for future tests.
        let _ = Severity::P0;
    }
}
