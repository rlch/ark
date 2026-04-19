//! `run_supervisor` — the R3 boot sequence (cavekit-soul Phase 1 rewrite).
//!
//! Implements the slimmed cavekit-supervisor.md R3 lifecycle. Callers
//! (the daemon fork entry from T-062 or the foreground path from T-063)
//! have already run logging setup + fork/detach; `run_supervisor` picks
//! up from there with the session spec + config in hand and drives the
//! rest of the lifecycle.
//!
//! ## Shape of the return
//!
//! [`run_supervisor`] returns `Result<(), anyhow::Error>` — `Ok(())` on a
//! clean run; `Err` signals that the supervisor infrastructure itself
//! could not start or could not complete. Callers (daemon path in
//! `ark-cli` / foreground path) derive their Unix exit code with
//! `match result { Ok(()) => 0, Err(_) => 1 }`. Methodology-flavoured
//! "outcome" semantics now re-home inside extensions in Phase 2+.

use std::sync::Arc;

use anyhow::{Context, Result};
use ark_core::consumers::{ReactionDispatcherCtx, reaction_dispatcher, state_writer};
use ark_core::status_writer::write_session_status_atomic;
use ark_core::{Config, World};
// cleanup-T-009: Engine + Orchestrator trait objects are gone from the
// runtime boot path — `run_supervisor_with` no longer accepts them, so the
// trait imports here were removed alongside the R3 step-6 diagnostic and
// the step-10/15 observability + teardown branches. `World` stays because
// the bare-session `world.cancel.cancelled().await` park still needs it;
// T-010 retires `World` along with the traits.
use ark_mux_zellij::ZellijMux;
use ark_scene::context::SessionSnapshot;
use ark_scene::hook_compat::HookEntry as SceneHookEntry;
use ark_scene::intent::{IntentContext, IntentRegistry};
use ark_scene::ops::register_core_ops;
use ark_types::{
    CancellationToken, CoreEvent, EventSink, SessionId, SessionSpec, SessionStatus, StateLayout,
    channel,
};
use chrono::Utc;
use std::collections::BTreeMap;
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

use crate::commands::{SupervisorCommandCtx, SupervisorCommandHandler};
use crate::consumers::status_pipe;
use crate::control_socket::{ControlCommandHandler, bind_control_socket, shutdown};
use crate::ready_signal::ReadyWriter;
use crate::scene_runtime::{CompiledScene, compile_scene_for_runtime};
use crate::signals::install_signal_handlers;

/// Which boot path reached [`run_supervisor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupervisorMode {
    Daemon,
    Foreground,
}

/// Event-bus capacity used when `Config` does not yet carry one.
const DEFAULT_EVENT_BUS_CAPACITY: usize = 256;

/// Run the supervisor to completion.
pub async fn run_supervisor(
    spec: SessionSpec,
    mode: SupervisorMode,
    config: Config,
    ready_writer: Option<ReadyWriter>,
    external_cancel: Option<CancellationToken>,
) -> Result<()> {
    let state_layout = StateLayout::from_env().context("resolve state layout")?;
    // cleanup-T-008: `build_multiplexer` factory was deleted; v1 is locked
    // to a single mux (`MUX_V1 = ["zellij"]`), so instantiate directly.
    // Adding a second concrete mux becomes a local edit here, not a factory
    // extension.
    let mux: Arc<ZellijMux> = Arc::new(ZellijMux::new());
    run_supervisor_with(
        spec,
        mode,
        config,
        state_layout,
        mux,
        ready_writer,
        external_cancel,
    )
    .await
}

/// Variant of [`run_supervisor`] that accepts an injected `StateLayout`
/// and `ZellijMux` for testability. Production callers reach this via
/// [`run_supervisor`].
///
/// cleanup-T-009: the legacy `engine: Option<Box<dyn Engine>>` +
/// `orchestrator: Option<Box<dyn Orchestrator>>` + `run_preflight: bool`
/// parameters were removed. Every production caller passed
/// `engine = None`, `orchestrator = None`, `run_preflight = true`, and
/// `engine_stub::preflight` was a no-op.
#[allow(clippy::too_many_arguments)]
pub async fn run_supervisor_with(
    spec: SessionSpec,
    mode: SupervisorMode,
    config: Config,
    state_layout: StateLayout,
    mux: Arc<ZellijMux>,
    ready_writer: Option<ReadyWriter>,
    external_cancel: Option<CancellationToken>,
) -> Result<()> {
    nuke_legacy_agents_dir(&state_layout);

    // ---- Step 1: StateDir + spec.json + initial status.json ----
    let session_dir = state_layout.session_dir(&spec.id);
    StateLayout::ensure_dir_0700(&session_dir).context("ensure session state dir")?;
    write_spec_json(&state_layout, &spec).context("write spec.json")?;
    let supervisor_pid = std::process::id();
    write_pid_file(&state_layout, &spec.id, supervisor_pid).context("write pid file")?;
    let initial_status = SessionStatus {
        id: spec.id.clone(),
        started_at: Utc::now(),
        terminated_at: None,
        ext_state: BTreeMap::new(),
    };
    write_session_status_atomic(&state_layout, &spec.id, &initial_status)
        .context("write initial status.json")?;
    debug!(session = %spec.id.as_str(), "R3 step 1: state dir ready");

    // ---- Step 2: acquire exclusive file lock ----
    let lock_guard = crate::lock::acquire_lock(&state_layout, &spec.id)
        .context("acquire per-session file lock")?;
    debug!(path = %lock_guard.path().display(), "R3 step 2: lock acquired");

    let state_arc: Arc<StateLayout> = Arc::new(state_layout.clone());
    let config_arc: Arc<Config> = Arc::new(config.clone());

    let cancel = external_cancel.unwrap_or_default();

    let (events, _boot_rx) = channel(DEFAULT_EVENT_BUS_CAPACITY);

    // ---- Step 7 (early): compile scene ----
    let hook_entries: Vec<SceneHookEntry> = Vec::new();
    let compiled_scene: Arc<CompiledScene> =
        match compile_scene_for_runtime(spec.scene_path.as_deref(), &hook_entries) {
            Ok(c) => {
                debug!(
                    source = %c.source.display(),
                    reactions = c.registry.len(),
                    "R3 step 7: scene compiled"
                );
                Arc::new(c)
            }
            Err(err) => {
                tracing::error!(
                    target: "scene::compile",
                    session = %spec.id.as_str(),
                    scene = ?spec.scene_path,
                    error = %err,
                    "scene compile failed; aborting spawn"
                );
                return Err(err.context("scene compile (R3 step 7)"));
            }
        };

    // ---- Step 3: bind control socket ----
    let intent_bridge = build_intent_bridge_for_socket(&spec, &compiled_scene).await;
    let command_handler: Arc<dyn ControlCommandHandler> =
        Arc::new(SupervisorCommandHandler::new(SupervisorCommandCtx {
            agent_id: spec.id.clone(),
            state_layout: state_layout.clone(),
            pid: nix::unistd::Pid::from_raw(supervisor_pid as i32),
            cancel: cancel.clone(),
            event_bus: events.clone(),
            audit: None,
            intents: Some(intent_bridge),
        }));
    let socket_handle = bind_control_socket(&state_layout, &spec.id, command_handler.clone())
        .await
        .context("bind control socket")?;
    debug!(path = %socket_handle.path().display(), "R3 step 3: control socket bound");

    let _signal_task = install_signal_handlers(socket_handle.path().to_path_buf(), cancel.clone())
        .await
        .context("install signal handlers")?;
    debug!("F-087: SIGTERM/SIGINT handlers installed");

    // ---- Step 6: factory (deleted per cleanup T-009; mux is the sole survivor) ----
    debug!(mux = mux.kind(), "R3 step 6: mux resolved");

    // ---- Step 7b: ensure mux session ----
    let session_name = format!("ark-{}", spec.id.as_path_leaf());
    mux.ensure_session(&session_name)
        .await
        .with_context(|| format!("mux.ensure_session({session_name})"))?;

    // ---- Step 8: preflight (engine_stub::preflight was a no-op; inlined to nothing per T-009) ----

    // ---- Step 9: spawn consumer tasks ----
    let mut consumers: JoinSet<Result<()>> = JoinSet::new();

    {
        let rx = events.subscribe();
        let tx_for_state = events.clone();
        let state_arc = state_arc.clone();
        let id = spec.id.clone();
        let cancel = cancel.clone();
        consumers.spawn(async move {
            state_writer(rx, Some(tx_for_state), state_arc, id, cancel).await
        });
    }

    {
        let rx = events.subscribe();
        let mux = mux.clone();
        let cancel = cancel.clone();
        consumers.spawn(async move { status_pipe(rx, mux, cancel).await });
    }

    {
        let reactions = compiled_scene.registry.clone();

        let mut intents = IntentRegistry::new();
        register_core_ops(&mut intents);

        let intent_ctx = IntentContext::new(compiled_scene.scene_id.clone(), "scene");

        let session_snapshot = Arc::new(SessionSnapshot {
            id: spec.id.clone(),
            name: spec.name.clone(),
            cwd: spec.cwd.clone(),
            started_at: spec.created_at,
            extensions: BTreeMap::new(),
        });

        let ctx = ReactionDispatcherCtx {
            reactions,
            intents: Arc::new(intents),
            intent_ctx,
            session: session_snapshot,
            bus: events.clone(),
            state: state_arc.clone(),
            session_id: spec.id.clone(),
            max_cascade_depth: compiled_scene.max_cascade_depth,
        };
        let rx = events.subscribe();
        let cancel = cancel.clone();
        consumers.spawn(async move { reaction_dispatcher(rx, ctx, cancel).await });
    }
    debug!("R3 step 9: consumer tasks spawned");

    // ---- Step 10: install observability (engine deleted per cleanup T-010; unconditionally skipped) ----
    debug!("R3 step 10: no engine; skipping observability");

    // ---- Step 10.5: mount always-on plugins ----
    let plugin_lifecycle = crate::plugin_lifecycle::PluginLifecycleManager::new();
    let mount_outcomes =
        mount_always_on_from_compiled(&compiled_scene, &plugin_lifecycle, &events).await;
    debug!(
        count = mount_outcomes.len(),
        source = %compiled_scene.source.display(),
        "R3 step 10.5: always-on plugin mount pass complete"
    );

    // ---- Step 11: emit SessionStarted ----
    let _ = events.send(CoreEvent::SessionStarted { spec: spec.clone() });
    debug!("R3 step 11: SessionStarted event emitted");

    // ---- Step 12: signal readiness to parent ----
    let _ = mode;
    if let Some(writer) = ready_writer {
        if let Err(err) = writer.write_ack() {
            warn!(error = %err, "failed to write ready ack to parent CLI");
        }
    }
    info!(session = %spec.id.as_str(), "supervisor ready");

    // ---- Step 13: orchestrator.run (long-running) — or park on cancel ----
    let hooks_dir = state_layout.session_hooks_dir(&spec.id);
    let world = World::new(
        mux.clone(),
        events.clone(),
        cancel.clone(),
        hooks_dir,
        state_arc.clone(),
        config_arc.clone(),
    );

    // Orchestrator trait deleted per cleanup T-010; bare-session is now the only path.
    debug!("R3 step 13: no orchestrator; parking on world.cancel.cancelled().await");
    world.cancel.cancelled().await;
    debug!("R3 step 13: cancel observed on bare-session path");

    // Final SessionEnded event so consumers observe a terminal record.
    let _ = events.send(CoreEvent::SessionEnded {
        terminated_at: Utc::now(),
        exit: ark_types::ExitReason::Normal,
    });

    // ---- Step 14: drain consumer tasks ----
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    drop(events);
    cancel.cancel();
    drain_consumers(&mut consumers, std::time::Duration::from_secs(5)).await;
    debug!("R3 step 14: consumers drained");

    // ---- Step 15: engine teardown (engine deleted per cleanup T-010; skipped) ----

    // ---- Step 16: finalize state ----
    if let Err(err) = finalize_state(&state_layout, &spec.id, supervisor_pid) {
        warn!(error = %err, "finalize_state failed — status.json may be stale");
    }
    debug!("R3 step 16: state finalised");

    // ---- Step 17: unlink control socket ----
    if let Err(err) = shutdown(socket_handle).await {
        warn!(error = %err, "control socket shutdown failed");
    }
    debug!("R3 step 17: control socket torn down");

    // ---- Step 18: release lock (drop guard on return) ----
    drop(lock_guard);
    debug!("R3 step 18: lock released");

    Ok(())
}

/// Phase 1 migration: delete legacy `$STATE/agents/` on boot.
fn nuke_legacy_agents_dir(layout: &StateLayout) {
    let agents_path = layout.base().join("agents");
    match agents_path.try_exists() {
        Ok(true) => match std::fs::remove_dir_all(&agents_path) {
            Ok(()) => info!(
                path = %agents_path.display(),
                "nuked legacy $STATE/agents/ directory (cavekit-soul Phase 1 migration)"
            ),
            Err(err) => warn!(
                path = %agents_path.display(),
                %err,
                "failed to remove legacy $STATE/agents/ directory; continuing"
            ),
        },
        Ok(false) => {}
        Err(err) => warn!(
            path = %agents_path.display(),
            %err,
            "legacy $STATE/agents/ existence check failed; skipping migration"
        ),
    }
}

/// Write the authoritative `spec.json` under the session state dir.
fn write_spec_json(layout: &StateLayout, spec: &SessionSpec) -> std::io::Result<()> {
    let path = layout.session_spec_path(&spec.id);
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

/// Write the supervisor pid to `$STATE/sessions/{id}/pid`.
fn write_pid_file(layout: &StateLayout, id: &SessionId, pid: u32) -> std::io::Result<()> {
    let path = layout.session_pid_path(id);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, format!("{pid}\n").as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Drain every consumer task with a bounded overall timeout.
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

/// Write the final `status.json` with `terminated_at` set.
pub fn finalize_state(layout: &StateLayout, id: &SessionId, _supervisor_pid: u32) -> Result<()> {
    let mut status = match ark_core::read_status(layout, id)? {
        Some(s) => s,
        None => SessionStatus {
            id: id.clone(),
            started_at: Utc::now(),
            terminated_at: None,
            ext_state: BTreeMap::new(),
        },
    };
    if status.terminated_at.is_none() {
        status.terminated_at = Some(Utc::now());
    }
    write_session_status_atomic(layout, id, &status)?;
    Ok(())
}

async fn build_intent_bridge_for_socket(
    _spec: &SessionSpec,
    compiled_scene: &CompiledScene,
) -> crate::commands::IntentBridge {
    let mut registry = IntentRegistry::new();
    register_core_ops(&mut registry);
    let ctx = IntentContext::new(compiled_scene.scene_id.clone(), "scene");
    crate::commands::IntentBridge {
        registry: Arc::new(registry),
        ctx,
    }
}

/// Mount every `Lifecycle::Always` plugin from the compiled scene.
async fn mount_always_on_from_compiled(
    compiled: &CompiledScene,
    manager: &crate::plugin_lifecycle::PluginLifecycleManager,
    event_bus: &EventSink,
) -> Vec<crate::plugin_lifecycle::MountOutcome> {
    let decls = compiled.plugin_decls();
    let mut registry = IntentRegistry::new();
    register_core_ops(&mut registry);
    let ctx = IntentContext::new(compiled.scene_id.clone(), "scene");
    manager
        .mount_always_on(&decls, &registry, &ctx, event_bus)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_mux_zellij::ZellijMux;

    fn short_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("sv-run")
            .tempdir_in("/tmp")
            .expect("short tempdir")
    }

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    fn sample_spec() -> SessionSpec {
        SessionSpec {
            id: SessionId::new("smoke"),
            name: "smoke".to_string(),
            scene_path: None,
            cwd: std::path::PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: BTreeMap::new(),
        }
    }

    /// Smoke test: bare-session boot drives the slimmed R3 sequence.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_supervisor_with_bare_session_completes() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let spec = sample_spec();

        let ok_status = tokio::process::Command::new("true").status().await.unwrap();
        let (mux, _stub) = ZellijMux::for_test(vec![ark_mux_zellij::executor::CommandOutput {
            status: ok_status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }]);

        // Pre-fire the cancel token so the bare-session path returns
        // immediately after parking on `world.cancel.cancelled().await`.
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = run_supervisor_with(
            spec.clone(),
            SupervisorMode::Foreground,
            Config::placeholder(),
            layout.clone(),
            Arc::new(mux),
            None,
            Some(cancel),
        )
        .await;

        result.expect("run_supervisor_with ok on bare-session path");

        // State dir + spec.json + status.json are present.
        assert!(layout.session_dir(&spec.id).is_dir());
        assert!(layout.session_spec_path(&spec.id).is_file());

        let status = ark_core::read_status(&layout, &spec.id)
            .expect("read status")
            .expect("status exists");
        assert!(
            status.terminated_at.is_some(),
            "finalize_state must set terminated_at"
        );

        // Socket file was unlinked on shutdown.
        let sock = layout.session_socket_path(&spec.id);
        assert!(
            !sock.exists(),
            "socket file should be unlinked after shutdown: {}",
            sock.display()
        );

        // Lock released — re-acquire should work.
        let re = crate::acquire_lock(&layout, &spec.id).expect("re-acquire");
        drop(re);
    }

    #[test]
    fn finalize_state_sets_terminated_at() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("final");
        StateLayout::ensure_dir_0700(&layout.session_dir(&id)).unwrap();
        finalize_state(&layout, &id, 42).expect("finalize");
        let s = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert!(s.terminated_at.is_some());
    }
}
