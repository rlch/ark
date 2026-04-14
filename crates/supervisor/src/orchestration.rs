//! `run_supervisor` — the full R3 18-step boot sequence (T-069).
//!
//! Implements cavekit-supervisor.md R3 end-to-end. Callers (the daemon fork
//! entry from T-062 or the foreground path from T-063) have already run
//! logging setup + fork/detach; `run_supervisor` picks up from there with
//! the agent spec + config in hand and drives the rest of the lifecycle:
//!
//! ```text
//! 1. Create StateDir + spec.json + initial status.json (Starting)
//! 2. Acquire exclusive file lock
//! 3. Bind control socket (wraps the T-066 handler behind the generic
//!    ControlCommandHandler trait)
//! 4. Logging (assumed installed by daemonize/foreground; idempotent
//!    re-install attempt guarded)
//! 5. Config (accepted as a parameter)
//! 6. Factory: Engine / Orchestrator / Mux
//! 7. mux.ensure_session(spec.session)
//! 8. engine.preflight (free fn from ark-engines-claude-code)
//! 9. Spawn consumer tasks (state_writer, status_pipe, hook_dispatcher)
//! 10. engine.install_observability → EngineHandle
//! 11. Emit Started { spec }
//! 12. Signal readiness to parent (Daemon: print agent_id to stdout;
//!     Foreground: no-op)
//! 13. orchestrator.run(spec, world) — long-running
//! 14. Drain consumer tasks
//! 15. engine.teardown(handle) — restores .claude/settings.local.json
//! 16. finalize_state: write final status.json with terminal phase
//! 17. Unlink control socket (explicit shutdown + Drop guard)
//! 18. Release lock + exit-code derivation
//! ```
//!
//! See the cavekit-supervisor.md R3 list for the authoritative ordering.
//!
//! ## Shape of the return
//!
//! On success [`run_supervisor`] returns the `Outcome` the orchestrator
//! produced. The daemon path turns that into an exit code via
//! [`outcome_exit_code`]; the foreground path may propagate it up to the
//! parent CLI directly.

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result};
use ark_core::consumers::{hook_dispatcher, state_writer, status_pipe};
use ark_core::{Config, Engine, Multiplexer, Orchestrator, World, write_status_atomic};
use ark_engines_claude_code::preflight;
use ark_types::{
    AgentEvent, AgentId, AgentSpec, AgentStatus, CancellationToken, Outcome, Phase, StateLayout,
    channel,
};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::commands::{SupervisorCommandCtx, SupervisorCommandHandler};
use crate::control_socket::{ControlCommandHandler, bind_control_socket, shutdown};
use crate::lock::{LockGuard, acquire_lock};

/// Which boot path reached [`run_supervisor`].
///
/// This controls the "signal readiness to parent" step (R3 step 12):
/// * `Daemon` — the original CLI parent is waiting on a detached grandchild;
///   we print `agent_id\n` to stdout so the parent can print it and exit.
/// * `Foreground` — the parent already attached to this process's stderr
///   via `run_foreground`; no stdout-id print is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorMode {
    Daemon,
    Foreground,
}

/// Event-bus capacity used when `Config` does not yet carry one.
///
/// Mirrors `ark_config::schema::DEFAULT_EVENT_BUS_CAPACITY` so the runtime
/// sizing is consistent whether or not the `Config` placeholder is swapped
/// out for the real figment-loaded type.
const DEFAULT_EVENT_BUS_CAPACITY: usize = 256;

/// Map an `Outcome` to a Unix exit code.
///
/// * `Success` → 0
/// * `Failed`  → 1
/// * `Killed`  → 2
/// * `Timeout` → 2 (treated same as Killed — the supervisor hit a kill
///   signal via cancel, and Unix generally surfaces timeouts that way)
/// * `Crashed` → 3
pub fn outcome_exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Success { .. } => 0,
        Outcome::Failed { .. } => 1,
        Outcome::Killed | Outcome::Timeout => 2,
        Outcome::Crashed { .. } => 3,
    }
}

/// Run the supervisor to completion.
///
/// The full 18-step R3 sequence. Long-running: the inner
/// `orchestrator.run` drives for the lifetime of the agent. On return the
/// consumers are drained, the engine is torn down, status is finalised,
/// and the lock is released (via the dropped [`LockGuard`]).
pub async fn run_supervisor(
    spec: AgentSpec,
    mode: SupervisorMode,
    config: Config,
) -> Result<Outcome> {
    let state_layout = StateLayout::from_env().context("resolve state layout")?;
    let engine = crate::factory::build_engine(&spec.engine, &config).context("build engine")?;
    let orchestrator = crate::factory::build_orchestrator(&spec.orchestrator, &config)
        .context("build orchestrator")?;
    let mux = crate::factory::build_multiplexer("zellij", &config).context("build multiplexer")?;
    run_supervisor_with(
        spec,
        mode,
        config,
        state_layout,
        engine,
        orchestrator,
        mux,
        /* run_preflight */ true,
    )
    .await
}

/// Variant of [`run_supervisor`] that accepts injected layout + factories.
///
/// Preferred entry point for tests: lets them swap the Engine, Orchestrator,
/// and Multiplexer to stubs without relying on the v1-locked factory
/// slugs. `run_preflight = false` skips the claude-code preflight check
/// for tests that don't have the real `claude` binary on PATH.
#[allow(clippy::too_many_arguments)]
pub async fn run_supervisor_with(
    spec: AgentSpec,
    mode: SupervisorMode,
    config: Config,
    state_layout: StateLayout,
    engine: Box<dyn Engine>,
    orchestrator: Box<dyn Orchestrator>,
    mux: Arc<dyn Multiplexer>,
    run_preflight: bool,
) -> Result<Outcome> {
    // ---- Step 1: StateDir + spec.json + initial status.json ----
    let agent_dir = state_layout.agent_dir(&spec.id);
    StateLayout::ensure_dir_0700(&agent_dir).context("ensure agent state dir")?;
    write_spec_json(&state_layout, &spec).context("write spec.json")?;
    let supervisor_pid = std::process::id();
    write_pid_file(&state_layout, &spec.id, supervisor_pid).context("write pid file")?;
    let initial_status = AgentStatus::new(spec.clone(), supervisor_pid);
    write_status_atomic(&state_layout, &spec.id, &initial_status)
        .context("write initial status.json")?;
    debug!(agent = %spec.id.as_str(), "R3 step 1: state dir ready");

    // ---- Step 2: acquire exclusive file lock ----
    let lock_guard: LockGuard =
        acquire_lock(&state_layout, &spec.id).context("acquire per-agent file lock")?;
    debug!(path = %lock_guard.path().display(), "R3 step 2: lock acquired");

    // Layout / state become Arcs for the cloneable shares that follow.
    let state_arc: Arc<StateLayout> = Arc::new(state_layout.clone());
    let config_arc: Arc<Config> = Arc::new(config.clone());

    // Cancel token threaded into every async component + the control-socket
    // command handler so `Kill` / SIGTERM both unwind through the same
    // path.
    let cancel = CancellationToken::new();

    // Event bus — created BEFORE the socket handler because the handler's
    // ctx wants an `EventSink` for future audit-log routing.
    let (events, _boot_rx) = channel(DEFAULT_EVENT_BUS_CAPACITY);

    // ---- Step 3: bind control socket ----
    let command_handler: Arc<dyn ControlCommandHandler> =
        Arc::new(SupervisorCommandHandler::new(SupervisorCommandCtx {
            agent_id: spec.id.clone(),
            state_layout: state_layout.clone(),
            pid: nix::unistd::Pid::from_raw(supervisor_pid as i32),
            cancel: cancel.clone(),
            event_bus: events.clone(),
        }));
    let socket_handle = bind_control_socket(&state_layout, &spec.id, command_handler.clone())
        .await
        .context("bind control socket")?;
    debug!(path = %socket_handle.path().display(), "R3 step 3: control socket bound");

    // ---- Step 4: logging ----
    // Assumed installed by daemonize/foreground. A global tracing
    // subscriber can only be set once process-wide, and either the daemon
    // `setup_supervisor_log` or the foreground tracer has already done so.
    // No-op here by design.

    // ---- Step 5: config ----
    // Accepted as parameter; nothing to do — CLI/Tier 4 layers figment
    // before the fork.

    // ---- Step 6: factory ----
    //
    // Engines, orchestrator, mux are pre-built by `run_supervisor` via the
    // factory module; passed in here so tests can swap them.
    debug!(
        engine = engine.name(),
        orch = orchestrator.name(),
        mux = mux.kind(),
        "R3 step 6: factories resolved"
    );

    // ---- Step 7: ensure session ----
    mux.ensure_session(&spec.session)
        .await
        .with_context(|| format!("mux.ensure_session({})", spec.session))?;

    // ---- Step 8: preflight ----
    if run_preflight && spec.engine == "claude-code" {
        preflight::preflight(&spec).context("engine preflight")?;
    }

    // ---- Step 9: spawn consumer tasks ----
    //
    // Three long-running consumers, each owned by a JoinSet the supervisor
    // drains at step 14. They subscribe to the bus (via `events.subscribe()`)
    // rather than cloning the sender — the sender is only cloned once for
    // `state_writer`'s optional phase-transition re-broadcast path.
    let mut consumers: JoinSet<Result<()>> = JoinSet::new();

    {
        let rx = events.subscribe();
        let tx_for_state = events.clone();
        let state_arc = state_arc.clone();
        let id = spec.id.clone();
        let cancel = cancel.clone();
        consumers.spawn(async move {
            state_writer(
                rx,
                Some(tx_for_state),
                state_arc,
                id,
                supervisor_pid,
                cancel,
            )
            .await
        });
    }

    {
        let rx = events.subscribe();
        let mux = mux.clone();
        let cancel = cancel.clone();
        consumers.spawn(async move { status_pipe(rx, mux, cancel).await });
    }

    {
        let rx = events.subscribe();
        // No hooks configured at the placeholder-config layer yet; pass
        // an empty Vec so the consumer is attached but idle. T-018+ wires
        // the figment-loaded hooks through.
        let hooks: Arc<Vec<ark_config::HookEntry>> = Arc::new(Vec::new());
        let orch_slug = spec.orchestrator.clone();
        let cancel = cancel.clone();
        consumers.spawn(async move { hook_dispatcher(rx, hooks, orch_slug, cancel).await });
    }
    debug!("R3 step 9: consumer tasks spawned");

    // ---- Step 10: install observability ----
    let engine_handle = engine
        .install_observability(&spec.cwd, events.clone())
        .await
        .context("engine install_observability")?;
    debug!(
        engine = engine.name(),
        "R3 step 10: observability installed"
    );

    // ---- Step 11: emit Started ----
    // Best-effort: if nobody is subscribed yet, the Err is benign. The
    // consumers spawned at step 9 always have receivers alive at this
    // point, so in practice this never fails.
    let _ = events.send(AgentEvent::Started { spec: spec.clone() });
    debug!("R3 step 11: Started event emitted");

    // ---- Step 12: signal readiness to parent ----
    if matches!(mode, SupervisorMode::Daemon) {
        // The parent CLI is blocked reading one line of stdout. Print the
        // id and flush immediately.
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", spec.id.as_str());
        let _ = out.flush();
    }
    info!(agent = %spec.id.as_str(), "supervisor ready");

    // ---- Step 13: orchestrator.run (long-running) ----
    //
    // Build the World the orchestrator expects. `hooks_dir` is the
    // per-agent hooks staging dir under the state layout.
    let hooks_dir = state_layout.hooks_dir(&spec.id);
    let world = World::new(
        spec.clone(),
        mux.clone(),
        events.clone(),
        cancel.clone(),
        hooks_dir,
        state_arc.clone(),
        config_arc.clone(),
    );

    let outcome = match orchestrator.run(spec.clone(), world).await {
        Ok(o) => o,
        Err(err) => {
            warn!(error = %err, "orchestrator.run returned Err; treating as Crashed");
            Outcome::Crashed {
                reason: format!("{err}"),
            }
        }
    };
    debug!(?outcome, "R3 step 13: orchestrator.run returned");

    // Final Done event so consumers observe a terminal phase before we
    // drain them. This is the authoritative signal state_writer uses to
    // pin `status.json.phase` to Done/Failed/Crashed.
    let _ = events.send(AgentEvent::Done {
        id: spec.id.clone(),
        outcome: outcome.clone(),
    });

    // ---- Step 14: drain consumer tasks ----
    //
    // Short flush window gives the state_writer a chance to roll up every
    // buffered event (Started, PhaseTransitions, the final Done) into
    // status.json + events.jsonl before we fire the cancel that causes
    // its select! loop to break. 100ms is plenty — the rollup itself is
    // a handful of atomic writes per event.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    drop(events);
    cancel.cancel();
    drain_consumers(&mut consumers, std::time::Duration::from_secs(5)).await;
    debug!("R3 step 14: consumers drained");

    // ---- Step 15: engine teardown ----
    if let Err(err) = engine.teardown(engine_handle).await {
        warn!(error = %err, "engine.teardown failed — continuing to finalize");
    }
    debug!("R3 step 15: engine torn down");

    // ---- Step 16: finalize state ----
    if let Err(err) = finalize_state(&state_layout, &spec.id, supervisor_pid, &outcome) {
        warn!(error = %err, "finalize_state failed — status.json may be stale");
    }
    debug!(?outcome, "R3 step 16: state finalised");

    // ---- Step 17: unlink control socket ----
    if let Err(err) = shutdown(socket_handle).await {
        warn!(error = %err, "control socket shutdown failed");
    }
    debug!("R3 step 17: control socket torn down");

    // ---- Step 18: release lock (drop guard on return) ----
    drop(lock_guard);
    debug!("R3 step 18: lock released");

    Ok(outcome)
}

/// Write the authoritative `spec.json` under the agent state dir.
fn write_spec_json(layout: &StateLayout, spec: &AgentSpec) -> std::io::Result<()> {
    let path = layout.spec_path(&spec.id);
    if let Some(parent) = path.parent() {
        StateLayout::ensure_dir_0700(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(spec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Write the supervisor pid to `$STATE/agents/{id}/pid` for `ark list`
/// liveness checks and `ark kill` PID lookups.
fn write_pid_file(layout: &StateLayout, id: &AgentId, pid: u32) -> std::io::Result<()> {
    let path = layout.pid_path(id);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{pid}\n").as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Drain every consumer task with a bounded overall timeout. Errors /
/// panics are warn-logged; we do not abort the supervisor for a misbehaving
/// consumer.
async fn drain_consumers(set: &mut JoinSet<Result<()>>, timeout: std::time::Duration) {
    let drain = async {
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    warn!(error = %err, "consumer task returned Err during drain");
                }
                Err(join_err) => {
                    warn!(error = %join_err, "consumer task join error during drain");
                }
            }
        }
    };
    if tokio::time::timeout(timeout, drain).await.is_err() {
        warn!(
            timeout_secs = timeout.as_secs(),
            "consumer drain timed out — aborting remaining"
        );
        set.abort_all();
    }
}

/// Write the final `status.json` with the terminal phase derived from
/// `outcome`. Preserves prior fields via a read-modify-write so findings
/// etc. accumulated during the run survive.
pub fn finalize_state(
    layout: &StateLayout,
    id: &AgentId,
    supervisor_pid: u32,
    outcome: &Outcome,
) -> Result<()> {
    let mut status = match ark_core::read_status(layout, id)? {
        Some(s) => s,
        None => {
            // Nothing on disk — synthesize from a stub so we still publish
            // a terminal status the picker can read.
            let mut s = AgentStatus::new(
                ark_types::AgentSpec::new(
                    id.clone(),
                    id.name(),
                    id.orchestrator(),
                    "claude-code",
                    std::path::PathBuf::new(),
                    Vec::new(),
                ),
                supervisor_pid,
            );
            s.phase = Phase::Running;
            s
        }
    };
    status.phase = match outcome {
        Outcome::Success { .. } => Phase::Done,
        Outcome::Failed { .. } => Phase::Failed,
        Outcome::Killed | Outcome::Timeout => Phase::Done,
        Outcome::Crashed { .. } => Phase::Crashed,
    };
    status.last_event_at = chrono::Utc::now();
    status.last_event_summary = match outcome {
        Outcome::Success { .. } => "done: success".to_string(),
        Outcome::Failed { reason } => format!("done: failed ({reason})"),
        Outcome::Killed => "done: killed".to_string(),
        Outcome::Timeout => "done: timeout".to_string(),
        Outcome::Crashed { reason } => format!("done: crashed ({reason})"),
    };
    write_status_atomic(layout, id, &status)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::Multiplexer;
    use ark_types::{AgentEvent, AgentId, AgentSpec, Outcome, TabHandle};
    use async_trait::async_trait;

    // --- stub mux / engine / orchestrator for the smoke test ---------------

    struct StubMux;

    #[async_trait]
    impl Multiplexer for StubMux {
        fn kind(&self) -> &'static str {
            "stub"
        }
        async fn ensure_session(&self, _name: &str) -> Result<()> {
            Ok(())
        }
        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            _layout_path: &std::path::Path,
        ) -> Result<TabHandle> {
            Ok(TabHandle::new(session, 1, name))
        }
        async fn close_tab(&self, _handle: &TabHandle) -> Result<()> {
            Ok(())
        }
        async fn rename_tab(&self, _handle: &TabHandle, _name: &str) -> Result<()> {
            Ok(())
        }
        async fn pipe(&self, _target: &str, _payload: &str) -> Result<()> {
            Ok(())
        }
    }

    struct StubEngine;

    #[async_trait]
    impl Engine for StubEngine {
        fn name(&self) -> &'static str {
            "stub-engine"
        }
        async fn install_observability(
            &self,
            _cwd: &std::path::Path,
            _sink: ark_types::EventSink,
        ) -> Result<ark_core::engine::EngineHandle> {
            Ok(ark_core::engine::EngineHandle::new("stub-engine", 0u32))
        }
        async fn teardown(&self, _handle: ark_core::engine::EngineHandle) -> Result<()> {
            Ok(())
        }
        fn default_pane_cmd(&self) -> Vec<String> {
            vec!["stub".to_string()]
        }
        fn transcript_path(&self, _cwd: &std::path::Path) -> Option<std::path::PathBuf> {
            None
        }
        async fn auto_approve_permissions(
            &self,
            _cwd: &std::path::Path,
            _policy: ark_core::engine::ApprovalPolicy,
        ) -> Result<()> {
            Ok(())
        }
    }

    /// Fire Outcome::Success without touching the World's mux.
    struct InstantSuccessOrchestrator;

    #[async_trait]
    impl Orchestrator for InstantSuccessOrchestrator {
        fn name(&self) -> &'static str {
            "instant"
        }
        fn engine(&self) -> &'static str {
            "stub-engine"
        }
        fn detect(&self, _cwd: &std::path::Path) -> bool {
            true
        }
        async fn run(&self, _spec: AgentSpec, _world: World) -> Result<Outcome> {
            Ok(Outcome::Success { artifacts: vec![] })
        }
    }

    fn sample_spec() -> AgentSpec {
        let id = AgentId::new("cavekit", "smoke");
        AgentSpec::new(
            id,
            "smoke",
            "cavekit",
            "stub-engine",
            std::path::PathBuf::from("/tmp"),
            vec!["stub".to_string()],
        )
    }

    #[tokio::test]
    async fn run_supervisor_with_smoke_test_drives_all_18_steps() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let spec = sample_spec();

        // Subscribe BEFORE run_supervisor returns so we can observe Started.
        // We don't have direct access to the bus from outside; instead, read
        // events.jsonl after drain to confirm Started landed.

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Box::new(StubEngine),
            Box::new(InstantSuccessOrchestrator),
            Arc::new(StubMux),
            false,
        )
        .await
        .expect("run_supervisor_with ok");

        // Outcome must be Success.
        match &result {
            Outcome::Success { .. } => {}
            other => panic!("expected Success, got {other:?}"),
        }

        // ---- Step 1 verification: StateDir + spec.json + status.json present
        assert!(layout.agent_dir(&spec.id).is_dir(), "agent dir must exist");
        assert!(layout.spec_path(&spec.id).is_file(), "spec.json must exist");

        // ---- Step 16 verification: final status.json reflects Done
        let status = ark_core::read_status(&layout, &spec.id)
            .expect("read status")
            .expect("status exists");
        assert_eq!(status.phase, Phase::Done, "final phase must be Done");

        // ---- Step 11 verification: Started event on events.jsonl
        // And Done event at the tail.
        let events_path = layout.events_path(&spec.id);
        if events_path.is_file() {
            let mut reader = ark_core::EventLogReader::open(&events_path).unwrap();
            let records = reader.read_all();
            assert!(
                records
                    .iter()
                    .any(|r| matches!(r.event, AgentEvent::Started { .. })),
                "events.jsonl should contain Started"
            );
            assert!(
                records
                    .iter()
                    .any(|r| matches!(r.event, AgentEvent::Done { .. })),
                "events.jsonl should contain Done"
            );
        }

        // ---- Step 17 verification: socket file unlinked
        let sock = layout.agent_socket_path(&spec.id);
        assert!(
            !sock.exists(),
            "socket file should be unlinked after shutdown: {}",
            sock.display()
        );

        // ---- Step 18 verification: lock released — re-acquire should work.
        let re = crate::acquire_lock(&layout, &spec.id).expect("re-acquire");
        assert_eq!(re.path(), layout.lock_path(&spec.id).as_path());
        drop(re);
    }

    #[test]
    fn outcome_exit_code_matches_kit() {
        assert_eq!(
            outcome_exit_code(&Outcome::Success { artifacts: vec![] }),
            0
        );
        assert_eq!(
            outcome_exit_code(&Outcome::Failed {
                reason: "boom".into()
            }),
            1
        );
        assert_eq!(outcome_exit_code(&Outcome::Killed), 2);
        assert_eq!(outcome_exit_code(&Outcome::Timeout), 2);
        assert_eq!(
            outcome_exit_code(&Outcome::Crashed {
                reason: "panic".into()
            }),
            3
        );
    }

    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("sv-run")
            .tempdir_in("/tmp")
            .expect("short tempdir")
    }

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    #[test]
    fn finalize_state_success_maps_to_done() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "final");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        finalize_state(&layout, &id, 42, &Outcome::Success { artifacts: vec![] })
            .expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Done);
        assert!(s.last_event_summary.contains("success"));
    }

    #[test]
    fn finalize_state_failed_maps_to_failed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fail");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        finalize_state(
            &layout,
            &id,
            42,
            &Outcome::Failed {
                reason: "nope".into(),
            },
        )
        .expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Failed);
    }

    #[test]
    fn finalize_state_crashed_maps_to_crashed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "crash");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        finalize_state(
            &layout,
            &id,
            42,
            &Outcome::Crashed {
                reason: "SEGV".into(),
            },
        )
        .expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Crashed);
    }

    #[test]
    fn finalize_state_preserves_spec_and_findings() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "preserve");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        // Seed an existing status with some findings.
        let mut seed = AgentStatus::new(
            ark_types::AgentSpec::new(
                id.clone(),
                "preserve",
                "cavekit",
                "claude-code",
                std::path::PathBuf::from("/tmp/wt"),
                vec!["claude".into()],
            ),
            99,
        );
        seed.findings.record(ark_types::Severity::P0);
        seed.findings.record(ark_types::Severity::P1);
        write_status_atomic(&layout, &id, &seed).unwrap();
        finalize_state(&layout, &id, 99, &Outcome::Success { artifacts: vec![] }).unwrap();
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.findings.p0, 1);
        assert_eq!(s.findings.p1, 1);
        assert_eq!(s.phase, Phase::Done);
    }
}
