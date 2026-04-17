//! Per-event JSONL writer (cavekit-hook-ipc.md R2).
//!
//! Each hook invocation appends one or more JSON lines to the matching
//! per-event file under `$STATE/sessions/{id}/hooks/{EventName}.jsonl`.
//!
//! Semantics:
//! - `O_APPEND + O_CREAT`: safe for concurrent writers without an fcntl
//!   lock. A single `write_all` of a short (<PIPE_BUF) line is atomic on
//!   unix, so lines from concurrent writers interleave at line granularity
//!   rather than tearing mid-line.
//! - If `$STATE/sessions/{id}/` does not yet exist, we refuse to create it
//!   — only the supervisor owns session-dir lifecycle. We log to stderr
//!   and return `Ok(())` (fail-open per R3: never block claude).
//! - If `$STATE/sessions/{id}/hooks/` is missing we *do* create it (0700 on
//!   unix) since the hooks subdir is squarely ark-hook's responsibility.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

use ark_types::SessionId;

use crate::event::HookEvent;

/// Resolve the session directory `<state_root>/sessions/<id>/`.
///
/// Mirrors `StateLayout::session_dir` without threading a full
/// `StateLayout` through every writer call site.
pub fn session_state_dir(state_root: &Path, id: &SessionId) -> PathBuf {
    state_root.join("sessions").join(id.as_path_leaf())
}

/// Resolve the target JSONL path for `(session, event)`.
///
/// Returns `{state_root}/sessions/{id}/hooks/{EventName}.jsonl`.
pub fn event_file_path(state_root: &Path, id: &SessionId, event: HookEvent) -> PathBuf {
    session_state_dir(state_root, id)
        .join("hooks")
        .join(format!("{}.jsonl", event.as_str()))
}

/// Append a single event (serialized as JSON) as one line to the
/// matching per-event jsonl file.
///
/// Fail-open contract (R3): if the session dir does not exist we log a
/// warning and return `Ok(())` rather than propagating the error. All
/// other I/O errors are returned so the caller can log + ignore at its
/// own discretion (run.rs wraps in `let _ =`).
pub fn append_event_jsonl(
    state_root: &Path,
    id: &SessionId,
    event: HookEvent,
    record: &serde_json::Value,
) -> io::Result<()> {
    let session_dir = session_state_dir(state_root, id);
    if !session_dir.exists() {
        warn!(
            session = %id.as_str(),
            event = %event,
            session_dir = %session_dir.display(),
            "session state dir missing; skipping JSONL write (fail-open per R3)"
        );
        return Ok(());
    }

    let hooks_dir = session_dir.join("hooks");
    if !hooks_dir.exists() {
        fs::create_dir_all(&hooks_dir)?;
        set_dir_mode_0700(&hooks_dir);
    }

    let path = hooks_dir.join(format!("{}.jsonl", event.as_str()));
    let mut line =
        serde_json::to_string(record).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');

    let mut f = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode_0700(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o700);
        let _ = fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_path: &Path) {
    // Non-unix: accept default perms (kit only mandates 0700 on unix).
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::thread;

    use tempfile::TempDir;

    fn id() -> SessionId {
        SessionId::new("writetest")
    }

    fn mk_session_dir(state_root: &Path, id: &SessionId) -> PathBuf {
        let dir = session_state_dir(state_root, id);
        fs::create_dir_all(&dir).expect("create session dir");
        dir
    }

    #[test]
    fn missing_session_dir_returns_ok_no_file_created() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        let rec = serde_json::json!({"kind": "tool.use"});
        append_event_jsonl(tmp.path(), &id, HookEvent::PostToolUse, &rec).expect("fail-open ok");
        assert!(!session_state_dir(tmp.path(), &id).exists());
        let expected = event_file_path(tmp.path(), &id, HookEvent::PostToolUse);
        assert!(!expected.exists());
    }

    #[test]
    fn fresh_session_dir_creates_hooks_subdir_and_file() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_session_dir(tmp.path(), &id);
        let rec = serde_json::json!({"kind": "tool.use", "tool": "Edit"});
        append_event_jsonl(tmp.path(), &id, HookEvent::PostToolUse, &rec).expect("write ok");

        let hooks = session_state_dir(tmp.path(), &id).join("hooks");
        assert!(hooks.is_dir(), "hooks dir created");

        let path = event_file_path(tmp.path(), &id, HookEvent::PostToolUse);
        assert!(path.is_file(), "jsonl file created");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "{\"kind\":\"tool.use\",\"tool\":\"Edit\"}\n");
    }

    #[test]
    fn two_appends_produce_two_lines() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_session_dir(tmp.path(), &id);
        let a = serde_json::json!({"n": 1});
        let b = serde_json::json!({"n": 2});
        append_event_jsonl(tmp.path(), &id, HookEvent::Stop, &a).unwrap();
        append_event_jsonl(tmp.path(), &id, HookEvent::Stop, &b).unwrap();

        let path = event_file_path(tmp.path(), &id, HookEvent::Stop);
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines, vec!["{\"n\":1}", "{\"n\":2}"]);
    }

    #[test]
    fn all_six_event_names_route_to_correct_file() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_session_dir(tmp.path(), &id);
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::PermissionRequest,
            HookEvent::Notification,
            HookEvent::SessionEnd,
            HookEvent::TaskCompleted,
        ] {
            let rec = serde_json::json!({"marker": ev.as_str()});
            append_event_jsonl(tmp.path(), &id, ev, &rec).unwrap();

            let expected = session_state_dir(tmp.path(), &id)
                .join("hooks")
                .join(format!("{}.jsonl", ev.as_str()));
            assert!(expected.is_file(), "{} file present", ev.as_str());
        }
    }

    #[test]
    fn concurrent_writes_interleave_cleanly() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_session_dir(tmp.path(), &id);
        let state = Arc::new(tmp.path().to_path_buf());
        let id_a = Arc::new(id);

        const N: usize = 50;
        let mut handles = Vec::new();
        for thread_idx in 0..2_usize {
            let state = Arc::clone(&state);
            let id_a = Arc::clone(&id_a);
            handles.push(thread::spawn(move || {
                for i in 0..N {
                    let rec = serde_json::json!({"t": thread_idx, "i": i});
                    append_event_jsonl(&state, &id_a, HookEvent::PostToolUse, &rec)
                        .expect("append ok");
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let path = event_file_path(&state, &id_a, HookEvent::PostToolUse);
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2 * N);
        for line in lines {
            let _: serde_json::Value = serde_json::from_str(line).expect("valid json");
        }
    }
}
