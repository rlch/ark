//! Per-event JSONL writer (T-048, cavekit-hook-ipc.md R2).
//!
//! Each hook invocation appends one or more JSON lines to the matching
//! per-event file under `$STATE/agents/{id}/hooks/{EventName}.jsonl`.
//!
//! Semantics:
//! - `O_APPEND + O_CREAT`: safe for concurrent writers without an fcntl
//!   lock. A single `write_all` of a short (<PIPE_BUF) line is atomic on
//!   unix, so lines from concurrent writers interleave at line granularity
//!   rather than tearing mid-line.
//! - If `$STATE/agents/{id}/` does not yet exist, we refuse to create it
//!   — only the supervisor owns agent-dir lifecycle. We log to stderr
//!   and return `Ok(())` (fail-open per R3: never block claude).
//! - If `$STATE/agents/{id}/hooks/` is missing we *do* create it (0700 on
//!   unix) since the hooks subdir is squarely ark-hook's responsibility.
//!
//! Mapping between the running hook's `HookEvent` and the target file:
//! the `HookEvent` variant alone picks the file. A single hook invocation
//! that produces N AgentEvents via [`payload_to_events`] writes N lines
//! to exactly one file, mirroring the kit's per-file semantic
//! (`PostToolUse.jsonl` is the stream of `AgentEvent::ToolUse`-derived
//! records for that hook).
//!
//! [`payload_to_events`]: crate::payload::payload_to_events

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

use ark_types::AgentId;

use crate::event::HookEvent;

/// Resolve the target JSONL path for `(agent, event)`.
///
/// Returns `{state_root}/agents/{id}/hooks/{EventName}.jsonl`.
pub fn event_file_path(state_root: &Path, agent_id: &AgentId, event: HookEvent) -> PathBuf {
    agent_id
        .state_dir(state_root)
        .join("hooks")
        .join(format!("{}.jsonl", event.as_str()))
}

/// Append a single AgentEvent (serialized as JSON) as one line to the
/// matching per-event jsonl file.
///
/// Fail-open contract (R3): if the agent dir does not exist we log a
/// warning and return `Ok(())` rather than propagating the error. All
/// other I/O errors are returned so the caller can log + ignore at its
/// own discretion (run.rs wraps in `let _ =`).
pub fn append_event_jsonl(
    state_root: &Path,
    agent_id: &AgentId,
    event: HookEvent,
    record: &serde_json::Value,
) -> io::Result<()> {
    let agent_dir = agent_id.state_dir(state_root);
    if !agent_dir.exists() {
        warn!(
            agent = %agent_id,
            event = %event,
            agent_dir = %agent_dir.display(),
            "agent state dir missing; skipping JSONL write (fail-open per R3)"
        );
        return Ok(());
    }

    let hooks_dir = agent_dir.join("hooks");
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

    use ark_types::AgentId;
    use tempfile::TempDir;

    fn id() -> AgentId {
        AgentId::new("cavekit", "writetest")
    }

    fn mk_agent_dir(state_root: &Path, id: &AgentId) -> PathBuf {
        let dir = id.state_dir(state_root);
        fs::create_dir_all(&dir).expect("create agent dir");
        dir
    }

    #[test]
    fn missing_agent_dir_returns_ok_no_file_created() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        let rec = serde_json::json!({"kind": "tool_use"});
        append_event_jsonl(tmp.path(), &id, HookEvent::PostToolUse, &rec).expect("fail-open ok");
        // No agent dir was created.
        assert!(!id.state_dir(tmp.path()).exists());
        // No hooks dir, no file.
        let expected = event_file_path(tmp.path(), &id, HookEvent::PostToolUse);
        assert!(!expected.exists());
    }

    #[test]
    fn fresh_agent_dir_creates_hooks_subdir_and_file() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_agent_dir(tmp.path(), &id);
        let rec = serde_json::json!({"kind": "tool_use", "tool": "Edit"});
        append_event_jsonl(tmp.path(), &id, HookEvent::PostToolUse, &rec).expect("write ok");

        let hooks = id.state_dir(tmp.path()).join("hooks");
        assert!(hooks.is_dir(), "hooks dir created");

        let path = event_file_path(tmp.path(), &id, HookEvent::PostToolUse);
        assert!(path.is_file(), "jsonl file created");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "{\"kind\":\"tool_use\",\"tool\":\"Edit\"}\n");
    }

    #[test]
    fn two_appends_produce_two_lines() {
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_agent_dir(tmp.path(), &id);
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
        mk_agent_dir(tmp.path(), &id);
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

            let expected = id
                .state_dir(tmp.path())
                .join("hooks")
                .join(format!("{}.jsonl", ev.as_str()));
            assert!(expected.is_file(), "{} file present", ev.as_str());
            let contents = fs::read_to_string(&expected).unwrap();
            assert!(
                contents.contains(ev.as_str()),
                "{} contents match",
                ev.as_str()
            );
        }
    }

    #[test]
    fn concurrent_writes_interleave_cleanly() {
        // Two threads, each writing N short lines (well under PIPE_BUF).
        // Every line must end up intact (parseable JSON on its own line).
        let tmp = TempDir::new().unwrap();
        let id = id();
        mk_agent_dir(tmp.path(), &id);
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
        assert_eq!(lines.len(), 2 * N, "every line intact");
        for line in lines {
            // Each line must parse as JSON (no mid-line tearing).
            let _: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("torn line `{line}`: {e}"));
        }
    }
}
