//! T-127 + T-128 — end-to-end scenarios (cavekit-testing R4).
//!
//! These tests drive the installed `ark` binary (and, where useful, the
//! `mock-claude` shim from `ark-test-fixtures`) across realistic lifecycle
//! scenarios. They are disk-heavy and occasionally spawn subprocesses, so
//! they are gated behind `ARK_E2E=1` — `cargo test` normally skips them
//! without failing. The gate, tempdir bundle, and subprocess cleanup
//! logic all live in [`e2e_support`] (T-128). Each scenario:
//!
//! 1. `if !e2e_support::require_e2e() { return; }` — skip when the
//!    `ARK_E2E=1` env var is absent.
//! 2. `let _env = e2e_support::E2eEnv::new();` — RAII guard that stamps
//!    `ARK_STATE_DIR` / `ARK_RUNTIME_DIR` / `ARK_CONFIG_DIR` onto the
//!    process and, on drop (including during a test panic), SIGTERMs
//!    any tracked subprocesses then restores the prior env vars.
//!
//! ## Scenario coverage
//!
//! The kit lists seven scenarios. Several require a live `ark-supervisor`
//! binary (listening on a control socket, reading events from mock-claude)
//! which does not yet exist in this build — `ark spawn` warns that the
//! supervisor is stubbed (see `crates/cli/src/commands/spawn.rs` §7). The
//! scenarios split like this:
//!
//! | # | Scenario                        | Status  | Notes |
//! |---|---------------------------------|---------|-------|
//! | 1 | spawn → list                    | covered | zellij-gated |
//! | 2 | spawn → kill                    | stubbed | supervisor not live; proxied via synthetic state |
//! | 3 | spawn → stall                   | deferred| needs real supervisor |
//! | 4 | spawn → done                    | deferred| needs real supervisor |
//! | 5 | crashed-supervisor archive      | covered | synthetic status.json |
//! | 6 | picker-spawn via `ark spawn`    | covered | zellij-gated |
//! | 7 | socket GC via `ark doctor --fix`| covered | no external deps |
//!
//! `spawn → kill` still has a smoke test against the `ark kill` idempotent
//! "already dead" path so the command surface is at least exercised.

mod e2e_support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use ark_test_fixtures::paths as fixture_paths;
use ark_types::{AgentId, AgentSpec, AgentStatus};

// ---- local helpers --------------------------------------------------------

/// Skip the test if `zellij` is not on PATH. Many scenarios need a real
/// zellij because `ark spawn` preflights it before doing anything else.
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

/// Run `cmd`, capture output, and include both streams in any panic
/// message. Callers still inspect `status`/`stdout`/`stderr` afterwards.
fn capture(cmd: &mut Command) -> Output {
    cmd.output().expect("spawn ark sub-process")
}

/// Write a synthetic `{state}/agents/{id}/spec.json` + `status.json` pair
/// as if a supervisor had completed and archived itself. Used by the
/// crashed-supervisor scenario.
fn seed_archived_agent(state_dir: &Path, orchestrator: &str, name: &str) -> AgentId {
    let id = AgentId::new(orchestrator, name);
    let agent_dir = state_dir.join("agents").join(id.as_str());
    std::fs::create_dir_all(&agent_dir).unwrap();

    let spec = AgentSpec::new(
        id.clone(),
        name,
        orchestrator,
        "claude-code",
        PathBuf::from("/tmp/fake-worktree"),
        vec!["claude".to_string()],
    );
    std::fs::write(
        agent_dir.join("spec.json"),
        serde_json::to_vec_pretty(&spec).unwrap(),
    )
    .unwrap();

    // Status.json declares a terminal phase so `ark list` classifies this
    // agent as `Archived` (F-518) rather than `Orphan`.
    let mut status = AgentStatus::new(spec, 0);
    status.phase = ark_types::Phase::Crashed;
    status.last_event_summary = "supervisor OOM — archived for doctor".into();
    std::fs::write(
        agent_dir.join("status.json"),
        serde_json::to_vec_pretty(&status).unwrap(),
    )
    .unwrap();

    id
}

// ---- scenario 1: spawn → list --------------------------------------------

/// Scenario 1 (spawn → list). Runs a real `ark spawn` against a tempdir
/// state layout, then `ark list` and asserts the agent shows up in
/// stdout. Requires zellij because `ark spawn` preflights it.
#[test]
fn scenario_spawn_then_list() {
    if !e2e_support::require_e2e() {
        return;
    }
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH (required by ark spawn preflight)");
        return;
    }

    let env = e2e_support::E2eEnv::new();

    // Drive `ark spawn` with a no-op command that exits quickly — the
    // supervisor is stubbed so we're really just exercising the CLI
    // side-effects (spec.json write, state dir creation).
    let cwd = env.state_dir().to_path_buf();
    let out = capture(
        env.ark()
            .arg("spawn")
            .arg("--orchestrator")
            .arg("claude-code")
            .arg("--cwd")
            .arg(&cwd)
            .arg("--name")
            .arg("e2e1")
            .arg("--no-detach")
            .arg("--")
            .arg("true"),
    );
    // We don't hard-assert success: depending on the zellij version +
    // no-detach timing the command may exit non-zero after running the
    // `true` CMD. What we *do* assert is that spec.json landed somewhere
    // in the state dir.
    let agents_root = env.state_dir().join("agents");
    let mut found_spec = false;
    if let Ok(entries) = std::fs::read_dir(&agents_root) {
        for e in entries.flatten() {
            if e.path().join("spec.json").is_file() {
                found_spec = true;
                break;
            }
        }
    }
    assert!(
        found_spec,
        "no spec.json under {}; ark spawn stderr={:?}, stdout={:?}",
        agents_root.display(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );

    // Now invoke `ark list` — the newly-spawned agent should appear even
    // though its supervisor is stubbed (rendered as an Orphan row).
    let list = capture(env.ark().arg("list"));
    assert!(list.status.success(), "ark list exited non-zero");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("e2e1") || stdout.contains("claude-code"),
        "ark list did not surface spawned agent; stdout={stdout}"
    );
}

// ---- scenario 2: spawn → kill (stubbed via idempotent path) ---------------

/// Scenario 2 (spawn → kill). The real supervisor is stubbed so `ark
/// spawn` cannot produce a live control socket. We exercise the next
/// best thing: `ark kill` against an agent that exists on disk but has
/// no live socket must take the "already dead — nothing to do"
/// idempotent branch (see `commands/kill.rs` §120).
#[test]
fn scenario_spawn_then_kill() {
    if !e2e_support::require_e2e() {
        return;
    }

    let env = e2e_support::E2eEnv::new();
    // Seed a synthetic agent so the id resolver has something to match.
    let id = seed_archived_agent(env.state_dir(), "claude-code", "e2e2");

    let out = capture(env.ark().arg("kill").arg(id.as_str()));
    assert!(
        out.status.success(),
        "ark kill on a dead supervisor must be idempotent; stderr={:?}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already dead") || stderr.contains("nothing to do"),
        "expected idempotent warning, got: {stderr}"
    );
}

// ---- scenarios 3 + 4: spawn → stall / done (deferred) --------------------

/// Scenario 3 (spawn → stall). DEFERRED: the stall detector lives inside
/// `ark-supervisor`, which has no ark-facing binary in this build
/// (see `commands/spawn.rs` §7). When the supervisor binary lands the
/// test wires up a mock-claude script with a long `delay_ms` between
/// events and asserts `Phase::Stalled` arrives in `status.json`.
#[test]
fn scenario_spawn_then_stall_deferred() {
    if !e2e_support::require_e2e() {
        return;
    }
    if e2e_support::mock_claude_bin().is_none() {
        eprintln!("SKIP: mock-claude binary not built; scenario deferred");
        return;
    }
    let _env = e2e_support::E2eEnv::new();
    // Placeholder assertion: the stall fixture exists so we know what
    // the real test would feed once the supervisor ships.
    let script = PathBuf::from(fixture_paths::MOCK_CLAUDE_SCRIPTS).join("stall-script.json");
    assert!(
        script.is_file(),
        "stall fixture missing at {}",
        script.display()
    );
    eprintln!("DEFERRED: supervisor binary not present; see T-062 / T-069");
}

/// Scenario 4 (spawn → done). DEFERRED for the same reason as scenario
/// 3. Once the supervisor binary lands the test spawns via ark with a
/// `stop-only` mock-claude script and asserts `Phase::Done` is persisted
/// to `status.json`.
#[test]
fn scenario_spawn_then_done_deferred() {
    if !e2e_support::require_e2e() {
        return;
    }
    if e2e_support::mock_claude_bin().is_none() {
        eprintln!("SKIP: mock-claude binary not built; scenario deferred");
        return;
    }
    let _env = e2e_support::E2eEnv::new();
    let script = PathBuf::from(fixture_paths::MOCK_CLAUDE_SCRIPTS).join("stop-only.json");
    assert!(
        script.is_file(),
        "stop-only fixture missing at {}",
        script.display()
    );
    eprintln!("DEFERRED: supervisor binary not present; see T-062 / T-069");
}

// ---- scenario 5: crashed-supervisor archive ------------------------------

/// Scenario 5. Seed a synthetic agent dir containing a persisted
/// terminal `status.json` — this is what the real supervisor writes at
/// shutdown (F-518). `ark list` must classify it as `Archived` (source:
/// status.json) rather than as an orphan that would force the user to
/// reach for `ark doctor`.
#[test]
fn scenario_crashed_supervisor_archive() {
    if !e2e_support::require_e2e() {
        return;
    }

    let env = e2e_support::E2eEnv::new();
    let id = seed_archived_agent(env.state_dir(), "cavekit", "oomd");

    // `ark list ID --json` emits the detail view as JSON, so we can
    // assert on `phase == "crashed"` programmatically.
    let out = capture(env.ark().arg("list").arg(id.as_str()).arg("--json"));
    assert!(
        out.status.success(),
        "ark list exited non-zero; stderr={:?}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The JSON detail view includes the persisted phase verbatim.
    assert!(
        stdout.contains("\"crashed\"") || stdout.contains("crashed"),
        "ark list --json did not report crashed phase; stdout={stdout}"
    );
}

// ---- scenario 6: picker-spawn via `ark spawn` ----------------------------

/// Scenario 6. The picker plugin's `run_command` path ultimately execs
/// `ark spawn --orchestrator X --cwd Y --name Z -- mock-claude ...`.
/// This test drives that exact argv shape against a tempdir state layout
/// — no plugin involved — to confirm the subcommand can be reached via
/// the same entry point the picker uses.
#[test]
fn scenario_picker_spawn_exec() {
    if !e2e_support::require_e2e() {
        return;
    }
    if !zellij_on_path() {
        eprintln!("SKIP: zellij not on PATH (required by ark spawn preflight)");
        return;
    }
    let Some(mock) = e2e_support::mock_claude_bin() else {
        eprintln!("SKIP: mock-claude binary not built");
        return;
    };
    let script = PathBuf::from(fixture_paths::MOCK_CLAUDE_SCRIPTS).join("stop-only.json");

    let env = e2e_support::E2eEnv::new();
    let cwd = env.state_dir().to_path_buf();
    let out = capture(
        env.ark()
            .arg("spawn")
            .arg("--orchestrator")
            .arg("claude-code")
            .arg("--cwd")
            .arg(&cwd)
            .arg("--name")
            .arg("picker-e2e")
            .arg("--no-detach")
            .arg("--")
            .arg(&mock)
            .arg("--script")
            .arg(&script),
    );
    // Same as scenario 1: behavior depends on zellij + stubbed
    // supervisor. We confirm the CLI progressed far enough to write
    // spec.json under the picker-supplied name.
    let agents_root = env.state_dir().join("agents");
    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(&agents_root) {
        for e in entries.flatten() {
            let spec = e.path().join("spec.json");
            if !spec.is_file() {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(&spec) {
                if raw.contains("picker-e2e") {
                    found = true;
                    break;
                }
            }
        }
    }
    assert!(
        found,
        "spec.json for picker-e2e agent was never written; stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---- scenario 7: socket GC via `ark doctor --fix` ------------------------

/// Scenario 7. Drop a plain file at the socket path; `ark doctor`
/// detects it as an orphan (no listener bound) and `--fix --yes` deletes
/// it. `ark doctor` may still exit non-zero if other preflight checks
/// fail in the stripped test env (e.g. claude CLI missing), so we read
/// the filesystem effect rather than the exit code.
#[test]
fn scenario_doctor_fix_removes_stale_socket() {
    if !e2e_support::require_e2e() {
        return;
    }

    let env = e2e_support::E2eEnv::new();
    // Use a well-formed agent id so AgentId::parse inside the orphan
    // classifier accepts it.
    let id = AgentId::new("claude-code", "ghost");
    let sock_path = env
        .runtime_dir()
        .join("agents")
        .join(format!("{}.sock", id.as_str()));
    std::fs::write(&sock_path, b"").expect("seed stale socket");
    assert!(sock_path.is_file());

    // `--fix --yes` auto-accepts every remediation prompt. Socket GC
    // runs unconditionally in `run_fixes`.
    let _ = capture(env.ark().arg("doctor").arg("--fix").arg("--yes"));

    assert!(
        !sock_path.exists(),
        "stale socket should have been deleted: {}",
        sock_path.display()
    );
}
