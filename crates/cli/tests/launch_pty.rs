//! Real-zellij smoke test for the bare-`ark` launch pipeline.
//!
//! Spawns the installed `ark` binary against a real zellij
//! installation via a pty, so every subsystem runs for real: scene
//! compile, supervisor fork, ready handshake, zellij layout parse,
//! session bring-up. The assertion is that zellij's `list-sessions`
//! eventually reports the session name we asked for — meaning our
//! compiled layout KDL passed zellij's parser and zellij accepted
//! the `-s <name>` invocation.
//!
//! Skipped cleanly when:
//! - `zellij` isn't on PATH (CI without zellij installed).
//! - We're already inside a zellij session (`$ZELLIJ` set). Nesting
//!   would confuse the assertions and pollute the caller's session.
//!
//! No `ARK_E2E=1` gate — this tier runs on every `cargo test` when
//! the environment supports it. That's the whole point of this
//! harness: the bug class that just bit us (compiled layout rejected
//! by real zellij) has to be visible to the default test command.
//!
//! Inherently flakier than the mock-based tests: real zellij, real
//! fork, real pty. The timeouts are generous (10 s for session
//! visibility) and teardown is belt-and-braces. If this test flakes
//! in CI, first suspect is timing — don't weaken the assertion.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn zellij_on_path() -> bool {
    Command::new("zellij")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn inside_zellij() -> bool {
    match std::env::var("ZELLIJ") {
        Ok(v) => !v.is_empty(),
        Err(_) => false,
    }
}

fn ark_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ark"))
}

/// Poll `zellij list-sessions` until `name` appears or `timeout`
/// elapses.
fn wait_for_session(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let out = Command::new("zellij").arg("list-sessions").output();
        if let Ok(o) = out {
            // `list-sessions` stdout is colorized; strip ANSI loosely by
            // discarding any byte outside printable range + newline.
            let text = String::from_utf8_lossy(&o.stdout);
            if text.lines().any(|line| line.contains(name)) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Best-effort zellij + child teardown. Never panics.
fn teardown(session_name: &str, child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    let _ = Command::new("zellij")
        .args(["kill-session", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    // Give the child a moment to wind down, then reap.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn real_zellij_accepts_compiled_default_layout() {
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH");
        return;
    }
    if inside_zellij() {
        eprintln!("SKIP: running inside zellij would nest clients");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let state = tmp.path().join("state");
    let config = tmp.path().join("config");
    // Runtime dir must stay under SUN_LEN (104 bytes on macOS) once
    // the `agents/<id>.sock` suffix is appended. `/tmp` stays short.
    let runtime = tempfile::Builder::new()
        .prefix("ark-pty-rt-")
        .tempdir_in("/tmp")
        .expect("runtime tempdir");

    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&config).unwrap();

    // Unique session name per pid/test so parallel runs and reruns
    // don't collide on the real zellij server's session namespace.
    let session_name = format!("ark-pty-test-{}", std::process::id());

    let pty_system = native_pty_system();
    let pty = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(ark_bin());
    cmd.arg("--session");
    cmd.arg(&session_name);
    cmd.env("ARK_STATE_DIR", state.display().to_string());
    cmd.env("ARK_CONFIG_DIR", config.display().to_string());
    cmd.env("ARK_RUNTIME_DIR", runtime.path().display().to_string());
    cmd.env("NO_COLOR", "1");
    cmd.env_remove("ZELLIJ");
    cmd.env_remove("ZELLIJ_PANE_ID");
    cmd.env_remove("ZELLIJ_SESSION_NAME");

    let mut child = pty.slave.spawn_command(cmd).expect("spawn ark in pty");
    // Drop the slave handle so the pty pair's master alone holds it.
    drop(pty.slave);

    // Ark launches zellij, which brings up the session. Wait up to
    // 10 s for the session name to show in `zellij list-sessions`.
    let appeared = wait_for_session(&session_name, Duration::from_secs(10));

    teardown(&session_name, &mut child);

    assert!(
        appeared,
        "real zellij must accept ark's compiled layout and create session `{session_name}` within 10s"
    );
}
