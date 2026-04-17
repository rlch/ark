//! Double-fork / setsid daemonize + stdio redirect + tracing install.
//!
//! See cavekit-supervisor.md R1 (all bullets except `--no-detach`, which
//! lives in T-063).
//!
//! # Pattern
//!
//! Classic POSIX daemon:
//!
//! 1. **First fork** — the original parent returns immediately (so the
//!    bare-ark CLI can print the agent id and exit).
//! 2. The child calls [`nix::unistd::setsid`] to detach from the
//!    controlling terminal and become a new session leader.
//! 3. **Second fork** — the session leader exits so the final grandchild
//!    can no longer re-acquire a TTY by accident.
//! 4. The grandchild redirects `stdin` to `/dev/null` (read-only),
//!    `stdout` + `stderr` to `$STATE/agents/{id}/supervisor.log` (append),
//!    and installs a [`tracing_subscriber`] fmt layer pointing at the
//!    same log file.
//!
//! The parent half of the call returns [`DaemonizeOutcome::Parent`]; the
//! grandchild returns [`DaemonizeOutcome::Daemon`]. Callers print the id
//! on the parent path and then call `std::process::exit(0)`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use ark_types::{AgentId, StateLayout};
use nix::unistd::{ForkResult, Pid, dup2, fork, setsid};
use thiserror::Error;

/// Result of a successful [`daemonize`] call.
///
/// The parent branch is returned to the bare-ark CLI — the caller
/// should print the agent id and then `std::process::exit(0)` so the
/// shell regains control immediately
/// (cavekit-supervisor R1: "parent CLI returns promptly <1s").
///
/// The daemon branch is returned inside the final grandchild — that's the
/// long-running supervisor process, with stdio redirected and a tracing
/// subscriber already installed.
#[derive(Debug)]
pub enum DaemonizeOutcome {
    /// Original CLI (bare-ark) process. Contains the PID of the immediate
    /// child — the session leader that itself will fork once more and
    /// exit. We expose it for observability (logging) only; the actual
    /// supervisor PID is written to `$STATE/agents/{id}/pid` by the
    /// daemon itself once R3 step 1 runs.
    Parent { child_pid: Pid },
    /// The final grandchild. Tracing subscriber installed; stdio
    /// redirected to `supervisor.log`. The caller owns the tokio runtime
    /// setup that follows.
    Daemon,
}

#[derive(Debug, Error)]
pub enum DaemonizeError {
    #[error("fork failed: {0}")]
    Fork(nix::Error),
    #[error("setsid failed: {0}")]
    Setsid(nix::Error),
    #[error("dup2 failed: {0}")]
    Dup2(nix::Error),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("tracing subscriber already installed")]
    TracingInit,
}

/// Double-fork the current process into a detached daemon.
///
/// See module docs for the pattern. Returns [`DaemonizeOutcome::Parent`]
/// in the original process and [`DaemonizeOutcome::Daemon`] in the
/// grandchild — both branches are reachable from the same return site.
///
/// The intermediate session-leader process does NOT return; it calls
/// `std::process::exit(0)` as soon as the second fork succeeds so it
/// can't accidentally continue executing caller code.
pub fn daemonize(
    state_layout: &StateLayout,
    agent_id: &AgentId,
) -> Result<DaemonizeOutcome, DaemonizeError> {
    let log_path = state_layout.supervisor_log_path(agent_id);

    // First fork — parent returns, child continues.
    // SAFETY: fork() on a single-threaded program is safe. Callers invoke
    // `daemonize` from the bare-ark launch path before any tokio runtime is
    // built, so we have not yet spawned helper threads. This matches the
    // cavekit contract: daemonize is the FIRST thing the CLI calls after
    // computing the spec.
    match unsafe { fork() }.map_err(DaemonizeError::Fork)? {
        ForkResult::Parent { child } => Ok(DaemonizeOutcome::Parent { child_pid: child }),
        ForkResult::Child => {
            // Session leader: detach from controlling terminal.
            setsid().map_err(DaemonizeError::Setsid)?;

            // Second fork — session leader exits, grandchild runs on.
            // SAFETY: same as above; we're still the only thread in this
            // process after the first fork cloned us.
            match unsafe { fork() }.map_err(DaemonizeError::Fork)? {
                ForkResult::Parent { .. } => {
                    // Session leader has done its job. Exit immediately
                    // so we cannot reacquire a TTY.
                    std::process::exit(0);
                }
                ForkResult::Child => {
                    // Final grandchild — set up the log file, redirect
                    // stdio, install tracing subscriber, return.
                    setup_supervisor_log(&log_path)?;
                    Ok(DaemonizeOutcome::Daemon)
                }
            }
        }
    }
}

/// Shared plumbing used by both [`daemonize`] and the forthcoming
/// `--no-detach` variant (T-063). Extracted as a free function so we can
/// unit test the stdio+tracing path without actually forking.
///
/// Steps:
/// 1. Ensure the parent directory of `log_path` exists.
/// 2. Open `log_path` in append mode (creating if absent).
/// 3. Redirect `stdin` to `/dev/null` (read-only), `stdout` + `stderr`
///    to the log file.
/// 4. Install a `tracing_subscriber` fmt layer writing to the log file.
///    The subscriber is installed as the global default — callers that
///    install their own subscriber must call this first, or not at all.
pub fn setup_supervisor_log(log_path: &Path) -> Result<(), DaemonizeError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    redirect_stdio(log_path, &log_file)?;

    // A fresh handle to the same file for tracing — we avoid reusing the
    // stdio handle so that log rotation / truncation semantics are
    // independent.
    let tracing_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || -> Box<dyn io::Write + Send> {
            Box::new(
                tracing_file
                    .try_clone()
                    .expect("clone supervisor log handle"),
            )
        })
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .finish();

    // Ignore "global subscriber already installed" — the grandchild
    // inherits the parent CLI's subscriber via `fork(2)`, which CANNOT
    // be replaced. The existing subscriber writes to fd 2, and we just
    // dup2'd fd 2 to the log file in `redirect_stdio` above, so all
    // emitted spans end up in the same `supervisor.log` we'd have
    // opened ourselves. Surfacing this as a fatal `TracingInit` error
    // bricks the entire daemonize chain (spawn.rs's failure path then
    // cleans up the agent dir, which masquerades as "supervisor failed
    // to ready"). See the W-3 / W-8 debug session and F-740.
    let _ = tracing::subscriber::set_global_default(subscriber);

    Ok(())
}

fn redirect_stdio(_log_path: &Path, log_file: &File) -> Result<(), DaemonizeError> {
    let devnull_read = OpenOptions::new().read(true).open("/dev/null")?;

    // stdin ← /dev/null
    dup2(devnull_read.as_raw_fd(), 0).map_err(DaemonizeError::Dup2)?;
    // stdout ← supervisor.log
    dup2(log_file.as_raw_fd(), 1).map_err(DaemonizeError::Dup2)?;
    // stderr ← supervisor.log
    dup2(log_file.as_raw_fd(), 2).map_err(DaemonizeError::Dup2)?;

    Ok(())
}

/// Convenience that mirrors [`setup_supervisor_log`] but takes a
/// [`StateLayout`] + [`AgentId`] pair. Unused by the fork path (it uses
/// the private helpers directly) but handy for T-063's `--no-detach`
/// future work which needs to redirect without forking.
pub fn supervisor_log_for(state_layout: &StateLayout, agent_id: &AgentId) -> PathBuf {
    state_layout.supervisor_log_path(agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // In-process tests are limited to things that don't actually dup2()
    // stdio (that would break the test harness). Subprocess tests for
    // `setup_supervisor_log` live in `tests/subprocess_tests.rs`.

    #[test]
    fn supervisor_log_for_matches_state_layout() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = AgentId::new("cavekit", "auth");
        assert_eq!(
            supervisor_log_for(&layout, &id),
            layout.supervisor_log_path(&id)
        );
    }

    /// Full fork path exercised only when explicitly requested — cargo
    /// test harness does not cope well with child processes writing to
    /// captured stdout, and the double-fork leaves a detached grandchild
    /// that outlives the test binary.
    #[test]
    #[ignore = "forks real processes"]
    fn daemonize_double_forks_and_writes_log() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = AgentId::new("cavekit", "test");
        let outcome = daemonize(&layout, &id).expect("daemonize");
        match outcome {
            DaemonizeOutcome::Parent { .. } => {
                // Let the grandchild run a moment, then exit.
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            DaemonizeOutcome::Daemon => {
                tracing::info!("hello from grandchild");
                std::process::exit(0);
            }
        }
    }
}
