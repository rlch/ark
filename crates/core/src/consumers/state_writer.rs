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
    AgentEvent, AgentId, AgentSpec, AgentStatus, EventSink, Outcome, Phase, StateLayout, TabHandle,
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
///
/// Lazy bootstrap: if `status.json` is missing AND the event is not
/// `Started`, a minimal [`AgentStatus`] is synthesized (phase = `Running`,
/// empty counts, a placeholder spec derived from `id`) so subsequent
/// events are still rolled up instead of being dropped. If `Started`
/// arrives later, its authoritative spec overlays the placeholder.
/// During the short window before `Started`, status.json may report
/// `phase = Running` with a stub spec — acceptable for v1 since events
/// are demonstrably flowing.
///
/// Late Started events fill in spec metadata but do NOT regress phase if
/// the agent has already advanced. If the writer lazy-bootstrapped from
/// an earlier non-Started event (setting phase = Running) and Started
/// arrives afterward, we overlay Started's spec but preserve the
/// already-advanced phase rather than snapping back to Starting. This
/// also guards terminal phases (Done / Failed / Crashed) from being
/// resurrected by an out-of-order Started. See F-054 (+F-047 cross-ref).
fn update_status(
    layout: &StateLayout,
    id: &AgentId,
    supervisor_pid: u32,
    event: &AgentEvent,
) -> std::io::Result<Phase> {
    // Load current status; if missing, bootstrap. Preferred path: Started
    // carries the authoritative spec. Fallback: synthesize a stub so the
    // rollup proceeds instead of dropping every event when the receiver
    // missed Started (e.g. due to Lagged).
    //
    // `freshly_created` distinguishes "we just made this status in this
    // call" from "we loaded an existing status from disk". Only the
    // fresh-from-Started case may legitimately set phase = Starting; a
    // late Started over an already-advanced status must not regress.
    let (mut status, freshly_created) = match read_status(layout, id)? {
        Some(s) => (s, false),
        None => match event {
            AgentEvent::Started { spec } => (AgentStatus::new(spec.clone(), supervisor_pid), true),
            _ => {
                let mut s = AgentStatus::new(stub_spec(id), supervisor_pid);
                s.phase = Phase::Running;
                (s, true)
            }
        },
    };

    // Always-applied fields.
    status.last_event_at = Utc::now();
    status.last_event_summary = summarize(event);

    // Per-variant rollups.
    match event {
        AgentEvent::Started { spec } => {
            // Overlay Started's authoritative spec if we lazy-bootstrapped
            // with a stub earlier; otherwise this is a fresh bootstrap and
            // AgentStatus::new already populated the spec.
            status.spec = spec.clone();
            // Phase semantics: Started means "the agent just came up".
            // Only apply that if (a) we just created status this call
            // (true fresh bootstrap) or (b) the loaded status is still
            // in Starting (hasn't advanced yet). Otherwise a late Started
            // would regress Running/Reviewing/Done/etc. back to Starting.
            // See F-054 and F-047 cross-reference.
            if freshly_created || status.phase == Phase::Starting {
                status.phase = Phase::Starting;
            }
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

/// Build a minimal placeholder [`AgentSpec`] from just the agent id.
///
/// Used exclusively by the lazy-bootstrap path when `status.json` is
/// missing and the event does not carry a spec (every variant except
/// `Started`). The placeholder is overwritten when `Started` arrives
/// later; consumers reading status.json during the bootstrap window
/// should treat these fields as stubs.
fn stub_spec(id: &AgentId) -> AgentSpec {
    AgentSpec::new(
        id.clone(),
        id.name(),
        id.orchestrator(),
        "",
        std::path::PathBuf::new(),
        Vec::new(),
    )
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
    async fn lazy_bootstrap_before_started_then_started_overlays_spec() {
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
            async move { state_writer(rx, Some(tx), layout, id, 7777, cancel).await }
        });

        // Feed events with NO Started first. Consumer must lazy-bootstrap.
        tx.send(AgentEvent::ToolUse {
            id: id.clone(),
            tool: "Read".into(),
            input_summary: "foo.rs".into(),
        })
        .unwrap();
        tx.send(AgentEvent::Message {
            id: id.clone(),
            role: MessageRole::Assistant,
            summary: "hi".into(),
        })
        .unwrap();

        // Wait until status.json exists and reports Running.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(s)) = read_status(&layout, &id)
                && s.phase == Phase::Running
            {
                break;
            }
        }
        let mid = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(mid.phase, Phase::Running, "pre-Started phase");
        // Stub spec: engine is "" until Started overlays.
        assert_eq!(mid.spec.engine, "", "stub spec engine should be empty");

        // Now Started arrives late — its spec must overlay the stub.
        let real_spec = sample_spec(&id);
        tx.send(AgentEvent::Started {
            spec: real_spec.clone(),
        })
        .unwrap();
        tx.send(AgentEvent::Done {
            id: id.clone(),
            outcome: Outcome::Success { artifacts: vec![] },
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
        let _ = writer.await.unwrap();

        let s = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(
            s.spec.engine, "claude-code",
            "Started must overlay stub spec with authoritative fields"
        );
        assert_eq!(s.spec.name, real_spec.name);
    }

    #[tokio::test]
    async fn late_started_does_not_regress_phase() {
        // F-054: Bootstrap ToolUse → phase=Running. Late Started arrives.
        // Spec must be overlaid, but phase must stay Running (not regress
        // to Starting).
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");

        // Drive update_status directly so we can observe status.json
        // after each event without racing the broadcast consumer.
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        // 1. Lazy bootstrap via ToolUse → phase=Running, stub spec.
        let p1 = update_status(
            &layout,
            &id,
            4242,
            &AgentEvent::ToolUse {
                id: id.clone(),
                tool: "Read".into(),
                input_summary: "foo.rs".into(),
            },
        )
        .unwrap();
        assert_eq!(p1, Phase::Running);
        let s1 = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s1.phase, Phase::Running);
        assert_eq!(s1.spec.engine, "", "stub spec engine should be empty");

        // 2. Late Started → spec overlays but phase MUST stay Running.
        let real_spec = sample_spec(&id);
        let p2 = update_status(
            &layout,
            &id,
            4242,
            &AgentEvent::Started {
                spec: real_spec.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            p2,
            Phase::Running,
            "late Started must NOT regress advanced phase to Starting"
        );
        let s2 = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s2.phase, Phase::Running);
        assert_eq!(
            s2.spec.engine, "claude-code",
            "late Started must still overlay spec"
        );
        assert_eq!(s2.spec.name, real_spec.name);
    }

    #[tokio::test]
    async fn toolue_started_done_final_phase_done() {
        // F-054: ToolUse → Started → Done. Final phase must be Done, not
        // Starting (i.e. Started never regressed, then Done applied).
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::ToolUse {
                id: id.clone(),
                tool: "Read".into(),
                input_summary: "x".into(),
            },
        )
        .unwrap();
        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::Started {
                spec: sample_spec(&id),
            },
        )
        .unwrap();
        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::Done {
                id: id.clone(),
                outcome: Outcome::Success { artifacts: vec![] },
            },
        )
        .unwrap();

        let s = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Done);
    }

    #[tokio::test]
    async fn late_started_after_done_preserves_terminal() {
        // F-054: Terminal phase (Done) must be preserved even if a
        // straggling Started arrives afterward. Spec still overlays.
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        // Bootstrap directly via Done → phase=Done (first event, stub spec).
        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::ToolUse {
                id: id.clone(),
                tool: "Read".into(),
                input_summary: "x".into(),
            },
        )
        .unwrap();
        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::Done {
                id: id.clone(),
                outcome: Outcome::Success { artifacts: vec![] },
            },
        )
        .unwrap();
        let pre = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(pre.phase, Phase::Done);

        // Late Started after terminal.
        let real_spec = sample_spec(&id);
        update_status(
            &layout,
            &id,
            1,
            &AgentEvent::Started {
                spec: real_spec.clone(),
            },
        )
        .unwrap();

        let s = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(
            s.phase,
            Phase::Done,
            "terminal phase must not be resurrected by late Started"
        );
        assert_eq!(
            s.spec.engine, "claude-code",
            "late Started must still overlay spec even over terminal phase"
        );
    }

    #[tokio::test]
    async fn lazy_bootstrap_without_started_at_all() {
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
            async move { state_writer(rx, Some(tx), layout, id, 8888, cancel).await }
        });

        // Skip Started entirely.
        tx.send(AgentEvent::ToolUse {
            id: id.clone(),
            tool: "Edit".into(),
            input_summary: "bar.rs".into(),
        })
        .unwrap();
        tx.send(AgentEvent::Done {
            id: id.clone(),
            outcome: Outcome::Success { artifacts: vec![] },
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
        let _ = writer.await.unwrap();

        let s = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(
            s.phase,
            Phase::Done,
            "Done outcome must still apply even when Started was missed"
        );
        assert!(
            s.last_event_summary.contains("done"),
            "summary should reflect the last event: {}",
            s.last_event_summary
        );
        // last_event_at was updated from the stub default.
        assert!(
            (Utc::now() - s.last_event_at).num_seconds().abs() < 10,
            "last_event_at should be recent"
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
