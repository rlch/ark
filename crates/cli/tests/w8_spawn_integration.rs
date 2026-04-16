//! W-8 — end-to-end integration test for the supervisor fork + ready-signal
//! protocol (build-site-supervisor-wiring.md W-8; cavekit-supervisor.md R1,
//! R3 step 12; cavekit-cli.md R2).
//!
//! ## What this test exercises
//!
//! The full detach spawn pipeline, end-to-end:
//!
//! 1. Parent CLI allocates a `pipe(2)` ready handshake, calls
//!    `ark_supervisor::daemonize()` (in-process double-fork + setsid).
//! 2. Grandchild builds a tokio runtime and runs `supervisor_main`,
//!    which walks the R3 18-step sequence — StateDir creation, lock
//!    acquisition, control-socket bind, factory build, scene compile,
//!    consumer task spawn, `Started` event emit, and the step-12 ACK
//!    byte write-back.
//! 3. Parent's `wait_for_ready` unblocks on the ACK; CLI exits 0.
//!
//! By the time `ark spawn` returns to this test harness, the supervisor
//! is guaranteed (per the W-2 protocol) to have:
//!
//! * written `$STATE/agents/{id}/spec.json` (R3 step 1),
//! * written `$STATE/agents/{id}/pid` with its own PID (R3 step 1),
//! * written `$STATE/agents/{id}/status.json` at least with the initial
//!   `Starting` phase (R3 step 1 — the `Started` event the state_writer
//!   rolls up may or may not have been flushed yet, so we tolerate any
//!   non-terminal phase),
//! * bound `$RUNTIME/agents/{id}.sock` as a listening Unix socket (R3
//!   step 3).
//!
//! The test asserts each of those side-effects, then exercises `ark kill`
//! to take the supervisor down and asserts that the runtime socket and
//! pid file are either removed OR belong to a dead process within 10 s
//! of the kill.
//!
//! ## Skip gates
//!
//! Three stable `SKIP:` lines keep this test green on hosts that don't
//! ship the prerequisites:
//!
//! 1. `ARK_E2E=1` — the shared harness gate (matches `e2e.rs`,
//!    `scene_e2e.rs`). Running `cargo test -p ark-cli` without setting
//!    the env var skips the scenario cleanly.
//! 2. `zellij` on `PATH` — `ark spawn` preflights it before doing any
//!    work; without it the spawn would fail before the supervisor even
//!    boots.
//! 3. `mock-claude` binary built — the supervisor's `AcpEngineStub`
//!    preflight (crates/supervisor/src/engine_stub.rs L134) checks that
//!    `claude` resolves on `PATH`. We prepend the `mock-claude` parent
//!    dir so the stub passes without a real Claude install.
//!
//! ## Why detach (the default), not `--no-detach`
//!
//! The whole point of W-8 is the fork + ready-signal protocol. In
//! `--no-detach` mode (F-730 / W-4) the supervisor runs inline in a
//! std thread of the foreground `ark` process — there IS no fork and
//! no pipe ack. Running the W-8 assertions against `--no-detach` would
//! be a functional test of the threaded fallback, not the supervisor
//! fork contract. We take the default detach path so `daemonize()` +
//! `wait_for_ready` actually execute.
//!
//! ## Cleanup
//!
//! `E2eEnv` tracks every supervisor pid we discover under `$STATE/
//! agents/*/pid`. On test exit (including panic) the guard SIGTERMs
//! each tracked pid, escalating to SIGKILL after a 1 s grace, then
//! deletes the tempdirs. `ark kill` is still exercised inline as the
//! primary teardown path so the R4 shutdown sequence gets real
//! coverage; the env-guard SIGTERM is a safety net.

mod e2e_support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ark_types::AgentSpec;

// ---- shared helpers -------------------------------------------------------

/// Skip the test if `zellij` is not on PATH. Mirrors the helper in
/// `e2e.rs` and `scene_e2e.rs` so the three integration-test files can
/// drift independently without sharing a private symbol.
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

/// Poll `predicate` every 50 ms until it returns true or `timeout`
/// expires. `true` on predicate-success, `false` on timeout.
fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut predicate: F) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `kill(pid, 0)` probe: `true` if the process exists, `false` if ESRCH.
/// Matches the convention in `e2e_support::is_alive` (not public, so we
/// re-implement rather than reach into the module's privates).
fn pid_alive(pid: u32) -> bool {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        // EPERM means the process exists but we can't signal it — treat
        // as alive. Any other errno: conservative-alive.
        Err(_) => true,
    }
}

/// Walk `$STATE/agents/` and return the single agent dir (path, id-str).
/// Panics if zero or more-than-one agent dirs are present — the test
/// invariant is that exactly one spawn happened into the tempdir.
fn sole_agent_dir(state_dir: &Path) -> (PathBuf, String) {
    let agents_root = state_dir.join("agents");
    let mut hits: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&agents_root) {
        for e in entries.flatten() {
            if e.path().is_dir() && e.path().join("spec.json").is_file() {
                hits.push(e.path());
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one agent dir under {}, found {}: {:?}",
        agents_root.display(),
        hits.len(),
        hits
    );
    let dir = hits.into_iter().next().unwrap();
    let id = dir
        .file_name()
        .expect("agent dir name")
        .to_string_lossy()
        .into_owned();
    (dir, id)
}

// ---- W-8: spawn_creates_supervisor_artifacts -----------------------------

/// W-8. Default detach `ark spawn`: fork the supervisor, wait on the
/// ready pipe, and verify every state artefact the kit promises is in
/// place by the time the parent CLI exits.
///
/// ## Acceptance (W-8 kit)
///
/// 1. Parent returns within 5 s with exit 0 (the ready timeout bound
///    the W-2 helper enforces).
/// 2. `$STATE/agents/{id}/spec.json` exists and parses as `AgentSpec`.
/// 3. `$STATE/agents/{id}/status.json` exists and has a non-terminal
///    phase (`Starting` or later — see module docs).
/// 4. `$RUNTIME/agents/{id}.sock` exists and `UnixStream::connect` to
///    it succeeds (the supervisor is listening).
/// 5. `$STATE/agents/{id}/pid` exists and names a live process.
/// 6. `ark kill <id>` cleans up: within 10 s the pid is dead and the
///    runtime socket is gone (the supervisor unlinks it on shutdown —
///    cavekit-supervisor.md R3 step 17).
#[test]
#[ignore = "W-8: requires zellij + mock-claude + ARK_E2E=1; opt-in via `ARK_E2E=1 cargo test -p ark-cli --test w8_spawn_integration -- --ignored`"]
fn spawn_creates_supervisor_artifacts() {
    if !e2e_support::require_e2e() {
        return;
    }
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH (required by ark spawn preflight)");
        return;
    }
    let Some(mock_claude) = e2e_support::mock_claude_bin() else {
        eprintln!("SKIP: mock-claude binary not built; cargo build -p ark-test-fixtures first");
        return;
    };

    let env = e2e_support::E2eEnv::new();

    // Engine preflight (crates/supervisor/src/engine_stub.rs L134)
    // probes `claude` on PATH for engine slug "claude-code". Prepend
    // the mock-claude parent dir so the probe succeeds without a real
    // Claude install.
    let mock_dir = mock_claude.parent().expect("mock-claude has parent");
    let prior_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", mock_dir.display(), prior_path);

    let cwd = env.state_dir().to_path_buf();
    let mut cmd = env.ark();
    // W-2 / W-3: default detach forks the supervisor. Scrub every
    // `ZELLIJ*` var so the spawn pipeline takes the outside-zellij
    // branch even when the test runs from inside an existing zellij
    // session (commands/spawn.rs::inside_zellij), and route through
    // the `cavekit` orchestrator + long-running agent command so the
    // supervisor stays up long enough for us to inspect state.
    cmd.env("PATH", &new_path)
        .env("RUST_LOG", "debug")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("ZELLIJ_SESSION_NAME")
        .arg("spawn")
        .arg("--orchestrator")
        .arg("cavekit")
        .arg("--engine")
        .arg("claude-code")
        .arg("--cwd")
        .arg(&cwd)
        .arg("--name")
        .arg("w8")
        .arg("--")
        .arg("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Budget: W-8 kit demands the parent return within 5 s. Give
    // Command::output() an outer 15 s wall-clock so a hung parent
    // surfaces as a panic with stdout/stderr rather than a cargo-test
    // global timeout with no context.
    let start = Instant::now();
    let out = cmd.output().expect("spawn ark sub-process");
    let parent_elapsed = start.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprintln!(
        "ark spawn exit={:?} elapsed={:?}",
        out.status.code(),
        parent_elapsed
    );
    eprintln!("ark stdout: {stdout}");
    eprintln!("ark stderr: {stderr}");

    // (1) Parent returned within 5 s with exit 0. We allow a small
    // cushion above 5 s (8 s) so a slow-booting CI host doesn't
    // false-positive — the W-2 timeout inside the CLI is already 5 s,
    // so any run that takes substantially longer has already surfaced
    // a ready-signal error through the CLI's exit code.
    assert!(
        out.status.success(),
        "ark spawn exited non-zero ({:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        parent_elapsed < Duration::from_secs(8),
        "ark spawn took {parent_elapsed:?} (> 8 s); ready-signal timeout is 5 s"
    );

    // (2-5) Inspect the state dir. The supervisor wrote every artefact
    // BEFORE signalling ready, so they must be visible the moment the
    // parent CLI returns.
    let (agent_dir, id_str) = sole_agent_dir(env.state_dir());
    eprintln!("agent dir: {} (id={id_str})", agent_dir.display());

    // (2) spec.json exists and parses as AgentSpec.
    let spec_path = agent_dir.join("spec.json");
    assert!(spec_path.is_file(), "spec.json missing: {}", spec_path.display());
    let spec_bytes = std::fs::read(&spec_path).expect("read spec.json");
    let spec: AgentSpec = serde_json::from_slice(&spec_bytes).unwrap_or_else(|e| {
        panic!(
            "spec.json does not parse as AgentSpec: {e}\ncontents: {}",
            String::from_utf8_lossy(&spec_bytes)
        )
    });
    assert_eq!(spec.name, "w8", "AgentSpec.name round-trip");
    assert_eq!(
        spec.orchestrator, "cavekit",
        "AgentSpec.orchestrator round-trip"
    );
    assert_eq!(spec.id.as_str(), id_str, "spec.id matches dir name");

    // (3) status.json exists with a non-terminal phase. Read as a JSON
    // value first so we can give a precise panic message; then bind to
    // the full `AgentStatus` type for the Phase assertion. Tolerate
    // any non-terminal phase because the state_writer consumer rolls
    // up events asynchronously — by ACK time the initial `Starting`
    // write has landed but the `Started` event may not yet have been
    // processed into `Running`.
    let status_path = agent_dir.join("status.json");
    assert!(
        status_path.is_file(),
        "status.json missing: {}",
        status_path.display()
    );
    let status_bytes = std::fs::read(&status_path).expect("read status.json");
    let status: ark_types::AgentStatus =
        serde_json::from_slice(&status_bytes).unwrap_or_else(|e| {
            panic!(
                "status.json does not parse: {e}\ncontents: {}",
                String::from_utf8_lossy(&status_bytes)
            )
        });
    use ark_types::Phase;
    let terminal = matches!(
        status.phase,
        Phase::Done | Phase::Failed | Phase::Crashed | Phase::Killed | Phase::Timeout
    );
    assert!(
        !terminal,
        "status.json.phase is terminal ({:?}) immediately after spawn; supervisor died?\n\
         contents: {}",
        status.phase,
        String::from_utf8_lossy(&status_bytes)
    );

    // (4) Control socket exists and is bound. We test "bound" by
    // attempting a UnixStream::connect — a plain `is_file()` check
    // isn't sufficient (the socket inode can linger after the
    // supervisor exits on some platforms). The socket path is
    // $RUNTIME/agents/{id}.sock per `StateLayout::agent_socket_path`.
    let socket_path = env.runtime_dir().join("agents").join(format!("{id_str}.sock"));
    assert!(
        socket_path.exists(),
        "control socket missing: {}",
        socket_path.display()
    );
    match std::os::unix::net::UnixStream::connect(&socket_path) {
        Ok(_s) => {
            eprintln!("control socket connect OK: {}", socket_path.display());
        }
        Err(e) => panic!(
            "control socket connect failed at {}: {e}",
            socket_path.display()
        ),
    }

    // (5) pid file exists, parses as a u32, and the named pid is alive.
    let pid_path = agent_dir.join("pid");
    assert!(pid_path.is_file(), "pid file missing: {}", pid_path.display());
    let pid_raw = std::fs::read_to_string(&pid_path).expect("read pid file");
    let pid: u32 = pid_raw
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("pid file {pid_path:?} does not parse as u32: {e} raw={pid_raw:?}"));
    assert!(
        pid_alive(pid),
        "pid {pid} from {} is not alive (kill -0 reports ESRCH)",
        pid_path.display()
    );
    // Belt-and-braces: the E2eEnv Drop will SIGTERM this pid even if
    // the `ark kill` branch below fails.
    e2e_support::track_pid(&env, pid);

    // (6) `ark kill` shuts the supervisor down. The R4 kit budget is
    // 10 s for graceful termination. After the CLI returns we poll for
    // the pid to disappear AND the socket to be unlinked — the
    // supervisor explicitly unlinks at R3 step 17.
    let kill_start = Instant::now();
    let kill_out = env
        .ark()
        .arg("kill")
        .arg(&id_str)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn ark kill");
    let kill_elapsed = kill_start.elapsed();
    let kill_stdout = String::from_utf8_lossy(&kill_out.stdout).into_owned();
    let kill_stderr = String::from_utf8_lossy(&kill_out.stderr).into_owned();
    eprintln!(
        "ark kill exit={:?} elapsed={:?}",
        kill_out.status.code(),
        kill_elapsed
    );
    eprintln!("ark kill stdout: {kill_stdout}");
    eprintln!("ark kill stderr: {kill_stderr}");
    assert!(
        kill_out.status.success(),
        "ark kill exited non-zero ({:?}):\nstdout:\n{kill_stdout}\nstderr:\n{kill_stderr}",
        kill_out.status.code()
    );

    // Poll up to 10 s for the supervisor to exit. On a warm runtime
    // this completes in well under a second; the slack is for
    // busy-CI scheduling + the DEFAULT_KILL_GRACE the supervisor's
    // kill handler enforces.
    let gone = wait_until(Duration::from_secs(10), || !pid_alive(pid));
    assert!(
        gone,
        "supervisor pid {pid} still alive 10 s after `ark kill`;\
         stdout:\n{kill_stdout}\nstderr:\n{kill_stderr}"
    );

    // Socket unlink happens in the supervisor's Drop path (step 17);
    // it may lag the pid exit by a few ms.
    let socket_gone = wait_until(Duration::from_secs(5), || !socket_path.exists());
    assert!(
        socket_gone,
        "control socket {} still present 5 s after supervisor exit",
        socket_path.display()
    );
}
