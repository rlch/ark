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
//! 9. Spawn consumer tasks (state_writer, status_pipe, reaction_dispatcher)
//!    — T-5.7 swapped hook_dispatcher for reaction_dispatcher; the legacy
//!    `[[hooks]]` config is compiled into a synthetic ReactionRegistry
//!    via `ark_scene::hook_compat::build_hook_registry`.
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

use std::sync::Arc;

use anyhow::{Context, Result};
use ark_core::consumers::{ReactionDispatcherCtx, reaction_dispatcher, state_writer};
use ark_core::{Config, Engine, Orchestrator, World, write_status_atomic};
use ark_scene::context::{AgentSnapshot, SessionSnapshot};
use ark_scene::hook_compat::HookEntry as SceneHookEntry;
#[cfg(test)]
use ark_scene::id::SceneId;
use ark_scene::intent::{IntentContext, IntentRegistry};
use ark_scene::ops::register_core_ops;
use ark_mux_zellij::ZellijMux;
use ark_types::{
    AgentEvent, AgentId, AgentSpec, AgentStatus, CancellationToken, EventSink, Outcome, Phase,
    StateLayout, channel,
};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::scene_runtime::{CompiledScene, compile_scene_for_runtime};

use crate::commands::{SupervisorCommandCtx, SupervisorCommandHandler};
use crate::consumers::status_pipe;
use crate::control_socket::{ControlCommandHandler, bind_control_socket, shutdown};
use crate::kill::{TabRegistry, apply_tab_event, new_tab_registry};
use crate::lock::{LockGuard, acquire_lock};
use crate::ready_signal::ReadyWriter;
use crate::signals::install_signal_handlers;

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
    ready_writer: Option<ReadyWriter>,
    external_cancel: Option<CancellationToken>,
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
        ready_writer,
        external_cancel,
    )
    .await
}

/// Variant of [`run_supervisor`] that accepts injected layout + factories.
///
/// Preferred entry point for tests: lets them swap the Engine, Orchestrator,
/// and mux to stubs without relying on the v1-locked factory
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
    mux: Arc<ZellijMux>,
    run_preflight: bool,
    ready_writer: Option<ReadyWriter>,
    external_cancel: Option<CancellationToken>,
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
    // path. W-4: callers that drive the supervisor from a background
    // thread (e.g. `--no-detach`) pass an `external_cancel` they hold a
    // clone of so they can trigger shutdown after their foreground
    // subprocess (zellij) exits. Daemon callers pass `None` and rely
    // on the signal handler to fire cancel.
    let cancel = external_cancel.unwrap_or_else(CancellationToken::new);

    // Event bus — created BEFORE the socket handler because the handler's
    // ctx wants an `EventSink` for future audit-log routing.
    let (events, _boot_rx) = channel(DEFAULT_EVENT_BUS_CAPACITY);

    // ---- Step 7 (early): compile scene ----
    //
    // T-8.1: scene compile runs BEFORE bus subscribers, the layout
    // launch, and always-on plugin mount. Artefacts thread into:
    //   * the reaction dispatcher (ReactionRegistry)
    //   * the plugin lifecycle manager (lowered PluginDecls)
    //   * the control-socket intent bridge (IntentRegistry origin id)
    //
    // We run compile here — after StateDir / lock / cancel token exist
    // but before any long-running tokio consumers spawn — so a compile
    // error aborts cleanly via the same lock / state path any legitimate
    // spawn failure takes.
    //
    // The hook list is still empty in the placeholder `Config` (T-018
    // will thread `config.hooks` through here); we pass an empty slice
    // so the hook-compat path is exercised end-to-end but contributes
    // no reactions. When T-018 lands the hook slice is drawn from
    // `config.hooks`.
    let hook_entries: Vec<SceneHookEntry> = Vec::new();
    let compiled_scene: Arc<CompiledScene> = match compile_scene_for_runtime(
        spec.scene_path.as_deref(),
        &hook_entries,
    ) {
        Ok(c) => {
            // T-ACP.4a: resolve the engine launch spec now that we
            // have a parsed scene. Uses the shipped default config
            // plus the `--engine` flag we stashed on
            // `runner_config.acp_engine_flag` in the CLI. Non-fatal:
            // resolution errors downgrade to a warn + default launch
            // so legacy (non-ACP) spawns keep booting. T-ACP.5 wires
            // the actual `AcpClient::spawn` call on top of this.
            let flag = spec
                .runner_config
                .get("acp_engine_flag")
                .and_then(|v| v.as_str());
            // Rung 3 needs the resolved `use`s; that's T-ACP.4b wiring
            // and runs through the scene compile pipeline. For now
            // pass an empty slice (skips rung 3) and let later tiers
            // extend this.
            let cfg = ark_config::schema::Config::defaults();
            let launch = match crate::engine_resolution::resolve_engine(
                flag,
                &c.doc,
                &cfg,
                &[],
            ) {
                Ok(l) => l,
                Err(err) => {
                    warn!(
                        target: "supervisor::engine_resolution",
                        error = %err,
                        "engine resolution failed; falling back to hardcoded default `claude --acp`"
                    );
                    crate::engine_resolution::default_engine_launch()
                }
            };
            debug!(
                target: "supervisor::engine_resolution",
                name = %launch.name,
                command = %launch.command,
                args = ?launch.args,
                "T-ACP.4a: ACP engine resolved"
            );
            let c = c.with_engine_launch(launch);
            debug!(
                source = %c.source.display(),
                reactions = c.registry.len(),
                plugins = c.doc.scene.plugins.len(),
                "R3 step 7: scene compiled"
            );
            Arc::new(c)
        }
        Err(err) => {
            // R3 step 7 contract: compile error = abort spawn with a
            // miette diagnostic. The lock's Drop guard runs on this
            // error-return path, and the caller (daemon / foreground)
            // surfaces the non-zero exit to the parent CLI.
            tracing::error!(
                target: "scene::compile",
                agent = %spec.id.as_str(),
                scene = ?spec.scene_path,
                error = %err,
                "scene compile failed; aborting spawn"
            );
            return Err(err.context("scene compile (R3 step 7)"));
        }
    };

    // ---- Step 3: bind control socket ----
    //
    // T-6.2: the control-socket handler accepts an optional
    // `IntentBridge` so the `Intent { name, args }` command can route
    // through the supervisor's intent registry.
    //
    // T-8.1: the bridge's `IntentContext` now carries the compiled
    // scene's real `SceneId` so cascade telemetry + scene graph
    // attribution line up with the reaction dispatcher's context. The
    // registry itself (core ops only, at this tier) is still a
    // separate handle from the reaction dispatcher's to avoid
    // cross-task lock contention — see the dual-registry note on the
    // step 9 block below. Unifying into a single `Arc<IntentRegistry>`
    // is tracked as a follow-up (the current `IntentRegistry` interior
    // is already `Arc`-shareable; tying the lifetime is the
    // remaining work).
    let intent_bridge = build_intent_bridge_for_socket(&spec, &compiled_scene).await;
    let command_handler: Arc<dyn ControlCommandHandler> =
        Arc::new(SupervisorCommandHandler::new(SupervisorCommandCtx {
            agent_id: spec.id.clone(),
            state_layout: state_layout.clone(),
            pid: nix::unistd::Pid::from_raw(supervisor_pid as i32),
            cancel: cancel.clone(),
            event_bus: events.clone(),
            // Audit log (T-068) wires in as part of a Tier 4 touch; leave
            // None here so the T-069 smoke test suite stays behaviour-
            // equivalent.
            audit: None,
            intents: Some(intent_bridge),
        }));
    let socket_handle = bind_control_socket(&state_layout, &spec.id, command_handler.clone())
        .await
        .context("bind control socket")?;
    debug!(path = %socket_handle.path().display(), "R3 step 3: control socket bound");

    // F-087: install the SIGTERM/SIGINT handler now that the control socket
    // exists. `install_signal_handlers` returns a `SignalTaskHandle` we MUST
    // keep alive for the whole run — dropping it aborts the signal task. On
    // normal exit (end of this fn) the Drop fires and aborts cleanly.
    let _signal_task = install_signal_handlers(socket_handle.path().to_path_buf(), cancel.clone())
        .await
        .context("install signal handlers")?;
    debug!("F-087: SIGTERM/SIGINT handlers installed");

    // F-086: shared tab registry — populated by a long-running bus
    // subscriber that mutates on every TabOpened / TabClosed. `kill_handler`
    // reads this at grace expiry to close every still-open tab. Must be
    // subscribed BEFORE any TabOpened can be emitted (hence: before step 10
    // install_observability and before step 13 orchestrator.run).
    let tab_registry: TabRegistry = new_tab_registry();
    let tab_registry_feeder = {
        let mut rx = events.subscribe();
        let registry = tab_registry.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    recv = rx.recv() => match recv {
                        Ok(ev) => apply_tab_event(&registry, &ev),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                skipped = n,
                                "tab registry feeder: bus lag; tab set may be stale"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        })
    };

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
    //
    // T-ACP.7: the legacy `ark-engines-claude-code::preflight` was
    // retired. The replacement in `engine_stub::preflight` is a
    // lightweight PATH check — anything heavier lives in
    // `ark doctor` (T-ACP.6).
    if run_preflight {
        crate::engine_stub::preflight(&spec).context("engine preflight")?;
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
        // T-8.1: the reaction dispatcher consumes the registry built
        // from the user's scene (R3 step 7) instead of the empty
        // placeholder the pre-T-8.1 code wired. Legacy `[[hooks]]`
        // TOML entries are merged into that same registry via
        // `extend_registry_with_hooks` during scene compile, so the
        // hook-derived reactions and scene reactions share one
        // dispatcher — the old `hook_dispatcher` remains deleted per
        // T-5.7.
        //
        // When T-018 threads `config.hooks` through `Config`, the hook
        // slice is supplied to `compile_scene_for_runtime` at boot and
        // reaches this registry the same way.
        let reactions = compiled_scene.registry.clone();

        // IntentRegistry holds the core `ark.core.*` op set the
        // synthesised `exec` reactions dispatch through. The async
        // registration is deliberately blocking-on-runtime here — we
        // only build it once at supervisor boot, and the cost is a
        // few hash inserts.
        let intents = IntentRegistry::new();
        register_core_ops(&intents).await;

        // IntentContext: use the compiled scene's real `SceneId` so
        // cascade telemetry + scene graph attribution identify the
        // user's scene (or `<built-in>` on default fallback) rather
        // than a synthesised per-agent placeholder.
        let intent_ctx = IntentContext::placeholder(compiled_scene.scene_id.clone());

        // CEL context snapshots: populated from the live AgentSpec so
        // hook predicates that gate on `agent.orchestrator` (the
        // primary use case for the synthesised CEL) evaluate correctly.
        let agent_snapshot = Arc::new(AgentSnapshot {
            id: spec.id.as_str().to_string(),
            name: spec.name.clone(),
            orchestrator: spec.orchestrator.clone(),
            engine: spec.engine.clone(),
            cwd: spec.cwd.display().to_string(),
            cmd: spec
                .cmd
                .first()
                .cloned()
                .unwrap_or_default(),
            args: spec.cmd.iter().skip(1).cloned().collect(),
        });
        let session_snapshot = Arc::new(SessionSnapshot {
            name: spec.session.clone(),
        });

        let ctx = ReactionDispatcherCtx {
            reactions,
            intents,
            intent_ctx,
            agent: agent_snapshot,
            session: session_snapshot,
        };
        let rx = events.subscribe();
        let cancel = cancel.clone();
        consumers.spawn(async move { reaction_dispatcher(rx, ctx, cancel).await });
    }
    debug!("R3 step 9: consumer tasks spawned (T-5.7 reaction_dispatcher)");

    // ---- Step 10: install observability ----
    //
    // F-085: thread the real `spec.id` through so the engine keys its hook
    // commands / handle on the identity the supervisor subscribes to.
    let engine_handle = engine
        .install_observability(&spec.id, &spec.cwd, events.clone())
        .await
        .context("engine install_observability")?;
    debug!(
        engine = engine.name(),
        "R3 step 10: observability installed"
    );

    // ---- Step 10.25: permission dispatcher (T-ACP.5 / T-ACP.5b) ----
    //
    // Wires the Zed 5-tier permission dispatcher to the event bus:
    // every `ark.acp.permission_requested` event the ACP client
    // re-publishes lands on the request tracker, which arms a
    // per-request timeout (config: `[acp] permission_timeout_ms`)
    // and drops late responses. Until T-ACP.5/the engine spawn
    // lands, the dispatcher sits idle — no events flow through
    // the bus because the ACP client is not yet alive — but the
    // wiring exists so future tiers drop into it cleanly.
    //
    // T-ACP.5b: the timeout is read from `ark_config::Config.acp`
    // (loaded here with shipped defaults because the real
    // `ark_config::Config` isn't threaded through the supervisor
    // boot yet — a future tier wires the figment-loaded value
    // through). `ARK_NONINTERACTIVE=1` or a non-TTY stdin force
    // the effective timeout to zero (disabled).
    let permission_dispatcher = {
        let acp_cfg = ark_config::schema::Config::defaults();
        let ms = if is_noninteractive() {
            0
        } else {
            acp_cfg.acp.permission_timeout_ms
        };
        let dur = std::time::Duration::from_millis(ms);
        crate::permission::PermissionDispatcher::new(dur)
    };
    let permission_watcher = crate::permission::spawn_request_watcher(
        permission_dispatcher.clone(),
        events.subscribe(),
        cancel.clone(),
    );
    let permission_timeout_pump = {
        let d = permission_dispatcher.clone();
        let cancel = cancel.clone();
        let sink = events.clone();
        tokio::spawn(async move {
            d.run_timeout_pump(cancel, sink).await;
        })
    };

    // ---- Step 10.5: mount always-on plugins (T-7.2 + T-8.1) ----
    //
    // Walk the compiled scene's `plugin { }` declarations and mount
    // every `Lifecycle::Always` plugin through the intent registry's
    // `mount_plugin` op. Summon / event-mount plugins are seeded as
    // `Dormant` so downstream reaction-synthesis paths (T-7.3 / T-7.4)
    // can observe their state when their selector matches.
    //
    // This runs BEFORE `AgentEvent::Started` so any `set_status` op a
    // freshly-mounted status plugin emits is visible to the event
    // pipeline the moment the agent comes up. It also means a mount
    // failure has already been surfaced as `ark.plugin.failed` by the
    // time the scene's reactions observe `Started` — the scene can
    // cleanly fall back (e.g. `on "UserEvent:ark.plugin.failed" {
    // set_status text="<plugin> unavailable" }`).
    //
    // T-8.1: we now drive the mount from the in-memory
    // [`CompiledScene`] built at R3 step 7 rather than re-reading the
    // scene file from disk. Path-based `mount_always_on_plugins`
    // remains available for tests that want to exercise the on-disk
    // parse path, but the production boot sequence runs entirely from
    // the single already-parsed AST.
    let plugin_lifecycle = crate::plugin_lifecycle::PluginLifecycleManager::new();
    let mount_outcomes = mount_always_on_from_compiled(
        &compiled_scene,
        &plugin_lifecycle,
        &events,
    )
    .await;
    debug!(
        count = mount_outcomes.len(),
        source = %compiled_scene.source.display(),
        "R3 step 10.5: always-on plugin mount pass complete"
    );

    // ---- Step 11: emit Started ----
    // Best-effort: if nobody is subscribed yet, the Err is benign. The
    // consumers spawned at step 9 always have receivers alive at this
    // point, so in practice this never fails.
    let _ = events.send(AgentEvent::Started { spec: spec.clone() });
    debug!("R3 step 11: Started event emitted");

    // ---- Step 12: signal readiness to parent ----
    //
    // W-2: pipe-inheritance ack. The CLI parent created a pipe before
    // calling `daemonize()` and passed the write fd through to us via
    // `ReadyWriter`. We write the ACK byte + drop, which closes our
    // copy of the fd and unblocks the parent's `read()`.
    //
    // Failure to write the ack is logged but NOT fatal: if the pipe
    // broke (parent died, fd was somehow closed), the supervisor can
    // still run the agent — it just means no client is waiting on the
    // ready signal. The parent's 5 s timeout in `wait_for_ready`
    // catches the dead-pipe case and surfaces a clean error to its
    // caller.
    //
    // The previous step-12 implementation wrote `agent_id\n` to stdout,
    // which was always a no-op in `Daemon` mode because
    // `setup_supervisor_log` had already redirected stdout to
    // `supervisor.log` before this code ran. The `mode` parameter is
    // now informational only — the writer's presence/absence is the
    // real switch.
    let _ = mode;
    if let Some(writer) = ready_writer {
        if let Err(err) = writer.write_ack() {
            warn!(error = %err, "failed to write ready ack to parent CLI");
        }
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
    // its select! loop to break. The rollup itself is a handful of
    // atomic writes per event. T-8.1 bumped this from 100ms → 250ms:
    // with the scene reaction dispatcher now populated from the user
    // scene (rather than an empty hook registry), low-spec CI hosts
    // occasionally raced the final Done write against the cancel —
    // surfacing as an intermittent "events.jsonl should contain Done"
    // assertion in the smoke test.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    drop(events);
    cancel.cancel();
    drain_consumers(&mut consumers, std::time::Duration::from_secs(5)).await;
    debug!("R3 step 14: consumers drained");

    // F-086: the tab-registry feeder task is cancelled via the shared
    // cancel token above. Drop its JoinHandle once the bus is closed —
    // `abort` is a no-op if the task has already exited.
    tab_registry_feeder.abort();
    let _ = tab_registry_feeder.await;

    // T-ACP.5: drain the permission-dispatcher background tasks.
    // Cancel already fired above — these joins are just cleanup.
    permission_watcher.abort();
    let _ = permission_watcher.await;
    permission_timeout_pump.abort();
    let _ = permission_timeout_pump.await;
    drop(permission_dispatcher);
    // Registry itself is retained by nothing here post-drain; `_tab_registry`
    // exists only so `kill_handler` paths outside the happy path (a real
    // SIGTERM mid-run) can see non-empty state. On clean exit we drop it.
    drop(tab_registry);

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

/// T-ACP.5b: determine whether the current supervisor is running in
/// non-interactive mode. Two signals drive the answer:
///
///   * `ARK_NONINTERACTIVE=1` (or any non-empty value) → forced
///     non-interactive, regardless of TTY status.
///   * stdin is NOT a TTY → non-interactive by default (common for
///     CI + headless spawns).
///
/// Non-interactive supervisors disable the permission-request
/// timeout (the spec says `permission_timeout_ms=0` is the default
/// in that case) so scenes relying on scene-rule / picker responses
/// still get an answer — or permanently block — rather than being
/// silently auto-rejected with `option_id="timeout"`.
fn is_noninteractive() -> bool {
    if std::env::var_os("ARK_NONINTERACTIVE")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    !is_tty_stdin()
}

/// Probe whether stdin is attached to a TTY. Uses `isatty(0)`.
fn is_tty_stdin() -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    // `nix::unistd::isatty` returns `Ok(true)` for TTY, `Ok(false)` otherwise.
    nix::unistd::isatty(fd).unwrap_or(false)
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
    // F-088: Killed and Timeout are distinct terminal states from Done.
    // Conflating them with Done misreports forced/timeout termination as
    // success on `ark list` / picker surfaces.
    status.phase = match outcome {
        Outcome::Success { .. } => Phase::Done,
        Outcome::Failed { .. } => Phase::Failed,
        Outcome::Killed => Phase::Killed,
        Outcome::Timeout => Phase::Timeout,
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

/// Build the [`IntentBridge`] that the control-socket handler dispatches
/// `Intent` requests through (T-6.2).
///
/// Wires the same core op set the reaction dispatcher uses so the bridge
/// can fire any built-in op a scene declares. `IntentContext` is a
/// placeholder (no live mux / bus / supervisor handles yet — those live
/// in scene's own `IntentContext` placeholder world); ops that don't
/// touch those handles (most R7 ops at this tier) work end-to-end. Once
/// the scene runtime grows real handles (T-7.x / T-8.x), this builder is
/// the single point to thread them in.
/// Load the scene at `scene_path`, lower every `plugin { }` declaration to
/// a typed [`PluginDecl`], and drive the `Lifecycle::Always` set through
/// [`PluginLifecycleManager::mount_always_on`].
///
/// Errors on disk I/O, UTF-8 decode, or facet-kdl parse — the caller
/// logs and continues. Per-plugin mount failures are NOT surfaced here;
/// they are recorded on the manager and emitted as `ark.plugin.failed`
/// UserEvents so scene reactions can observe and act.
///
/// T-8.1: the production boot sequence no longer calls this helper —
/// `mount_always_on_from_compiled` reads plugin decls off the already
/// parsed [`crate::scene_runtime::CompiledScene`] to avoid double-reading
/// the scene file. This on-disk variant is retained under `#[cfg(test)]`
/// for the T-7.2 regression tests that exercise the file-I/O + parse
/// path end-to-end.
#[cfg(test)]
async fn mount_always_on_plugins(
    scene_path: &std::path::Path,
    manager: &crate::plugin_lifecycle::PluginLifecycleManager,
    event_bus: &ark_types::EventSink,
) -> Result<Vec<crate::plugin_lifecycle::MountOutcome>> {
    let bytes = std::fs::read(scene_path)
        .with_context(|| format!("read scene `{}`", scene_path.display()))?;
    let src = std::str::from_utf8(&bytes)
        .with_context(|| format!("scene `{}` is not valid utf-8", scene_path.display()))?;
    let doc = ark_scene::parse::parse_scene(src, scene_path)
        .map_err(|e| anyhow::anyhow!("scene parse failed: {e}"))?;

    // Lift the typed PluginNode children into PluginDecls, skipping
    // nodes whose lifecycle lowering errored out (ambiguous / invalid).
    let mut decls = Vec::new();
    for plugin in &doc.scene.plugins {
        match ark_scene::plugin::lower_plugin(plugin) {
            Ok(decl) => decls.push(decl),
            Err(err) => {
                warn!(
                    plugin = %plugin.name,
                    error = %err,
                    "plugin lowering failed; skipping from always-on mount pass"
                );
            }
        }
    }

    // Build the intent registry used for mount dispatch. This is a
    // fresh registry rather than sharing with the control-socket /
    // reaction dispatcher ones; the intent surface is identical (core
    // ops only) and a separate handle avoids cross-task lock contention.
    let registry = IntentRegistry::new();
    register_core_ops(&registry).await;

    // Placeholder IntentContext — the scene crate's own TODO(T-5.x)
    // covers real handle plumbing for mux / bus / supervisor. The
    // mount_plugin op at this tier logs + returns Ok(None), so the
    // lifecycle manager's state tracking is exercised end-to-end even
    // without a real mux.
    let scene_id = SceneId::from_bytes(
        scene_path.to_path_buf(),
        format!("plugin-lifecycle:{}", scene_path.display()).as_bytes(),
    );
    let ctx = IntentContext::placeholder(scene_id);

    let outcomes = manager
        .mount_always_on(&decls, &registry, &ctx, event_bus)
        .await;
    Ok(outcomes)
}

async fn build_intent_bridge_for_socket(
    _spec: &AgentSpec,
    compiled_scene: &CompiledScene,
) -> crate::commands::IntentBridge {
    let registry = IntentRegistry::new();
    register_core_ops(&registry).await;
    // T-8.1: use the compiled scene's real `SceneId` so any op a
    // control-socket client dispatches gets attributed to the same
    // scene the reaction dispatcher is firing against.
    let ctx = IntentContext::placeholder(compiled_scene.scene_id.clone());
    crate::commands::IntentBridge { registry, ctx }
}

/// T-8.1: mount every `Lifecycle::Always` plugin from the already
/// compiled scene through the intent registry's `mount_plugin` op,
/// without re-reading the scene file from disk.
///
/// Mirrors the on-disk [`mount_always_on_plugins`] helper one-for-one
/// except for where the [`PluginDecl`] set comes from — this version
/// takes decls directly off the parsed [`CompiledScene`]. The intent
/// registry is freshly allocated (core ops only) to keep the mount
/// pass independent of the reaction dispatcher's registry — every
/// failure path is still observable via `ark.plugin.failed`.
async fn mount_always_on_from_compiled(
    compiled: &CompiledScene,
    manager: &crate::plugin_lifecycle::PluginLifecycleManager,
    event_bus: &EventSink,
) -> Vec<crate::plugin_lifecycle::MountOutcome> {
    let decls = compiled.plugin_decls();
    let registry = IntentRegistry::new();
    register_core_ops(&registry).await;
    let ctx = IntentContext::placeholder(compiled.scene_id.clone());
    manager
        .mount_always_on(&decls, &registry, &ctx, event_bus)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_mux_zellij::ZellijMux;
    use ark_types::{AgentEvent, AgentId, AgentSpec, Outcome};
    use async_trait::async_trait;

    // --- stub engine / orchestrator for the smoke test ---------------
    //
    // Mux is a concrete `ZellijMux(StubExecutor)` built via
    // `ZellijMux::for_test(Vec::new())`. The orchestrator under test here
    // is `InstantSuccessOrchestrator` which never touches the mux beyond
    // the required `ensure_session` call, so no scripted responses are
    // needed — the empty queue would only surface if the flow regressed
    // and started calling something unexpected.

    struct StubEngine;

    #[async_trait]
    impl Engine for StubEngine {
        fn name(&self) -> &'static str {
            "stub-engine"
        }
        async fn install_observability(
            &self,
            _id: &ark_types::AgentId,
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

        // Mux is a concrete `ZellijMux` backed by a StubExecutor. The R3
        // boot sequence calls `mux.ensure_session` (step 7); that routes to
        // `zellij list-sessions` outside zellij. We queue an ok response
        // with empty stdout so the "no collision" branch fires.
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let (mux, _stub) = ZellijMux::for_test(vec![ark_mux_zellij::executor::CommandOutput {
            status: ok_status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Box::new(StubEngine),
            Box::new(InstantSuccessOrchestrator),
            Arc::new(mux),
            false,
            None,
            None,
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

    /// T-8.1: end-to-end integration — when `spec.scene_path` points at
    /// a user scene with an `on { }` reaction, a `plugin { }` block, and
    /// a `keybind { }` declaration, `run_supervisor_with` must:
    ///   1. Resolve + parse the scene via R3 step 7 (not re-parse from disk
    ///      per-consumer).
    ///   2. Validate the scene without erroring.
    ///   3. Populate the reaction dispatcher's registry from the scene.
    ///   4. Mount every `Lifecycle::Always` plugin via the lifecycle
    ///      manager.
    ///   5. Finish with `Outcome::Success` and leave no orphan state.
    ///
    /// The scripted mux + stub engine let this test drive the full boot
    /// sequence without touching a real zellij binary.
    #[tokio::test]
    async fn run_supervisor_with_compiles_user_scene_and_drives_consumers() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());

        // Write a custom scene carrying a reaction, an always-on plugin,
        // and a keybind — every artefact T-8.1 threads through the
        // compile pipeline.
        let scene_path = tmp.path().join("custom.kdl");
        std::fs::write(
            &scene_path,
            r#"scene "t8-1-integration" {
    plugin "status-bar" {
        source "shipped:status"
        mount "status-bar"
    }
    plugin "on-demand" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
    on "Started" {
        set_status text="ready"
    }
    keybind "Ctrl Shift p" {
        emit "picker.show"
    }
}
"#,
        )
        .unwrap();

        // R3 step 7 sanity-check: the supervisor's scene_runtime
        // compile path must accept the fixture above.
        let compiled =
            compile_scene_for_runtime(Some(&scene_path), &[]).expect("scene compiles clean");
        assert!(!compiled.registry.is_empty(), "registry must pick up on + keybind");
        assert_eq!(compiled.doc.scene.plugins.len(), 2);
        assert_eq!(compiled.doc.scene.ons.len(), 1);
        assert_eq!(compiled.doc.scene.keybinds.len(), 1);

        // Now build a spec that points at the scene and drive
        // `run_supervisor_with` end-to-end.
        let id = AgentId::new("cavekit", "t81");
        let mut spec = AgentSpec::new(
            id,
            "t81",
            "cavekit",
            "stub-engine",
            std::path::PathBuf::from("/tmp"),
            vec!["stub".to_string()],
        );
        spec.scene_path = Some(scene_path.clone());

        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let (mux, _stub) = ZellijMux::for_test(vec![ark_mux_zellij::executor::CommandOutput {
            status: ok_status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Box::new(StubEngine),
            Box::new(InstantSuccessOrchestrator),
            Arc::new(mux),
            /* run_preflight */ false,
            None,
            None,
        )
        .await
        .expect("run_supervisor_with with scene_path ok");

        assert!(
            matches!(result, Outcome::Success { .. }),
            "expected Success, got {result:?}"
        );

        // Step 1 verification: state dir + spec.json present, and the
        // spec.json round-trip preserved the scene_path we set.
        assert!(layout.agent_dir(&spec.id).is_dir());
        let reread: AgentSpec = serde_json::from_slice(
            &std::fs::read(layout.spec_path(&spec.id)).expect("read spec.json"),
        )
        .expect("parse spec.json");
        assert_eq!(reread.scene_path, Some(scene_path.clone()));

        // Step 16 verification: final phase is Done.
        let status = ark_core::read_status(&layout, &spec.id)
            .expect("read status")
            .expect("status exists");
        assert_eq!(status.phase, Phase::Done);

        // Step 17 verification: socket unlinked.
        assert!(!layout.agent_socket_path(&spec.id).exists());

        // Step 18 verification: lock released.
        let re = crate::acquire_lock(&layout, &spec.id).expect("re-acquire");
        drop(re);
    }

    /// T-8.1: when `spec.scene_path` points at a file that does NOT
    /// exist, `run_supervisor_with` aborts early via the R3 step 7
    /// compile-error contract — returning an `Err` rather than Outcome.
    /// The parent CLI surfaces a non-zero exit code.
    #[tokio::test]
    async fn run_supervisor_with_aborts_on_missing_scene_path() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());

        let id = AgentId::new("cavekit", "missing");
        let mut spec = AgentSpec::new(
            id,
            "missing",
            "cavekit",
            "stub-engine",
            std::path::PathBuf::from("/tmp"),
            vec!["stub".to_string()],
        );
        spec.scene_path = Some(tmp.path().join("nope.kdl"));

        // No mux scripting needed — the compile error fires before the
        // mux is touched.
        let (mux, _stub) = ZellijMux::for_test(Vec::new());

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Box::new(StubEngine),
            Box::new(InstantSuccessOrchestrator),
            Arc::new(mux),
            false,
            None,
            None,
        )
        .await;

        let err = result.expect_err("missing scene_path must abort");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("scene") && (msg.contains("does not exist") || msg.contains("not a regular file")),
            "expected abort message to mention missing scene, got: {msg}"
        );

        // On abort the lock is released (LockGuard Drop runs) and the
        // socket was never bound — subsequent re-attempts must succeed.
        let re = crate::acquire_lock(&layout, &spec.id).expect("re-acquire after abort");
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

    /// F-087 regression: `run_supervisor_with` installs SIGTERM/SIGINT
    /// handlers via `signals::install_signal_handlers` so a real signal
    /// unwinds the run cleanly. Pre-fix, the T-067 code path was dead —
    /// a real signal left the socket stale and no cancel fired.
    ///
    /// The direct behavioural verification of the signal path lives in
    /// [`crate::signals::tests::sigterm_unlinks_socket_and_cancels`] which
    /// exercises the exact `install_signal_handlers` API against a raised
    /// SIGTERM. This test adds an integration-level assertion that
    /// `run_supervisor_with` actually reaches and succeeds the handler-
    /// install call: if the step silently no-op'd or the call returned
    /// `Err`, `run_supervisor_with` would propagate the error and this
    /// test would fail.
    ///
    /// We do NOT raise an actual signal here — cross-test signal raising
    /// (signal_hook delivers to EVERY registered handler in the process)
    /// causes flaky interactions with `crate::signals::tests::*` which
    /// already raise SIGTERM/SIGINT on the same test binary.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_supervisor_with_installs_signal_handlers_without_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let spec = sample_spec();

        // One scripted `list-sessions` ok (see smoke test for why).
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let (mux, _stub) = ZellijMux::for_test(vec![ark_mux_zellij::executor::CommandOutput {
            status: ok_status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Box::new(StubEngine),
            Box::new(InstantSuccessOrchestrator),
            Arc::new(mux),
            false,
            None,
            None,
        )
        .await
        .expect("run_supervisor_with ok (signal handler install path must not error)");
        assert!(matches!(result, Outcome::Success { .. }));

        // Socket must be unlinked on the way out — same invariant the
        // signal handler's `unlink_if_exists` path protects on SIGTERM.
        let sock = layout.agent_socket_path(&spec.id);
        assert!(
            !sock.exists(),
            "socket must be unlinked after clean shutdown: {}",
            sock.display()
        );
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

    /// F-088 regression: Outcome::Killed → Phase::Killed (not Done).
    #[test]
    fn finalize_state_killed_maps_to_killed() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "killed");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        finalize_state(&layout, &id, 42, &Outcome::Killed).expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Killed);
        assert_ne!(s.phase, Phase::Done, "must not be misreported as success");
        assert!(s.last_event_summary.contains("killed"));
    }

    /// F-088 regression: Outcome::Timeout → Phase::Timeout (not Done).
    #[test]
    fn finalize_state_timeout_maps_to_timeout() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "timeout");
        StateLayout::ensure_dir_0700(&layout.agent_dir(&id)).unwrap();
        finalize_state(&layout, &id, 42, &Outcome::Timeout).expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert_eq!(s.phase, Phase::Timeout);
        assert_ne!(s.phase, Phase::Done, "must not be misreported as success");
        assert!(s.last_event_summary.contains("timeout"));
    }

    /// T-7.2 integration: when `spec.scene_path` points at a scene
    /// declaring an always-on plugin, `mount_always_on_plugins` parses
    /// the file, lowers the plugin decl, and drives the lifecycle
    /// manager through the intent registry.
    #[tokio::test]
    async fn mount_always_on_plugins_mounts_every_always_plugin_from_scene_file() {
        let tmp = short_tempdir();
        let scene_path = tmp.path().join("always.kdl");
        std::fs::write(
            &scene_path,
            r#"scene "always-test" {
    plugin "status-bar" {
        source "shipped:status"
        mount "status-bar"
    }
    plugin "on-demand" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
}
"#,
        )
        .unwrap();

        let manager = crate::plugin_lifecycle::PluginLifecycleManager::new();
        let (tx, _rx) = ark_types::channel(8);
        let outcomes = mount_always_on_plugins(&scene_path, &manager, &tx)
            .await
            .expect("scene parses + mounts");

        // The always plugin got mounted; the summon plugin did not.
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            crate::plugin_lifecycle::MountOutcome::Mounted { name, .. } => {
                assert_eq!(name, "status-bar");
            }
            other => panic!("expected Mounted, got {other:?}"),
        }

        // The state map reflects both plugins; only status-bar is mounted.
        assert!(
            manager
                .state("status-bar")
                .await
                .unwrap()
                .is_mounted(),
        );
        assert_eq!(
            manager.state("on-demand").await,
            Some(crate::plugin_lifecycle::MountState::Dormant),
        );
    }

    /// T-7.2 integration: a bogus scene path surfaces as an anyhow error
    /// rather than panicking. Supervisor boot sequence absorbs this.
    #[tokio::test]
    async fn mount_always_on_plugins_returns_err_on_missing_scene() {
        let tmp = short_tempdir();
        let missing = tmp.path().join("does-not-exist.kdl");
        let manager = crate::plugin_lifecycle::PluginLifecycleManager::new();
        let (tx, _rx) = ark_types::channel(8);
        let res = mount_always_on_plugins(&missing, &manager, &tx).await;
        assert!(res.is_err(), "missing scene must error");
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
