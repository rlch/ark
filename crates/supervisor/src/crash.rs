//! PID liveness + crash detection (cavekit-soul Phase 1).
//!
//! Implements the slimmed cavekit-supervisor.md R5:
//!
//! > `ark list` checks PID liveness via `kill(pid, 0)` (nix); marks the
//! > session terminated if the supervisor pid is dead.
//!
//! Three layers:
//!
//! 1. [`is_pid_alive`] — low-level probe via `nix::sys::signal::kill(pid, None)`.
//!    `Ok` = alive, `Err(ESRCH)` = dead, other errors (EPERM in particular)
//!    are treated as alive.
//!
//! 2. [`detect_crashed`] — reads the supervisor pid from
//!    `$STATE/sessions/{id}/pid` (written by orchestration.rs during R3
//!    setup) and probes liveness. Missing pid file returns `Ok(false)` —
//!    "no pid file" means "not currently running", not crashed.
//!
//! 3. [`adjust_status_if_crashed`] — applies detection to `status.json`.
//!    If the file shows a non-terminated session but the recorded pid is
//!    dead, rewrites the status with `terminated_at = Utc::now()`.

use std::io;

use ark_core::status_writer::{read_status, write_session_status_atomic};
use ark_types::{SessionId, StateLayout};
use nix::errno::Errno;
use nix::unistd::Pid;
use tracing::debug;

/// Is the process for `pid` currently alive?
pub fn is_pid_alive(pid: Pid) -> bool {
    match nix::sys::signal::kill(pid, None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(err) => {
            debug!(pid = pid.as_raw(), %err, "kill(pid,0) probe: non-ESRCH error treated as alive");
            true
        }
    }
}

/// Read the pid file for `session_id` and probe liveness.
pub fn detect_crashed(layout: &StateLayout, session_id: &SessionId) -> io::Result<bool> {
    let path = layout.session_pid_path(session_id);
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let pid_str = contents.trim();
    let pid_raw: i32 = match pid_str.parse() {
        Ok(v) => v,
        Err(_) => {
            debug!(
                session = %session_id.as_str(),
                raw = pid_str,
                "malformed pid file; treating as not-crashed"
            );
            return Ok(false);
        }
    };
    let pid = Pid::from_raw(pid_raw);
    Ok(!is_pid_alive(pid))
}

/// If `status.json` shows the session has not yet terminated but the pid
/// file points to a dead process, rewrite the status with
/// `terminated_at = Utc::now()` and preserve every other field.
///
/// Returns `Ok(true)` if an adjustment landed, else `Ok(false)`.
pub fn adjust_status_if_crashed(
    layout: &StateLayout,
    session_id: &SessionId,
) -> io::Result<bool> {
    let Some(mut status) = read_status(layout, session_id).map_err(io::Error::other)? else {
        return Ok(false);
    };
    if status.terminated_at.is_some() {
        // Already terminal — nothing to adjust.
        return Ok(false);
    }
    let crashed = detect_crashed(layout, session_id)?;
    if !crashed {
        return Ok(false);
    }
    status.terminated_at = Some(chrono::Utc::now());
    write_session_status_atomic(layout, session_id, &status).map_err(io::Error::other)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::SessionStatus;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("crash")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    fn write_pid_file(layout: &StateLayout, id: &SessionId, pid: i32) {
        let path = layout.session_pid_path(id);
        StateLayout::ensure_dir_0700(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{pid}\n")).unwrap();
    }

    fn sample_status(id: &SessionId, terminated: bool) -> SessionStatus {
        SessionStatus {
            id: id.clone(),
            started_at: chrono::Utc::now(),
            terminated_at: terminated.then(chrono::Utc::now),
            ext_state: BTreeMap::new(),
        }
    }

    /// A pid that almost certainly does not exist.
    const DEAD_PID: i32 = 999_999;

    #[test]
    fn is_pid_alive_current_pid_is_alive() {
        let pid = Pid::from_raw(std::process::id() as i32);
        assert!(is_pid_alive(pid));
    }

    #[test]
    fn is_pid_alive_dead_pid_is_dead() {
        let pid = Pid::from_raw(DEAD_PID);
        assert!(!is_pid_alive(pid));
    }

    #[test]
    fn is_pid_alive_pid_one_is_alive() {
        let pid = Pid::from_raw(1);
        assert!(is_pid_alive(pid));
    }

    #[test]
    fn detect_crashed_missing_pid_file_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("nopid");
        assert!(!detect_crashed(&layout, &id).expect("ok"));
    }

    #[test]
    fn detect_crashed_live_pid_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("live");
        write_pid_file(&layout, &id, std::process::id() as i32);
        assert!(!detect_crashed(&layout, &id).expect("ok"));
    }

    #[test]
    fn detect_crashed_dead_pid_is_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("dead");
        write_pid_file(&layout, &id, DEAD_PID);
        assert!(detect_crashed(&layout, &id).expect("ok"));
    }

    #[test]
    fn detect_crashed_malformed_pid_file_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("malform");
        let path = layout.session_pid_path(&id);
        StateLayout::ensure_dir_0700(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not-a-number").unwrap();
        assert!(!detect_crashed(&layout, &id).expect("ok"));
    }

    #[test]
    fn adjust_status_if_crashed_sets_terminated_at() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("adj");
        StateLayout::ensure_dir_0700(&layout.session_dir(&id)).unwrap();

        let status = sample_status(&id, false);
        write_session_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, DEAD_PID);

        assert!(adjust_status_if_crashed(&layout, &id).expect("ok"));
        let after = read_status(&layout, &id).unwrap().unwrap();
        assert!(after.terminated_at.is_some());
    }

    #[test]
    fn adjust_status_if_crashed_noop_when_alive() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("noop");
        StateLayout::ensure_dir_0700(&layout.session_dir(&id)).unwrap();

        let status = sample_status(&id, false);
        write_session_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, std::process::id() as i32);

        assert!(!adjust_status_if_crashed(&layout, &id).expect("ok"));
        let after = read_status(&layout, &id).unwrap().unwrap();
        assert!(after.terminated_at.is_none());
    }

    #[test]
    fn adjust_status_if_crashed_noop_for_already_terminated() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("term");
        StateLayout::ensure_dir_0700(&layout.session_dir(&id)).unwrap();

        let status = sample_status(&id, true);
        let original = status.terminated_at;
        write_session_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, DEAD_PID);

        assert!(!adjust_status_if_crashed(&layout, &id).expect("ok"));
        let after = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(after.terminated_at, original);
    }

    #[test]
    fn adjust_status_if_crashed_noop_when_no_status() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("missing");
        assert!(!adjust_status_if_crashed(&layout, &id).expect("ok"));
    }
}
