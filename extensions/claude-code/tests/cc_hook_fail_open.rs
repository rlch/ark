//! T-013 integration — `cc-hook` binary fail-open smoke tests.
//!
//! These spawn the actual `cc-hook` binary (built in the same
//! workspace via `env!("CARGO_BIN_EXE_cc-hook")`) against missing /
//! unreachable sockets and assert every path exits 0. Kit R2 is
//! load-bearing on this guarantee — a non-zero exit from cc-hook
//! blocks Claude Code's main loop, so the fail-open contract has to
//! stay covered at the binary boundary rather than the library layer.
//!
//! Note: the in-process unit tests in `bin/cc-hook/main.rs` cover the
//! function-level plumbing (`post_ndjson`, `parse_payload`, etc.). This
//! suite is the cross-process seal.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use tempfile::TempDir;

/// Path to the binary under test — populated by cargo for every
/// integration test target in the crate.
fn cc_hook_bin() -> PathBuf {
    // `env!` is OK here — `tests/` integration targets for a crate
    // with `[[bin]]` get this injected automatically.
    PathBuf::from(env!("CARGO_BIN_EXE_cc-hook"))
}

fn run_cc_hook(
    session: &str,
    socket: &std::path::Path,
    event: &str,
    stdin: &str,
) -> std::process::Output {
    let mut child = Command::new(cc_hook_bin())
        .args([
            "--session",
            session,
            "--socket",
            socket.to_str().unwrap(),
            "--event",
            event,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Silence the tracing init so test output stays clean.
        .env("RUST_LOG", "error")
        .spawn()
        .expect("spawn cc-hook");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait cc-hook")
}

#[test]
fn exits_zero_when_socket_file_missing() {
    let td = TempDir::new().unwrap();
    // Socket path points at a non-existent file inside an existing
    // directory. This is the "ark not running" scenario.
    let sock = td.path().join("cc-hook.sock");
    let out = run_cc_hook(
        "s1",
        &sock,
        "Stop",
        r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"Stop"}"#,
    );
    assert!(
        out.status.success(),
        "cc-hook must exit 0 on missing socket; got {:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exits_zero_when_parent_dir_missing() {
    let td = TempDir::new().unwrap();
    // Parent directory does NOT exist either — the "socket path points
    // into a dir that doesn't exist yet" scenario. Still fail-open.
    let sock = td.path().join("does/not/exist/cc-hook.sock");
    let out = run_cc_hook(
        "s1",
        &sock,
        "Stop",
        r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"Stop"}"#,
    );
    assert!(
        out.status.success(),
        "cc-hook must exit 0 on missing parent dir; got {:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exits_zero_with_empty_stdin() {
    // R2 fail-open: even a completely empty stdin (e.g. Claude Code
    // misfires the hook template) MUST exit 0. cc-hook synthesises a
    // placeholder payload in this branch.
    let td = TempDir::new().unwrap();
    let sock = td.path().join("nope.sock");
    let out = run_cc_hook("s1", &sock, "SessionStart", "");
    assert!(
        out.status.success(),
        "cc-hook must exit 0 on empty stdin; got {:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exits_zero_with_garbage_stdin() {
    let td = TempDir::new().unwrap();
    let sock = td.path().join("nope.sock");
    let out = run_cc_hook("s1", &sock, "Stop", "this is not json at all\n");
    assert!(
        out.status.success(),
        "cc-hook must exit 0 on garbage stdin; got {:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exits_zero_with_unparseable_cli_args() {
    // Even a bogus --event variant exits 0 (graceful-degradation
    // guard: a misconfigured settings.json entry MUST NOT wedge
    // claude). clap validation failure routes through the fail-open
    // branch in main.rs.
    let td = TempDir::new().unwrap();
    let sock = td.path().join("nope.sock");
    let out = Command::new(cc_hook_bin())
        .args([
            "--session",
            "s1",
            "--socket",
            sock.to_str().unwrap(),
            "--event",
            "NotARealHookName",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn cc-hook");
    assert!(
        out.status.success(),
        "cc-hook must exit 0 on bogus --event; got {:?}, stderr={:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}
