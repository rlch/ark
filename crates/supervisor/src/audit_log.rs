//! Control-socket audit log (T-068).
//!
//! Appends one JSONL line per dispatched command to `$STATE/control.log`
//! (aggregate — NOT per-agent). Implements cavekit-hook-ipc.md R5 audit
//! log bullet:
//!
//! > Audit log: every command appended to `$STATE/control.log`
//!
//! # File shape
//!
//! Each line is a JSON object with the following fields:
//! ```json
//! {
//!   "ts": "2026-04-14T12:34:56.789Z",   // ISO 8601 / RFC 3339
//!   "agent_id": "cavekit-auth-01JX...", // AgentId::as_str()
//!   "cmd": { ... },                     // raw request JSON
//!   "response": { ... }                 // raw response JSON
//! }
//! ```
//!
//! # Permissions / atomicity
//!
//! * File created with `O_APPEND | O_CREAT` and mode `0600`.
//! * Parent directory created with mode `0700` (reuses
//!   [`StateLayout::ensure_dir_0700`]).
//! * The kernel's `O_APPEND` guarantee covers concurrent single-line writes
//!   below `PIPE_BUF` (4096 bytes on Linux / macOS) — lines from multiple
//!   tasks will not interleave within a single line. Each `record` call
//!   issues exactly one `write(2)` syscall with the JSONL line (including
//!   trailing `\n`).
//!
//! # Integration
//!
//! [`SupervisorCommandCtx`](crate::commands::SupervisorCommandCtx) gains an
//! optional `Arc<AuditLogger>` — when set, the command handler wraps each
//! dispatch with a `record(...)` call. Callers that don't want audit
//! logging (existing tests) leave the field at `None`.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ark_types::{AgentId, StateLayout};
use chrono::Utc;
use serde_json::{Value as JsonValue, json};

/// Aggregate audit logger writing to `{state_root}/control.log`.
///
/// Cheap to clone via [`Arc`](std::sync::Arc) — stores only the state root.
#[derive(Clone, Debug)]
pub struct AuditLogger {
    state_root: PathBuf,
}

impl AuditLogger {
    /// Construct a logger writing to `{state_root}/control.log`.
    ///
    /// No I/O happens at construction; the log file is created lazily on
    /// the first [`record`](Self::record) call.
    pub fn new(state_root: PathBuf) -> Self {
        Self { state_root }
    }

    /// Path of the control log file (`{state_root}/control.log`).
    pub fn path(&self) -> PathBuf {
        self.state_root.join("control.log")
    }

    /// The state root this logger writes to.
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Append one `{ts, agent_id, cmd, response}` JSONL line.
    ///
    /// * Creates `state_root` (mode 0700) if missing.
    /// * Opens `control.log` with `O_APPEND | O_CREAT`, mode 0600.
    /// * Issues a single `write_all` of the serialized line (including
    ///   trailing `\n`) — atomic against other `O_APPEND` writers below
    ///   `PIPE_BUF`.
    pub fn record(
        &self,
        agent_id: &AgentId,
        cmd: &JsonValue,
        response: &JsonValue,
    ) -> io::Result<()> {
        // Ensure state_root exists with 0700 perms.
        StateLayout::ensure_dir_0700(&self.state_root)?;

        let line = json!({
            "ts": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "agent_id": agent_id.as_str(),
            "cmd": cmd,
            "response": response,
        });

        // Serialize to a single line; JSON objects never contain raw
        // newlines unless explicitly embedded (our keys don't).
        let mut buf =
            serde_json::to_vec(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        buf.push(b'\n');

        let path = self.path();
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(&buf)?;
        // Don't fsync per record — cost would dominate. The supervisor's
        // exit path (or the OS) flushes. Audit loss on hard crash is
        // acceptable per kit.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("audit")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    fn read_lines(path: &Path) -> Vec<JsonValue> {
        let f = std::fs::File::open(path).expect("open log");
        let r = std::io::BufReader::new(f);
        r.lines()
            .map(|l| serde_json::from_str(&l.expect("line")).expect("parse json"))
            .collect()
    }

    #[test]
    fn record_writes_jsonl_parseable_line() {
        let tmp = short_tempdir();
        let state = tmp.path().join("state");
        let logger = AuditLogger::new(state.clone());
        let id = AgentId::new("cavekit", "log1");

        logger
            .record(
                &id,
                &json!({ "cmd": "Ping" }),
                &json!({ "ok": true, "data": "pong" }),
            )
            .expect("record ok");

        let path = state.join("control.log");
        assert!(path.is_file(), "log file exists");
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["agent_id"], JsonValue::String(id.as_str().to_string()));
        assert_eq!(line["cmd"]["cmd"], JsonValue::String("Ping".into()));
        assert_eq!(line["response"]["ok"], JsonValue::Bool(true));
        assert!(
            line["ts"].as_str().is_some(),
            "ts must be a string, got {line}"
        );
    }

    #[test]
    fn record_creates_state_root_with_0700() {
        let tmp = short_tempdir();
        let state = tmp.path().join("deep").join("state");
        assert!(!state.exists());
        let logger = AuditLogger::new(state.clone());
        let id = AgentId::new("cavekit", "mkdir");

        logger
            .record(&id, &json!({}), &json!({}))
            .expect("record ok");

        assert!(state.is_dir(), "state_root created");
        let mode = state.metadata().unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700, "dir mode must be 0700, got {:o}", mode);
    }

    #[test]
    fn log_file_mode_is_0600() {
        let tmp = short_tempdir();
        let state = tmp.path().join("state");
        let logger = AuditLogger::new(state.clone());
        let id = AgentId::new("cavekit", "chmod");

        logger.record(&id, &json!({}), &json!({})).unwrap();

        let path = state.join("control.log");
        let mode = path.metadata().unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "file mode must be 0600, got {:o}", mode);
    }

    #[test]
    fn timestamps_are_monotonic_across_records() {
        let tmp = short_tempdir();
        let state = tmp.path().join("state");
        let logger = AuditLogger::new(state.clone());
        let id = AgentId::new("cavekit", "mono");

        for i in 0..5 {
            logger
                .record(&id, &json!({ "n": i }), &json!({ "ok": true }))
                .unwrap();
            // chrono::Utc::now() has ms resolution; sleep a tick so
            // consecutive records land on distinct stamps.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let lines = read_lines(&state.join("control.log"));
        assert_eq!(lines.len(), 5);
        let mut last: Option<String> = None;
        for line in &lines {
            let ts = line["ts"].as_str().expect("ts str").to_string();
            if let Some(prev) = &last {
                assert!(
                    ts.as_str() >= prev.as_str(),
                    "timestamps must not regress: {prev} -> {ts}"
                );
            }
            last = Some(ts);
        }
    }

    #[tokio::test]
    async fn concurrent_writes_do_not_interleave() {
        let tmp = short_tempdir();
        let state = tmp.path().join("state");
        let logger = Arc::new(AuditLogger::new(state.clone()));
        let id = Arc::new(AgentId::new("cavekit", "concurrent"));

        let mut handles = Vec::new();
        for task_num in 0..2 {
            let logger = logger.clone();
            let id = id.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                for i in 0..50 {
                    logger
                        .record(
                            &id,
                            &json!({ "task": task_num, "i": i }),
                            &json!({ "ok": true, "task": task_num }),
                        )
                        .expect("record ok");
                }
            }));
        }
        for h in handles {
            h.await.expect("task join");
        }

        // Every line must be valid JSON (i.e. no interleaved partial lines).
        let lines = read_lines(&state.join("control.log"));
        assert_eq!(lines.len(), 100, "expected 100 lines, got {}", lines.len());
        for line in &lines {
            assert!(line["cmd"]["task"].is_number(), "malformed: {line}");
            assert!(line["response"]["ok"].as_bool().unwrap_or(false));
        }
    }
}
