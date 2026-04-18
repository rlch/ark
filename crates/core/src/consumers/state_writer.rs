//! `state_writer` consumer task (soul phase 1 T-025).
//!
//! Consumes `CoreEvent` from the supervisor broadcast bus and maintains the
//! on-disk session state tree per cavekit-soul-phase-1-supervisor.md R2 +
//! cavekit-soul-phase-1-types.md R1/R4/R6:
//!
//! - [`CoreEvent::SessionStarted`] — atomically publishes `spec.json` and
//!   seeds `status.json` with an empty `ext_state` map.
//! - [`CoreEvent::SessionEnded`] — read-modify-writes `status.json`,
//!   setting `terminated_at`.
//! - [`CoreEvent::Ext`], [`CoreEvent::Log`], [`CoreEvent::Error`] — append
//!   as line-delimited JSON to `sessions/{id}/events.jsonl`.
//!
//! `ext_state` stays empty here; per cavekit-soul roadmap core never writes
//! into those buckets — each extension owns its own entry via the
//! `reaction_dispatcher` (`SetStatus` op) and, later, via dedicated
//! ext-registered state-writer hooks.
//!
//! Resilient to `RecvError::Lagged(n)` (warn-log + continue), exits on
//! `RecvError::Closed`, honors a `tokio_util::sync::CancellationToken` for
//! supervisor-driven shutdown.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use ark_types::{CoreEvent, EventSink, SessionId, SessionStatus, StateLayout};
use chrono::Utc;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::events_log::EventLogWriter;
use crate::status_writer::{read_status, write_session_status_atomic};

/// Long-running consumer task. Returns once the bus closes or the cancel
/// token fires. Per-event IO failures are logged and do not terminate the
/// loop.
///
/// `id` is the session whose state tree this writer maintains. The
/// writer will:
/// - Create `sessions/{id}/` on-demand.
/// - Publish `spec.json` and `status.json` on the first `SessionStarted`
///   it observes for the session.
/// - Append every subsequent event to `events.jsonl`.
///
/// The `_tx` parameter is kept for API symmetry with earlier revisions —
/// the new core writer does not re-broadcast any derived events (phase is
/// an extension concept now). It's unused today but reserved for future
/// ext-hook fan-out without churning callers.
// TODO(cavekit-soul Phase 2): ext-registered state_writer hooks
pub async fn state_writer(
    mut rx: Receiver<CoreEvent>,
    _tx: Option<EventSink>,
    layout: Arc<StateLayout>,
    id: SessionId,
    cancel: CancellationToken,
) -> Result<()> {
    // Ensure `sessions/{id}/` exists with tight perms before the event-log
    // writer tries to open `events.jsonl`. `StateLayout::ensure_dir_0700`
    // is idempotent.
    StateLayout::ensure_dir_0700(&layout.session_dir(&id))?;

    let events_path = layout.session_events_path(&id);
    let log_handle = EventLogWriter::spawn(events_path)?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!(session_id = %id.as_str(), "state_writer: cancel fired, exiting");
                break;
            }
            recv = rx.recv() => match recv {
                Ok(event) => {
                    // Lifecycle rollups first; the events.jsonl append
                    // below is unconditional (every CoreEvent gets logged).
                    match &event {
                        CoreEvent::SessionStarted { spec } => {
                            if let Err(e) = write_session_spec(&layout, spec) {
                                warn!(error = %e, "state_writer: spec.json write failed");
                            }
                            if let Err(e) = seed_session_status(&layout, spec) {
                                warn!(error = %e, "state_writer: status.json seed failed");
                            }
                        }
                        CoreEvent::SessionEnded { terminated_at, .. } => {
                            if let Err(e) = update_terminated_at(&layout, &id, *terminated_at) {
                                warn!(error = %e, "state_writer: status.json terminated_at update failed");
                            }
                        }
                        // Log / Error / Ext are append-only on events.jsonl.
                        _ => {}
                    }

                    // Unconditional: every event lands in events.jsonl.
                    if let Err(e) = log_handle.sender.send(event) {
                        warn!(error = %e, "state_writer: events.jsonl writer channel closed");
                    }
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

/// Atomically publish `spec.json` for `spec.id` via temp-file + rename.
fn write_session_spec(layout: &StateLayout, spec: &ark_types::SessionSpec) -> std::io::Result<()> {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    let session_dir = layout.session_dir(&spec.id);
    StateLayout::ensure_dir_0700(&session_dir)?;
    let final_path = layout.session_spec_path(&spec.id);
    let tmp_path = {
        let mut p = final_path.clone();
        let mut name = p
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("spec.json"));
        name.push(".tmp");
        p.set_file_name(name);
        p
    };

    let bytes = serde_json::to_vec_pretty(spec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            tracing::warn!(
                path = %tmp_path.display(),
                "stale spec.json.tmp from previous writer; overwriting"
            );
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?
        }
        Err(e) => return Err(e),
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Seed `status.json` on the first `SessionStarted` observation. Later
/// observations leave the existing status intact — `spec.json` is
/// immutable so a second seed would be a no-op; idempotency is
/// preserved by short-circuiting when the file already exists.
fn seed_session_status(layout: &StateLayout, spec: &ark_types::SessionSpec) -> std::io::Result<()> {
    if read_status(layout, &spec.id)?.is_some() {
        return Ok(());
    }
    let status = SessionStatus {
        id: spec.id.clone(),
        started_at: Utc::now(),
        terminated_at: None,
        ext_state: BTreeMap::new(),
    };
    write_session_status_atomic(layout, &spec.id, &status)
}

/// Update `status.json.terminated_at`. When status.json is missing
/// (SessionEnded before SessionStarted — shouldn't happen in practice but
/// we accept it) we bootstrap a minimal status so the terminal timestamp
/// is still captured.
fn update_terminated_at(
    layout: &StateLayout,
    id: &SessionId,
    terminated_at: chrono::DateTime<Utc>,
) -> std::io::Result<()> {
    let status = match read_status(layout, id)? {
        Some(mut s) => {
            s.terminated_at = Some(terminated_at);
            s
        }
        None => SessionStatus {
            id: id.clone(),
            started_at: terminated_at,
            terminated_at: Some(terminated_at),
            ext_state: BTreeMap::new(),
        },
    };
    write_session_status_atomic(layout, id, &status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{ExtEvent, SessionSpec, channel};
    use std::path::PathBuf;

    fn layout_in(base: PathBuf) -> Arc<StateLayout> {
        Arc::new(StateLayout::new(
            base.clone(),
            base.join("rt"),
            base.join("cfg"),
        ))
    }

    fn sample_spec(id: &SessionId) -> SessionSpec {
        SessionSpec {
            id: id.clone(),
            name: "auth".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp/worktree"),
            env: BTreeMap::new(),
            created_at: Utc::now(),
            ext_config: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn session_started_writes_spec_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = SessionId::new("auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(64);
        let _keepalive = tx.subscribe();

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, None, layout, id, cancel).await }
        });

        tx.send(CoreEvent::SessionStarted {
            spec: sample_spec(&id),
        })
        .unwrap();

        // Wait for spec.json to appear.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if layout.session_spec_path(&id).is_file() {
                break;
            }
        }

        cancel.cancel();
        let _ = writer.await.unwrap();

        // spec.json present and round-trips.
        let spec_bytes = std::fs::read(layout.session_spec_path(&id)).expect("spec.json");
        let got_spec: SessionSpec = serde_json::from_slice(&spec_bytes).expect("spec parse");
        assert_eq!(got_spec.id, id);
        assert_eq!(got_spec.name, "auth");

        // status.json seeded with terminated_at = None and empty ext_state.
        let status = read_status(&layout, &id)
            .expect("read status")
            .expect("status.json");
        assert_eq!(status.id, id);
        assert!(status.terminated_at.is_none());
        assert!(status.ext_state.is_empty());
    }

    #[tokio::test]
    async fn session_ended_updates_terminated_at() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = SessionId::new("auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(64);
        let _keepalive = tx.subscribe();

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, None, layout, id, cancel).await }
        });

        // Start first, then end.
        tx.send(CoreEvent::SessionStarted {
            spec: sample_spec(&id),
        })
        .unwrap();
        let end_ts = Utc::now();
        tx.send(CoreEvent::SessionEnded {
            terminated_at: end_ts,
            exit: ark_types::ExitReason::Normal,
        })
        .unwrap();

        // Wait for terminated_at to land.
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if let Ok(Some(s)) = read_status(&layout, &id)
                && s.terminated_at.is_some()
            {
                break;
            }
        }

        cancel.cancel();
        let _ = writer.await.unwrap();

        let status = read_status(&layout, &id).unwrap().unwrap();
        let got = status.terminated_at.expect("terminated_at populated");
        // Timestamps round-trip through serde so exact-equality is fine.
        assert_eq!(got, end_ts);
    }

    #[tokio::test]
    async fn ext_event_appends_to_events_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = SessionId::new("auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(64);
        let _keepalive = tx.subscribe();

        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, None, layout, id, cancel).await }
        });

        tx.send(CoreEvent::SessionStarted {
            spec: sample_spec(&id),
        })
        .unwrap();
        tx.send(CoreEvent::Ext(ExtEvent {
            ext: "claude-code".into(),
            kind: "tool.use".into(),
            payload: serde_json::json!({ "tool": "Read" }),
        }))
        .unwrap();
        tx.send(CoreEvent::Log {
            level: "info".into(),
            message: "hello".into(),
            target: None,
        })
        .unwrap();

        // Wait until events.jsonl has content.
        let events_path = layout.session_events_path(&id);
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            if events_path.is_file()
                && std::fs::metadata(&events_path)
                    .map(|m| m.len())
                    .unwrap_or(0)
                    > 0
            {
                break;
            }
        }

        cancel.cancel();
        let _ = writer.await.unwrap();

        // Read the log back and verify we see SessionStarted + Ext + Log.
        let mut reader = crate::EventLogReader::open(&events_path).unwrap();
        let records = reader.read_all();
        assert!(
            records.len() >= 3,
            "expected at least 3 records, got {}",
            records.len()
        );
        assert!(
            records.iter().any(|r| matches!(r.event, CoreEvent::Ext(_))),
            "at least one Ext event must be present in events.jsonl"
        );
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout_in(dir.path().to_path_buf());
        let id = SessionId::new("auth");
        let cancel = CancellationToken::new();

        let (tx, rx) = channel(8);
        let writer = tokio::spawn({
            let layout = layout.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            async move { state_writer(rx, None, layout, id, cancel).await }
        });

        cancel.cancel();
        drop(tx);
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .expect("state_writer didn't return promptly on cancel");
        assert!(res.unwrap().is_ok());
    }
}
