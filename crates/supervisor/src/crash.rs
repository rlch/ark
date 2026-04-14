//! PID liveness + crash detection (T-071).
//!
//! Implements cavekit-supervisor.md R5:
//!
//! > `ark list` checks PID liveness via `kill(pid, 0)` (nix); marks
//! > `Crashed` phase in displayed status if pid dead
//!
//! Three layers:
//!
//! 1. [`is_pid_alive`] — low-level probe via `nix::sys::signal::kill(pid, None)`
//!    which issues signal 0 (a noop that still performs the permission +
//!    existence check). `Ok` = alive, `Err(ESRCH)` = dead, other errors
//!    (EPERM in particular) are treated as alive — a process we don't own
//!    is still *a* process, so we conservatively avoid marking it crashed.
//!
//! 2. [`detect_crashed`] — file-level helper that reads the supervisor pid
//!    from `$STATE/agents/{id}/pid` (written by orchestration.rs during the
//!    R3 state-dir setup) and runs [`is_pid_alive`]. Missing pid file
//!    returns `Ok(false)` — "no pid file" means "not currently running",
//!    which is the honest state; it is not a crash.
//!
//! 3. [`adjust_status_if_crashed`] — applies the detection to the persisted
//!    `status.json`. If the file shows a live phase (Starting / Running /
//!    Reviewing) but the recorded pid is dead, rewrites the status with
//!    `phase = Crashed` and keeps all other fields intact. Returns
//!    `Some(Phase::Crashed)` if an adjustment landed, else `None`. `ark
//!    list` (Tier 4) runs this to display honest state.

use std::io;

use ark_core::{read_status, write_status_atomic};
use ark_types::{AgentId, AgentStatus, Phase, StateLayout};
use nix::errno::Errno;
use nix::unistd::Pid;
use tracing::debug;

/// Is the process for `pid` currently alive?
///
/// Uses `nix::sys::signal::kill(pid, None)` which issues signal 0 — a
/// noop probe that returns `Ok(())` when the target exists and the caller
/// has permission to signal it.
///
/// Semantics:
/// * `Ok(())` → alive.
/// * `Err(ESRCH)` → no such process → dead.
/// * `Err(EPERM)` / other → conservatively treated as alive. We don't
///   want to mark a process we can't touch as crashed (it may be running
///   under another uid for all we know).
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

/// Read the pid file for `agent_id` and probe liveness.
///
/// Returns:
/// * `Ok(true)` — pid file exists, parses as a valid pid, and the process
///   is dead (crashed).
/// * `Ok(false)` — pid file missing *or* parses but points to a live
///   process. "Missing" is not treated as crashed because it just means
///   the agent never wrote a pid file (e.g. never started cleanly) — not
///   a crash mid-run.
/// * `Err(_)` — I/O failures other than NotFound.
pub fn detect_crashed(layout: &StateLayout, agent_id: &AgentId) -> io::Result<bool> {
    let path = layout.pid_path(agent_id);
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // No pid file → not currently running, not crashed.
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    let pid_str = contents.trim();
    let pid_raw: i32 = match pid_str.parse() {
        Ok(v) => v,
        Err(_) => {
            // Malformed pid file — treat as not-crashed (best-effort,
            // `ark doctor` surfaces this separately).
            debug!(
                agent = agent_id.as_str(),
                raw = pid_str,
                "malformed pid file; treating as not-crashed"
            );
            return Ok(false);
        }
    };
    let pid = Pid::from_raw(pid_raw);
    Ok(!is_pid_alive(pid))
}

/// If `status.json` shows a live phase (Starting / Running / Reviewing)
/// but the pid file points to a dead process, rewrite the status file
/// with `phase = Phase::Crashed` and preserve every other field.
///
/// Returns:
/// * `Ok(Some(Phase::Crashed))` — adjustment applied.
/// * `Ok(None)` — no adjustment needed (pid alive, no pid file, no
///   status file, or status already in a terminal phase).
/// * `Err(_)` — I/O error.
pub fn adjust_status_if_crashed(
    layout: &StateLayout,
    agent_id: &AgentId,
) -> io::Result<Option<Phase>> {
    let Some(mut status): Option<AgentStatus> =
        read_status(layout, agent_id).map_err(io::Error::other)?
    else {
        return Ok(None);
    };
    // Only "live" phases are candidates for crash adjustment.
    let is_live = matches!(
        status.phase,
        Phase::Starting | Phase::Running | Phase::Reviewing
    );
    if !is_live {
        return Ok(None);
    }
    let crashed = detect_crashed(layout, agent_id)?;
    if !crashed {
        return Ok(None);
    }
    status.phase = Phase::Crashed;
    write_status_atomic(layout, agent_id, &status).map_err(io::Error::other)?;
    Ok(Some(Phase::Crashed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentSpec, Findings};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
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

    fn write_pid_file(layout: &StateLayout, id: &AgentId, pid: i32) {
        let path = layout.pid_path(id);
        StateLayout::ensure_dir_0700(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{pid}\n")).unwrap();
    }

    fn sample_status(id: &AgentId, phase: Phase) -> AgentStatus {
        let mut spec = AgentSpec::new(
            id.clone(),
            "crash-test",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        spec.env = BTreeMap::new();
        AgentStatus {
            spec,
            phase,
            progress: None,
            last_event_at: Utc::now(),
            last_event_summary: "before crash".into(),
            tab_handles: vec![],
            supervisor_pid: 1,
            stalled_since: None,
            findings: Findings::default(),
            hide: false,
        }
    }

    /// A pid that almost certainly does not exist — used across the crash
    /// tests as a "dead pid" sentinel.
    const DEAD_PID: i32 = 999_999;

    #[test]
    fn is_pid_alive_current_pid_is_alive() {
        let pid = Pid::from_raw(std::process::id() as i32);
        assert!(is_pid_alive(pid), "our own pid must register as alive");
    }

    #[test]
    fn is_pid_alive_dead_pid_is_dead() {
        let pid = Pid::from_raw(DEAD_PID);
        assert!(
            !is_pid_alive(pid),
            "pid {DEAD_PID} should not exist and must report dead"
        );
    }

    #[test]
    fn is_pid_alive_pid_one_is_alive() {
        // init is always alive on a booted Unix host. We cannot signal
        // it without CAP_KILL / root, so this exercises the EPERM path
        // which we conservatively treat as alive.
        let pid = Pid::from_raw(1);
        assert!(is_pid_alive(pid), "pid 1 must register as alive");
    }

    #[test]
    fn detect_crashed_missing_pid_file_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "nopid");
        let crashed = detect_crashed(&layout, &id).expect("ok");
        assert!(!crashed, "missing pid file → not crashed");
    }

    #[test]
    fn detect_crashed_live_pid_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "live");
        write_pid_file(&layout, &id, std::process::id() as i32);
        let crashed = detect_crashed(&layout, &id).expect("ok");
        assert!(!crashed, "own pid → alive → not crashed");
    }

    #[test]
    fn detect_crashed_dead_pid_is_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "dead");
        write_pid_file(&layout, &id, DEAD_PID);
        let crashed = detect_crashed(&layout, &id).expect("ok");
        assert!(crashed, "pid {DEAD_PID} → dead → crashed");
    }

    #[test]
    fn detect_crashed_malformed_pid_file_is_not_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "malform");
        let path = layout.pid_path(&id);
        StateLayout::ensure_dir_0700(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not-a-number").unwrap();
        let crashed = detect_crashed(&layout, &id).expect("ok");
        assert!(!crashed, "malformed pid → not crashed");
    }

    #[test]
    fn adjust_status_if_crashed_sets_phase_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "adj");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        let status = sample_status(&id, Phase::Running);
        write_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, DEAD_PID);

        let res = adjust_status_if_crashed(&layout, &id).expect("ok");
        assert_eq!(res, Some(Phase::Crashed));

        let after = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(after.phase, Phase::Crashed);
        // Non-phase fields are preserved.
        assert_eq!(after.last_event_summary, "before crash");
    }

    #[test]
    fn adjust_status_if_crashed_noop_when_alive() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "noop");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        let status = sample_status(&id, Phase::Running);
        write_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, std::process::id() as i32);

        let res = adjust_status_if_crashed(&layout, &id).expect("ok");
        assert_eq!(res, None);
        let after = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(after.phase, Phase::Running);
    }

    #[test]
    fn adjust_status_if_crashed_noop_for_terminal_phase() {
        // Terminal phases (Done / Failed / Crashed) must not be
        // re-adjusted even if the pid is dead — the process already
        // exited cleanly / was marked failed.
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "term");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        let status = sample_status(&id, Phase::Done);
        write_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, DEAD_PID);

        let res = adjust_status_if_crashed(&layout, &id).expect("ok");
        assert_eq!(res, None);
        let after = read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(after.phase, Phase::Done);
    }

    #[test]
    fn adjust_status_if_crashed_noop_when_no_status() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "missing");
        let res = adjust_status_if_crashed(&layout, &id).expect("ok");
        assert_eq!(res, None);
    }

    #[test]
    fn adjust_status_if_crashed_handles_reviewing_phase() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "rev");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();

        let status = sample_status(&id, Phase::Reviewing);
        write_status_atomic(&layout, &id, &status).unwrap();
        write_pid_file(&layout, &id, DEAD_PID);

        let res = adjust_status_if_crashed(&layout, &id).expect("ok");
        assert_eq!(res, Some(Phase::Crashed));
    }
}
