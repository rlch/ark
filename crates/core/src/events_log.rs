//! Append-only events.jsonl writer/reader.
//!
//! Implements cavekit-types-state-events.md R7:
//! - Background writer task consumes `AgentEvent` via tokio mpsc channel and
//!   appends one JSON object per line: `{"ts": "...", "event": <AgentEvent>}`.
//! - Per-event flush (no batching — agents produce <100 events/sec).
//! - Corruption-tolerant reader skips malformed lines with a warn log; a
//!   subsequent write continues at end of file.
//! - Rotation: none in v1; single file per run.
//!
//! Live tailing (notify-based follow for `ark pane log`) is intentionally NOT
//! implemented here; that belongs to T-042. This module provides the
//! foundation: write, open, read_all.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use ark_types::AgentEvent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tracing::warn;

/// One line of events.jsonl: timestamped `AgentEvent` envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts: DateTime<Utc>,
    pub event: AgentEvent,
}

/// Handle returned by `EventLogWriter::spawn` — sender + task join handle.
///
/// Drop the sender to signal the task to flush and exit; then `await` `task`
/// to observe clean shutdown.
pub struct EventLogHandle {
    pub sender: mpsc::UnboundedSender<AgentEvent>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Spawner for the background writer task.
pub struct EventLogWriter;

impl EventLogWriter {
    /// Open `path` (create + append) and spawn a writer task.
    ///
    /// Each received event is serialized as `EventRecord { ts: now, event }`,
    /// written as a single line, and the file is flushed. Errors during
    /// per-event writes are logged at `warn` and do not terminate the task.
    /// The task exits once all senders are dropped.
    pub fn spawn(path: PathBuf) -> std::io::Result<EventLogHandle> {
        // Open synchronously so the caller sees IO errors immediately rather
        // than losing them inside the spawned task.
        let std_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let file = tokio::fs::File::from_std(std_file);

        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

        let task = tokio::spawn(async move {
            let mut file = file;
            while let Some(event) = rx.recv().await {
                let record = EventRecord {
                    ts: Utc::now(),
                    event,
                };
                let mut line = match serde_json::to_string(&record) {
                    Ok(s) => s,
                    Err(err) => {
                        warn!(error = %err, "events_log: serialize failed; dropping event");
                        continue;
                    }
                };
                line.push('\n');
                if let Err(err) = file.write_all(line.as_bytes()).await {
                    warn!(error = %err, path = %path.display(), "events_log: write_all failed");
                    continue;
                }
                if let Err(err) = file.flush().await {
                    warn!(error = %err, path = %path.display(), "events_log: flush failed");
                }
            }
        });

        Ok(EventLogHandle { sender: tx, task })
    }
}

/// Corruption-tolerant reader over a static snapshot of an events.jsonl file.
///
/// For live tailing see T-042 (`ark pane log`).
pub struct EventLogReader {
    file: std::fs::File,
}

impl EventLogReader {
    /// Open `path` for reading. Returns an error if the file cannot be opened.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self { file })
    }

    /// Read every line currently present, parse as `EventRecord`. Malformed
    /// lines are skipped with a `warn` log. Empty lines are ignored.
    pub fn read_all(&mut self) -> Vec<EventRecord> {
        use std::io::{Seek, SeekFrom};
        // Always read from the start of the file.
        let _ = self.file.seek(SeekFrom::Start(0));
        let reader = BufReader::new(&self.file);
        let mut out = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(err) => {
                    warn!(error = %err, line = lineno + 1, "events_log: read line failed");
                    continue;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventRecord>(&line) {
                Ok(record) => out.push(record),
                Err(err) => {
                    warn!(
                        error = %err,
                        line = lineno + 1,
                        "events_log: malformed line; skipping"
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, LogLevel};
    use std::io::Write as _;

    fn sample_event(n: u32) -> AgentEvent {
        AgentEvent::Log {
            id: AgentId::new("cavekit", "auth"),
            level: LogLevel::Info,
            line: format!("line {n}"),
        }
    }

    #[tokio::test]
    async fn spawn_write_five_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let handle = EventLogWriter::spawn(path.clone()).unwrap();

        for i in 0..5 {
            handle.sender.send(sample_event(i)).unwrap();
        }
        drop(handle.sender);
        handle.task.await.unwrap();

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert_eq!(
            records.len(),
            5,
            "expected 5 records, got {}",
            records.len()
        );

        for (i, rec) in records.iter().enumerate() {
            match &rec.event {
                AgentEvent::Log { line, .. } => {
                    assert_eq!(line, &format!("line {i}"));
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn malformed_line_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Write: valid, garbage, valid.
        {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle.sender.send(sample_event(0)).unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        }
        {
            // Inject garbage directly.
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "this is not json {{").unwrap();
            f.flush().unwrap();
        }
        {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle.sender.send(sample_event(1)).unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        }

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert_eq!(
            records.len(),
            2,
            "expected malformed line skipped, got {} records",
            records.len()
        );
    }

    #[tokio::test]
    async fn per_event_flush_visible_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let handle = EventLogWriter::spawn(path.clone()).unwrap();

        handle.sender.send(sample_event(42)).unwrap();

        // Poll for the write to land (flush is per-event but spawn scheduling
        // is async). Allow up to ~1s; normally completes in <10ms.
        let mut records = Vec::new();
        for _ in 0..100 {
            if let Ok(mut reader) = EventLogReader::open(&path) {
                records = reader.read_all();
                if !records.is_empty() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(records.len(), 1, "per-event flush should expose record");

        drop(handle.sender);
        handle.task.await.unwrap();
    }

    #[tokio::test]
    async fn timestamps_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let handle = EventLogWriter::spawn(path.clone()).unwrap();

        for i in 0..10 {
            handle.sender.send(sample_event(i)).unwrap();
        }
        drop(handle.sender);
        handle.task.await.unwrap();

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert_eq!(records.len(), 10);
        for pair in records.windows(2) {
            assert!(
                pair[1].ts >= pair[0].ts,
                "ts not monotonic: {} then {}",
                pair[0].ts,
                pair[1].ts
            );
        }
    }

    /// T-118 (cavekit-testing R3): an empty `events.jsonl` must produce an
    /// empty iterator without panicking. This is the common "agent just
    /// spawned, no events yet" case for `ark pane log`.
    #[test]
    fn empty_file_yields_no_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // Touch empty file.
        std::fs::File::create(&path).unwrap();

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert!(
            records.is_empty(),
            "empty file should yield zero records, got {}",
            records.len()
        );
    }

    /// T-118: a record truncated mid-line (no trailing newline, invalid JSON
    /// suffix) must be skipped without panicking and without poisoning the
    /// rest of the reader. Simulates a supervisor crash partway through a
    /// `write_all` call.
    #[tokio::test]
    async fn truncated_trailing_record_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Write one valid record using the real writer so the JSON shape
        // matches what a reader would expect.
        {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle.sender.send(sample_event(0)).unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        }
        // Append a truncated JSON fragment with no newline (simulating a
        // crash mid-write).
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            // Deliberately missing closing braces + newline.
            f.write_all(b"{\"ts\":\"2026-04-15T00:00:00Z\",\"event\":{\"Log\":{\"id\"")
                .unwrap();
            f.flush().unwrap();
        }

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert_eq!(
            records.len(),
            1,
            "truncated trailing record must be skipped (got {} records)",
            records.len()
        );
    }

    /// T-118: every line malformed → reader returns an empty vec without
    /// panic. Worst-case corruption scenario.
    #[test]
    fn all_garbage_file_yields_no_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"not json\n{unclosed\nplain text\n").unwrap();

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert!(records.is_empty(), "all-garbage file must yield 0 records");
    }

    /// T-118: blank lines (leading, trailing, interspersed) are silently
    /// ignored so that operator edits or POSIX tools that append bare
    /// newlines don't cost us valid records.
    #[tokio::test]
    async fn blank_lines_interspersed_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        // Valid record.
        {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle.sender.send(sample_event(0)).unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        }
        // Mixed blank lines + whitespace-only line.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f).unwrap();
            writeln!(f, "   ").unwrap();
            writeln!(f).unwrap();
            f.flush().unwrap();
        }
        // Another valid record.
        {
            let handle = EventLogWriter::spawn(path.clone()).unwrap();
            handle.sender.send(sample_event(1)).unwrap();
            drop(handle.sender);
            handle.task.await.unwrap();
        }

        let mut reader = EventLogReader::open(&path).unwrap();
        let records = reader.read_all();
        assert_eq!(
            records.len(),
            2,
            "blank/whitespace lines must not consume records (got {})",
            records.len()
        );
    }

    /// T-118: `read_all` can be called twice on the same reader and yields
    /// identical results (seeks back to start). Guards the `Seek::seek` call
    /// in the reader against regression.
    #[tokio::test]
    async fn read_all_is_rewindable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let handle = EventLogWriter::spawn(path.clone()).unwrap();
        for i in 0..3 {
            handle.sender.send(sample_event(i)).unwrap();
        }
        drop(handle.sender);
        handle.task.await.unwrap();

        let mut reader = EventLogReader::open(&path).unwrap();
        let first = reader.read_all();
        let second = reader.read_all();
        assert_eq!(first.len(), 3);
        assert_eq!(
            first.len(),
            second.len(),
            "second read must yield the same record count"
        );
    }

    /// T-118: opening a non-existent file surfaces `NotFound` — callers
    /// (e.g. `ark pane log` before the agent writes anything) rely on this
    /// to distinguish "never logged" from "empty log".
    #[test]
    fn open_missing_file_returns_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.jsonl");
        match EventLogReader::open(&path) {
            Ok(_) => panic!("open of missing file must fail"),
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
        }
    }
}
