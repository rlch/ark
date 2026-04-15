//! T-8.3 + T-8.4 — scene-driven end-to-end tests
//! (cavekit-scene.md R4 reactions, R5 keybinds, R7 op vocabulary).
//!
//! Two scenarios live in this file:
//!
//! | # | Scenario                       | Cavekit ref | Notes |
//! |---|--------------------------------|-------------|-------|
//! | 1 | `scene_reactions_fire`         | R4 + R7 #12 | filesystem-observability — `on "Started" { exec script="touch <tmp>/fired" }` |
//! | 2 | `scene_keybind_dispatches`     | R5          | `zellij action message-plugin ark-bus --name ark-intent --payload '…'` round-trips through ark-bus → ark-hook → supervisor → mux |
//!
//! Both scenarios spawn a real `ark` binary against a tempdir bundle
//! ([`e2e_support::E2eEnv`]) and a freshly-written scene fixture under
//! `${ARK_CONFIG_DIR}/scenes/<name>.kdl` — the path the spawn pipeline
//! resolves when `--scene NAME` is passed (T-8.2 / R13 path precedence).
//!
//! ## Skip gates
//!
//! 1. `ARK_E2E=1` — the existing e2e harness gate (shared with `e2e.rs`).
//! 2. `zellij` on `PATH` — `ark spawn` preflights it; without the binary
//!    the spawn would fail before reactions could fire.
//! 3. (T-8.4 only) the `mock-claude` binary — used as the agent command
//!    so the engine preflight (`claude` on PATH) succeeds without a real
//!    Claude install.
//!
//! Each gate emits a stable `SKIP:` line on stderr and early-returns,
//! matching the convention used in `e2e.rs`.
//!
//! ## Tempdir + cleanup
//!
//! Every test owns an [`e2e_support::E2eEnv`]; the guard's Drop signals
//! tracked pids and restores `ARK_*_DIR` env vars even on panic. Spawned
//! `ark` child processes are tracked via `env.track_pid()` so a wedged
//! supervisor cannot leak across tests.
//!
//! ## Why `--no-detach` for T-8.3
//!
//! In `--no-detach` mode (W-4 / F-730) the supervisor runs inline in a
//! background thread of the foreground `ark` process; `Started` fires
//! within tens of milliseconds of the supervisor binding its control
//! socket, well within the 2 s deadline the kit calls out. The
//! foreground ark stays attached to the test process's TTY and only
//! exits when the launched zellij child exits — we kill the whole
//! process group at the end of the test.

mod e2e_support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---- shared helpers -------------------------------------------------------

/// Skip the test if `zellij` is not on PATH. `ark spawn` preflights it
/// before doing any other work; without it both scenarios are dead on
/// arrival. Mirrors the helper in `e2e.rs` so the two files can drift
/// independently without tripping over a shared symbol.
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

/// Write a scene fixture under `${config_dir}/scenes/<name>.kdl` so
/// `ark spawn --scene NAME` resolves to it (combo 3A in
/// `commands/spawn.rs::resolve_layout_source`). Returns the absolute
/// path to the written fixture for diagnostic logging.
fn write_scene_fixture(config_dir: &Path, name: &str, kdl: &str) -> PathBuf {
    let scenes_dir = config_dir.join("scenes");
    std::fs::create_dir_all(&scenes_dir).expect("create scenes dir");
    let path = scenes_dir.join(format!("{name}.kdl"));
    std::fs::write(&path, kdl).expect("write scene fixture");
    path
}

/// Poll `predicate` every 50 ms until it returns true or `timeout`
/// expires. Returns `true` on success, `false` on timeout. Used by both
/// scenarios so the wait loops stay uniform.
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

/// Best-effort SIGTERM-then-SIGKILL of a tracked pid. Mirrors the Drop
/// path inside `E2eEnv`, but exposed here so each scenario can take down
/// its `ark` child the moment its assertion has succeeded — keeps test
/// runtime well under the 2 s deadline even on a slow CI host.
fn kill_pid_graceful(pid: u32) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    );
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

// ---- T-8.3: scene_reactions_fire -----------------------------------------

/// T-8.3. Fixture scene declares one reaction:
///
/// ```kdl
/// on "Started" { exec script="touch <fired-path>" }
/// ```
///
/// The supervisor compiles the scene at boot (R3 step 7), populates the
/// reaction registry, and emits `Started` after the always-on plugin
/// mount pass. The reaction dispatcher consumes the event and runs the
/// `exec` op, which spawns `sh -c "touch <fired-path>"` against the
/// supervisor's tokio runtime. Pure filesystem observability — no
/// status-bar / pipe roundtrip — so the assertion is decoupled from the
/// ark-bus / zellij plugin chain.
///
/// ## Status
///
/// `#[ignore]`d in v0.1: the scene reaction dispatcher fires reactions
/// against the matching selector (we observed `selector=Started …
/// status="ok"` in `supervisor.log` end-to-end), but `ops_run` is
/// **always 0** because `crates/scene/src/reactions.rs::op_node_to_compiled`
/// is a documented stub that returns `None` for every op until T-3.2
/// unifies the typed-AST branch with `OpNode`. Until then, the reaction
/// fires but produces no side effect — the marker file never lands.
///
/// The test infrastructure is otherwise complete: scene compile,
/// registry population, dispatcher subscription, Started broadcast, CEL
/// evaluation, telemetry. Run via
/// `cargo test -p ark-cli --test scene_e2e -- --ignored
/// scene_reactions_fire` once T-3.2 lifts the stub, and the assertion
/// becomes meaningful.
///
/// ## Why detach mode (default), not `--no-detach`
///
/// `--no-detach` is documented in the kit, but in v0.1 the supervisor
/// runs in a background std thread of the foreground `ark` process and
/// the parent thread spawns zellij + waits. There is NO ready handshake
/// in that mode; if zellij dies fast (which it does in CI without a
/// real TTY), the parent fires `cancel.cancel()` before the supervisor
/// has reliably reached step 11 (Started). The race makes the reaction
/// effectively unobservable in CI.
///
/// Detach mode (the default) writes a ready-pipe ack at R3 step 12,
/// AFTER Started has fired at step 11 — so by the time `ark spawn`
/// returns to the test, the reaction has already dispatched. zellij's
/// own success/failure becomes irrelevant.
///
/// We need `mock-claude` on PATH to pass the engine preflight; the
/// helper from `e2e_support` locates the sibling binary that
/// `ark-test-fixtures` builds.
#[test]
#[ignore = "T-8.3: scene-reaction op execution gated on T-3.2 (op_node_to_compiled stub returns None for every op in v0.1; reactions fire but produce no side effect)"]
fn scene_reactions_fire() {
    if !e2e_support::require_e2e() {
        return;
    }
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH (required by ark spawn preflight)");
        return;
    }
    let Some(mock_claude) = e2e_support::mock_claude_bin() else {
        eprintln!(
            "SKIP: mock-claude binary not built; cargo build -p ark-test-fixtures first"
        );
        return;
    };

    let env = e2e_support::E2eEnv::new();

    // Marker path the reaction will `touch`. Keep it under the runtime
    // tempdir so the E2eEnv Drop sweeps it on teardown.
    let fired = env.runtime_dir().join("fired");
    let fired_str = fired.display().to_string();

    // Minimal scene: an empty `layout { }` block (required by
    // `compile_scene_file` — see crates/scene/src/compile/mod.rs L82-89)
    // and a single reaction that `touch`es the marker on `Started`.
    //
    // The scene intentionally declares NO `tab`/`pane` children. The
    // kdl 6.5.0 builder used by `compile_layout` emits unquoted bare
    // identifiers for entry values like `tab "agent"` → `tab agent`,
    // which zellij's stricter layout-KDL parser rejects
    // (TODO(T-14.x): force-quote string entries in
    // `crates/scene/src/compile/layout.rs::lower_tab/lower_pane`). An
    // empty layout produces `layout { }`, which zellij accepts; this
    // assertion only cares about the reaction firing, not about what
    // zellij paints.
    let scene_kdl = format!(
        r#"scene "react" {{
    layout {{ }}

    on "Started" {{
        exec script="touch {fired}"
    }}
}}
"#,
        fired = fired_str
    );
    let scene_path = write_scene_fixture(env.config_dir(), "react", &scene_kdl);
    eprintln!("scene fixture written: {}", scene_path.display());

    // Prepend mock-claude's parent dir to PATH so the engine preflight
    // (`claude` on PATH) passes — same trick as
    // `scenario_spawn_supervisor_lives_then_dies` in `e2e.rs`.
    //
    // Strip every `ZELLIJ*` env var inherited from the test runner —
    // when the test is invoked from inside an existing zellij session
    // (the developer's outer shell) `ark spawn` would otherwise take
    // the inside-zellij `switch-session` branch (commands/spawn.rs L1183)
    // instead of forking a new server, and we genuinely need a fresh
    // session for the supervisor to spawn.
    let mock_dir = mock_claude.parent().expect("mock-claude has parent");
    let prior_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", mock_dir.display(), prior_path);

    let cwd = env.state_dir().to_path_buf();
    let mut cmd = env.ark();
    cmd.env("PATH", &new_path)
        // Surface debug-level supervisor / scene tracing into
        // supervisor.log on the agent state dir; we replay it on
        // assertion failure for triage.
        .env("RUST_LOG", "debug")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("ZELLIJ_SESSION_NAME")
        .arg("spawn")
        .arg("--orchestrator")
        .arg("claude-code")
        .arg("--cwd")
        .arg(&cwd)
        .arg("--name")
        .arg("react")
        .arg("--scene")
        .arg("react")
        // Default detach mode. The parent CLI returns only after the
        // supervisor has bound its socket AND emitted Started — so the
        // reaction has already had a chance to run by the time `output()`
        // returns to us.
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = cmd.output().expect("spawn ark");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    eprintln!("ark spawn exit={:?}", out.status.code());
    eprintln!("ark stdout: {stdout}");
    eprintln!("ark stderr: {stderr}");

    // Locate the supervisor pid so we can clean it up after the
    // assertion. The supervisor wrote `${state_dir}/agents/<id>/pid`
    // before signalling ready. Failure to read the pid is non-fatal —
    // the supervisor will exit on its own when the test process dies.
    let agents_root = env.state_dir().join("agents");
    let mut supervisor_pid: Option<u32> = None;
    if let Ok(entries) = std::fs::read_dir(&agents_root) {
        for e in entries.flatten() {
            let pid_path = e.path().join("pid");
            if let Ok(raw) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    e2e_support::track_pid(&env, pid);
                    supervisor_pid = Some(pid);
                    break;
                }
            }
        }
    }

    // 2 s deadline (R4: reactions should fire well under a second; the
    // extra slack covers CI jitter + the supervisor's cold-start tokio
    // runtime + the `exec` op spawning a sh subprocess).
    let appeared = wait_until(Duration::from_secs(2), || fired.is_file());

    // Drain the supervisor's tracing log (written to the agent state
    // dir in detach mode) BEFORE killing the supervisor — we want to
    // attach it to the panic message on failure for triage.
    let supervisor_log = if let Ok(entries) = std::fs::read_dir(&agents_root) {
        let mut buf = String::new();
        for e in entries.flatten() {
            let log = e.path().join("supervisor.log");
            if let Ok(contents) = std::fs::read_to_string(&log) {
                buf.push_str(&format!("\n--- {} ---\n", log.display()));
                buf.push_str(&contents);
            }
        }
        buf
    } else {
        String::new()
    };

    // Tear down before assert so cargo test doesn't wait on /bin/sleep.
    if let Some(pid) = supervisor_pid {
        kill_pid_graceful(pid);
    }

    assert!(
        appeared,
        "reaction never fired: marker {} did not appear within 2s\n\
         --- ark stdout ---\n{stdout}\n\
         --- ark stderr ---\n{stderr}\n\
         --- supervisor log ---{supervisor_log}",
        fired.display(),
    );
}

// ---- T-8.4: scene_keybind_dispatches -------------------------------------

/// T-8.4. Fixture scene declares one keybind:
///
/// ```kdl
/// keybind "Alt q" intent="ark.core.close_tab" name="builder"
/// ```
///
/// The compile pipeline auto-injects `plugin "ark-bus" { mount "hidden" }`
/// because the scene declares a keybind (T-6.7). We bypass the actual
/// key-press by sending the intent envelope directly through zellij's
/// `action message-plugin` IPC — exactly what a real key-press would
/// do — and assert the named tab disappears within 2 s via
/// `zellij action list-tabs --output-json`.
///
/// Status: the dispatch chain depends on the `ark-hook` binary being on
/// `PATH` inside the zellij subprocess (ark-bus's `dispatch_intent`
/// shells out to `ark-hook intent --json '<payload>'` via a hidden
/// command pane — see `crates/plugins/ark-bus/src/lib.rs` L437-449).
/// Until T-14.x makes that wiring deterministic on a stripped CI host,
/// the test is `#[ignore]`d by default — run it via
/// `cargo test -p ark-cli --test scene_e2e -- --ignored
/// scene_keybind_dispatches` after `cargo build -p ark-cli` produces
/// `ark-hook` next to `ark`.
#[test]
#[ignore = "T-8.4: requires ark-hook on PATH inside the zellij subprocess; gated until T-14.x firms up the per-session PATH plumbing"]
fn scene_keybind_dispatches() {
    if !e2e_support::require_e2e() {
        return;
    }
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH (required by ark spawn preflight)");
        return;
    }

    let env = e2e_support::E2eEnv::new();

    // Two-tab scene: `agent` is the long-running pane that holds zellij
    // open, `builder` is the disposable target whose disappearance the
    // assertion key off. The keybind target intent
    // `ark.core.close_tab` matches the op registered in
    // `crates/scene/src/ops/tabs.rs` (R7 #2).
    let scene_kdl = r#"scene "keybind" {
    layout {
        tab name="agent" {
            pane name="agent" {
                command "/bin/sleep"
            }
        }
        tab name="builder" {
            pane name="builder" {
                command "/bin/sleep"
            }
        }
    }

    keybind "Alt q" intent="ark.core.close_tab" name="builder"
}
"#;
    let scene_path = write_scene_fixture(env.config_dir(), "keybind", scene_kdl);
    eprintln!("scene fixture written: {}", scene_path.display());

    // Spawn ark in --no-detach mode so the supervisor (which owns the
    // intent dispatcher) lives inside the foreground ark process — same
    // rationale as T-8.3.
    let cwd = env.state_dir().to_path_buf();
    let mut cmd = env.ark();
    cmd.arg("spawn")
        .arg("--orchestrator")
        .arg("claude-code")
        .arg("--cwd")
        .arg(&cwd)
        .arg("--name")
        .arg("keybind")
        .arg("--scene")
        .arg("keybind")
        .arg("--no-detach")
        .arg("--")
        .arg("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = cmd.spawn().expect("spawn ark");
    let pid = child.id();
    env.track_pid(pid);

    // Discover the actual zellij session name. `ark spawn` derives it
    // from `unique_session_name(spec.id)` (`{base}-{ulid8}`); the
    // simplest discovery is to wait until exactly one ark-... session
    // shows up in `zellij list-sessions`.
    let session = wait_until_session(Duration::from_secs(3))
        .expect("zellij never reported the new session within 3s");
    eprintln!("discovered session: {session}");

    // Wait for both tabs to materialise so we know the dispatch target
    // exists before we ask zellij to close it.
    let initial_tabs = wait_until_tabs(&session, Duration::from_secs(3), |tabs| {
        tabs.iter().any(|t| t == "builder") && tabs.iter().any(|t| t == "agent")
    });
    assert!(
        initial_tabs,
        "expected both `agent` and `builder` tabs to come up; \
         dispatch test is meaningless without the target tab"
    );

    // Send the intent via zellij's IPC. Mirrors the keybind path: a
    // real Alt+q would post `MessagePlugin "ark-bus" { name "ark-intent";
    // payload "<JSON>" }`; we post the same thing manually.
    let payload = r#"{"intent":"ark.core.close_tab","args":{"name":"builder"}}"#;
    let dispatch = Command::new("zellij")
        .arg("--session")
        .arg(&session)
        .arg("action")
        .arg("message-plugin")
        .arg("ark-bus")
        .arg("--name")
        .arg("ark-intent")
        .arg("--payload")
        .arg(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .expect("invoke zellij action message-plugin");
    assert!(
        dispatch.success(),
        "zellij action message-plugin exited non-zero: {:?}",
        dispatch.code()
    );

    // Assert the `builder` tab is gone within 2 s.
    let closed = wait_until_tabs(&session, Duration::from_secs(2), |tabs| {
        !tabs.iter().any(|t| t == "builder")
    });

    kill_pid_graceful(pid);

    assert!(
        closed,
        "tab `builder` did not close within 2s of dispatch; \
         dispatch chain is wedged somewhere between ark-bus and the mux"
    );
}

/// T-8.4 pre-flight: the keybind-dispatch e2e assumes the scene
/// compile pipeline auto-mounts `plugin "ark-bus" { mount "hidden" }`
/// whenever a scene declares ANY `keybind` node (T-6.7). This unit-style
/// assertion runs without `ARK_E2E=1` so a regression in
/// `crates/scene/src/compile/inject_bus.rs` (e.g. injection trigger
/// stops covering the keybind-only case) trips the next `cargo test
/// -p ark-cli` rather than waiting for someone to opt into the
/// (currently `#[ignore]`d) e2e.
///
/// This is the cheapest possible smoke around T-8.4's dispatch chain:
/// if ark-bus isn't injected, the `zellij action message-plugin
/// ark-bus` step in `scene_keybind_dispatches` would fail with
/// "plugin not found" before the supervisor gets a chance to dispatch.
#[test]
fn scene_with_keybind_auto_injects_ark_bus_plugin() {
    let scene_kdl = r#"scene "keybind-only" {
    layout { }
    keybind "Alt q" intent="ark.core.close_tab" name="builder"
}
"#;
    let mut doc: ark_scene::ast::SceneDoc =
        facet_kdl::from_str(scene_kdl).expect("scene parses");
    assert!(
        doc.scene
            .plugins
            .iter()
            .all(|p| p.name != ark_scene::compile::ARK_BUS_PLUGIN_NAME),
        "scene must NOT declare ark-bus before the injection pass"
    );
    let injected = ark_scene::compile::maybe_inject_ark_bus(&mut doc.scene);
    assert!(
        injected,
        "T-6.7: keybind-bearing scene must trigger ark-bus injection"
    );
    let bus = doc
        .scene
        .plugins
        .iter()
        .find(|p| p.name == ark_scene::compile::ARK_BUS_PLUGIN_NAME)
        .expect("ark-bus plugin must be present after injection");
    assert_eq!(
        bus.mount.as_ref().map(|m| m.target.as_str()),
        Some(ark_scene::compile::ARK_BUS_MOUNT_TARGET),
        "ark-bus must mount via the suppressed-pane API ({})",
        ark_scene::compile::ARK_BUS_MOUNT_TARGET
    );
}

/// Poll `zellij list-sessions` until exactly one ark-prefixed session is
/// reported, returning its name. Strips ANSI escape sequences zellij
/// emits even with `--no-decoration` set on some platforms.
fn wait_until_session(timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(s) = list_one_ark_session() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Best-effort: returns the first session name starting with `"ark-"`
/// from `zellij list-sessions`. None if the binary isn't running yet,
/// or if zero / multiple ark sessions are visible (multiple = caller's
/// problem; we keep the helper conservative).
fn list_one_ark_session() -> Option<String> {
    let out = Command::new("zellij")
        .arg("list-sessions")
        .arg("--no-formatting")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut found: Option<String> = None;
    for line in stdout.lines() {
        // zellij prints `<session-name> [Created ...]`; we want the
        // bare token.
        let token = line.split_whitespace().next().unwrap_or("");
        if token.starts_with("ark-") {
            if found.is_some() {
                // Multiple ark sessions visible — caller would need to
                // disambiguate. Be conservative.
                return None;
            }
            found = Some(token.to_string());
        }
    }
    found
}

/// Poll `zellij action list-tabs` (JSON output) until `predicate` over
/// the parsed tab-name list returns true or `timeout` expires.
fn wait_until_tabs<F: Fn(&[String]) -> bool>(
    session: &str,
    timeout: Duration,
    predicate: F,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let tabs = list_tabs(session).unwrap_or_default();
        if predicate(&tabs) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(75));
    }
    false
}

/// Run `zellij --session <s> action list-tabs` (no `--output-json`
/// flag in stable zellij — `list-tabs` emits one tab name per line by
/// default, which is enough for the predicate). Returns the tab names
/// as `Vec<String>`; empty on any IPC failure (caller treats as
/// "predicate not yet satisfied").
fn list_tabs(session: &str) -> Option<Vec<String>> {
    let out = Command::new("zellij")
        .arg("--session")
        .arg(session)
        .arg("action")
        .arg("list-tabs")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut tabs: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip the trailing " (current focus)" annotation zellij adds
        // to the focused tab so the equality check is uniform.
        let name = trimmed
            .strip_suffix(" (current focus)")
            .unwrap_or(trimmed)
            .to_string();
        tabs.push(name);
    }
    Some(tabs)
}
