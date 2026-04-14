//! Tests that need to spawn `ark-supervisor-testhelper` as a subprocess.
//!
//! Lives under `tests/` (not `#[cfg(test)]` inside the lib) so that
//! cargo exposes `CARGO_BIN_EXE_ark-supervisor-testhelper` at compile
//! time for use with `env!(...)`, and so cargo builds the helper
//! binary before these tests run.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ark_supervisor::lock::{LockError, acquire_lock};
use ark_types::{AgentId, StateLayout};
use tempfile::tempdir;

fn helper_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ark-supervisor-testhelper"))
}

fn layout_at(base: &Path) -> StateLayout {
    StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
}

fn lock_args(mode: &str, layout: &StateLayout, id: &AgentId) -> Vec<String> {
    vec![
        mode.to_string(),
        layout.base().display().to_string(),
        layout.runtime().display().to_string(),
        layout.config().display().to_string(),
        id.as_str().to_string(),
    ]
}

fn spawn_lock_holder(layout: &StateLayout, id: &AgentId) -> Child {
    Command::new(helper_bin())
        .args(lock_args("lock-hold-and-sleep", layout, id))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lock holder")
}

fn run_lock_acquirer(layout: &StateLayout, id: &AgentId) -> i32 {
    Command::new(helper_bin())
        .args(lock_args("lock-acquire-and-exit", layout, id))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run lock acquirer")
        .code()
        .unwrap_or(-1)
}

fn read_pid(path: &Path) -> Option<i32> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    buf.trim().parse::<i32>().ok()
}

// ---- lock: cross-process ---------------------------------------------------

#[test]
fn lock_contention_from_other_process_returns_already_locked() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    let id = AgentId::new("cavekit", "contend");

    let mut child = spawn_lock_holder(&layout, &id);

    // Wait until the helper actually grabbed the lock — it writes its
    // pid to the lock file after the flock succeeds.
    let lock_file = layout.lock_path(&id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if read_pid(&lock_file).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        read_pid(&lock_file).is_some(),
        "helper never wrote pid to lock file"
    );

    // Parent-process acquire must see WouldBlock.
    let err = acquire_lock(&layout, &id).expect_err("parent should be blocked");
    match err {
        LockError::AlreadyLocked { existing_pid } => {
            assert!(
                existing_pid.is_some(),
                "pid file should carry child pid, got: {existing_pid:?}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }

    // Release the helper and wait.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "quit");
    }
    let _ = child.wait();
}

#[test]
fn lock_drop_releases_and_another_process_can_acquire() {
    let tmp = tempdir().expect("tempdir");
    let layout = layout_at(tmp.path());
    let id = AgentId::new("cavekit", "reacquire");

    // Hold-and-drop in this process.
    {
        let g = acquire_lock(&layout, &id).expect("parent acquire");
        drop(g);
    }

    // A child process should now be able to acquire cleanly.
    let code = run_lock_acquirer(&layout, &id);
    assert_eq!(code, 0, "child exit code: {code}");
}

// ---- daemon: setup_supervisor_log in a fresh subprocess -------------------

fn run_daemon_helper(log: &Path) -> std::process::ExitStatus {
    Command::new(helper_bin())
        .arg("daemon-setup-log")
        .arg(log)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn daemon helper")
}

#[test]
fn setup_supervisor_log_creates_parent_dirs_and_redirects_all_streams() {
    let tmp = tempdir().expect("tempdir");
    let log = tmp
        .path()
        .join("agents")
        .join("some-id")
        .join("supervisor.log");

    let status = run_daemon_helper(&log);
    assert!(status.success(), "helper exit: {status:?}");
    assert!(log.is_file(), "log file should exist: {}", log.display());

    let mut buf = String::new();
    File::open(&log)
        .expect("open log")
        .read_to_string(&mut buf)
        .expect("read log");
    assert!(
        buf.contains("stdout-line"),
        "expected stdout redirected to log, got: {buf:?}"
    );
    assert!(
        buf.contains("stderr-line"),
        "expected stderr redirected to log, got: {buf:?}"
    );
    assert!(
        buf.contains("tracing-line"),
        "expected tracing output in log, got: {buf:?}"
    );
}

#[test]
fn setup_supervisor_log_appends_across_runs() {
    let tmp = tempdir().expect("tempdir");
    let log = tmp.path().join("supervisor.log");

    let s1 = run_daemon_helper(&log);
    assert!(s1.success());
    let s2 = run_daemon_helper(&log);
    assert!(s2.success());

    let mut buf = String::new();
    File::open(&log)
        .expect("open log")
        .read_to_string(&mut buf)
        .expect("read log");
    let count = buf.matches("stdout-line").count();
    assert_eq!(count, 2, "expected 2 append markers, got log: {buf:?}");
}
