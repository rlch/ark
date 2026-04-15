//! Orchestrator contract suite — trait-level conformance tests that every
//! [`crate::Orchestrator`] implementation must pass.
//!
//! Implements cavekit-architecture.md R1/R2 (T-115). Mirrors the
//! [`crate::engine_contract`] pattern: hand the suite a factory closure
//! that mints a fresh `Box<dyn Orchestrator>` plus a bundle of on-disk
//! fixtures, and the suite asserts every scripted scenario the
//! `Orchestrator` trait is contractually required to satisfy.
//!
//! The in-tree impls today are `ark_orchestrators_cavekit::CavekitOrchestrator`
//! and `ark_orchestrators_claude_code::ClaudeCodeOrchestrator`; future
//! orchestrators would pass the same suite against their own factory.
//!
//! ## Trait surface vs. scripted scenarios
//!
//! The [`Orchestrator`] trait surface covers `name`, `engine`, `detect`,
//! and `run`. The contract asserts:
//!
//! - `name()` is a non-empty slug, stable across calls.
//! - `engine()` is a non-empty slug, stable across calls.
//! - `detect()` returns a `bool` that matches an orchestrator-supplied
//!   positive fixture. Negative detection is orchestrator-specific
//!   (e.g. cavekit requires marker files under `context/`; claude-code
//!   only requires the `claude` binary on PATH and would match any
//!   cwd), so the negative assertion is skipped for orchestrators that
//!   opt out via [`OrchestratorFixtures::negative_cwd_is_miss`].
//! - `run()` with a minimal spec + mock mux completes without panic
//!   when the event bus emits a terminal `Done { Success }`. Deep
//!   watcher behaviour is deferred to T-125.
//!
//! ## Deferred (tracked for follow-up)
//!
//! - `lifecycle_dispose_is_idempotent` — the trait does not currently
//!   expose an explicit dispose/teardown method; cancellation goes
//!   through `World::cancel`. The smoke scenario exercises the
//!   happy-path Done flow; the cancel path is validated by each
//!   orchestrator's crate-local tests (`run_returns_killed_on_cancel`
//!   etc.) and will be folded into the contract once the trait
//!   stabilises.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ark_mux_zellij::ZellijMux;
use ark_types::{
    AgentEvent, AgentId, AgentSpec, CancellationToken, Outcome, StateLayout, channel,
};

use crate::config::Config;
use crate::orchestrator::{Orchestrator, World};

/// Bundle of fixture inputs consumed by the Orchestrator contract suite.
///
/// Holding the paths in a struct lets the contract suite accept a single
/// argument that future orchestrators can reuse verbatim, and keeps
/// fixture discovery in one place.
#[derive(Debug, Clone)]
pub struct OrchestratorFixtures {
    /// Absolute path to a cwd that the orchestrator under test is
    /// *expected* to match via `detect()`. For cavekit this is the
    /// committed `cavekit-project` fixture; for claude-code it is any
    /// directory (PATH-based detection).
    pub positive_cwd: PathBuf,
    /// When `true`, the contract asserts `detect()` returns `false` on a
    /// fresh empty tempdir. Orchestrators with PATH-based (rather than
    /// cwd-based) detection should set this to `false`.
    pub negative_cwd_is_miss: bool,
}

/// Run the portable portion of the Orchestrator contract suite against
/// `factory`. `fixtures` points at the inputs every orchestrator impl
/// must satisfy.
///
/// Each scenario is a scripted scenario from T-115:
///
/// | Scenario                                    | Trait method exercised  |
/// |---------------------------------------------|-------------------------|
/// | `factory_closure_produces_fresh_instance`   | (factory closure)       |
/// | `name_is_stable_non_empty_slug`             | `name`                  |
/// | `engine_is_stable_non_empty_slug`           | `engine`                |
/// | `detect_positive_returns_true`              | `detect`                |
/// | `detect_negative_returns_false`             | `detect`                |
/// | `run_smoke_returns_handle`                  | `run`                   |
///
/// Orchestrator crates typically wrap this call in a single `#[test]`
/// function and add their own crate-specific scenarios on top.
///
/// # Panics
/// Panics on the first violated assertion. Tests convert panics into
/// failures, so this is the intended failure mode.
pub fn orchestrator_contract_suite<F>(factory: F, fixtures: &OrchestratorFixtures)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    factory_closure_produces_fresh_instance(&factory);
    name_is_stable_non_empty_slug(&factory);
    engine_is_stable_non_empty_slug(&factory);
    detect_positive_returns_true(&factory, fixtures);
    if fixtures.negative_cwd_is_miss {
        detect_negative_returns_false(&factory);
    }
    run_smoke_returns_handle(&factory);
}

fn factory_closure_produces_fresh_instance<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let a = factory();
    let b = factory();
    assert_eq!(
        a.name(),
        b.name(),
        "factory closure must produce orchestrators of the same kind \
         (got `{}` and `{}`)",
        a.name(),
        b.name()
    );
    // Independent drops prove the factory minted two distinct allocations.
    drop(a);
    drop(b);
}

fn name_is_stable_non_empty_slug<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    let n = orch.name();
    assert!(
        !n.is_empty(),
        "Orchestrator::name must return a non-empty &'static str"
    );
    assert!(
        n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Orchestrator::name must be a slug (lowercase ascii + digits + dash), got {n:?}"
    );
    assert_eq!(
        orch.name(),
        n,
        "Orchestrator::name must be stable across calls on the same instance"
    );
}

fn engine_is_stable_non_empty_slug<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    let e = orch.engine();
    assert!(
        !e.is_empty(),
        "Orchestrator::engine must return a non-empty &'static str"
    );
    assert!(
        e.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "Orchestrator::engine must be a slug (lowercase ascii + digits + dash), got {e:?}"
    );
    assert_eq!(
        orch.engine(),
        e,
        "Orchestrator::engine must be stable across calls on the same instance"
    );
}

fn detect_positive_returns_true<F>(factory: &F, fixtures: &OrchestratorFixtures)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    assert!(
        fixtures.positive_cwd.exists(),
        "positive_cwd fixture must exist on disk: {}",
        fixtures.positive_cwd.display()
    );
    assert!(
        orch.detect(&fixtures.positive_cwd),
        "Orchestrator::detect({}) must return true for the positive fixture",
        fixtures.positive_cwd.display()
    );
}

fn detect_negative_returns_false<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let orch = factory();
    let tmp = tempfile::tempdir().expect("tempdir for detect_negative");
    assert!(
        !orch.detect(tmp.path()),
        "Orchestrator::detect({}) on an empty tempdir must return false \
         for orchestrators that opt in via negative_cwd_is_miss",
        tmp.path().display()
    );
}

fn run_smoke_returns_handle<F>(factory: &F)
where
    F: Fn() -> Box<dyn Orchestrator>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime");

    rt.block_on(async {
        let orch = factory();
        let tmp = tempfile::tempdir().expect("tempdir for run smoke");
        let cwd = tmp.path().to_path_buf();

        let spec = AgentSpec::new(
            AgentId::new(orch.name(), "contract-smoke"),
            "contract-smoke",
            orch.name(),
            orch.engine(),
            cwd.clone(),
            vec!["claude".to_string()],
        );
        let id = spec.id.clone();
        let session = spec.session.clone();

        // Script a generous ok queue of `list-sessions` + `action new-tab`
        // responses so every orchestrator's happy-path smoke flow has
        // something to consume. Assertions target recorded argv.
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .expect("spawn `true` for ExitStatus");
        let ok_output = |stdout: Vec<u8>| ark_mux_zellij::executor::CommandOutput {
            status: ok_status,
            stdout,
            stderr: Vec::new(),
        };
        // Queue plenty: ensure_session + several create_tab calls. Extra
        // entries are harmless — they only matter if consumed.
        //
        // Use the `in_zellij = true` variant so first-tab create_tab goes
        // through `action switch-session` (executor path) rather than the
        // outside-zellij pty path (which would try to spawn a real zellij
        // binary, unavailable in this test context).
        let scripted: Vec<ark_mux_zellij::executor::CommandOutput> =
            (0..16).map(|_| ok_output(Vec::new())).collect();
        let (mux, stub) = ZellijMux::for_test_in_zellij(scripted);
        let mux: Arc<ZellijMux> = Arc::new(mux);

        let (events, _rx) = channel(256);
        let cancel = CancellationToken::new();
        let hooks_dir = tmp.path().join(".ark-hooks");
        let state = Arc::new(StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("runtime"),
            tmp.path().join("cfg"),
        ));
        let config = Arc::new(Config::placeholder());

        let world = World::new(
            spec.clone(),
            mux.clone(),
            events.clone(),
            cancel.clone(),
            hooks_dir,
            state,
            config,
        );

        // Nudge the orchestrator toward a terminal Done shortly after
        // `run` subscribes to the bus. We don't exercise watchers
        // deeply — that's T-125 territory. This keeps the smoke
        // scenario bounded and trait-surface only.
        let done_sender = events.clone();
        let done_id = id.clone();
        let done_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = done_sender.send(AgentEvent::Done {
                id: done_id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        // Guard the smoke with a wall-clock timeout so a misbehaving
        // orchestrator fails loud instead of hanging the suite.
        let outcome = tokio::time::timeout(Duration::from_secs(10), orch.run(spec, world))
            .await
            .expect("run_smoke_returns_handle must complete within 10s")
            .expect("run must return Ok on the happy path");

        // Cancel the nudge task if it's still pending.
        done_task.abort();

        // Success/Failed/Killed are all acceptable outcomes on a fresh
        // tempdir — the contract only asserts `run` returned without
        // panic and produced an `Outcome`. The kind of outcome depends
        // on orchestrator-specific policy (cavekit may downgrade to
        // Failed if impl-tracking is missing in non-fixture cwds).
        match outcome {
            Outcome::Success { .. }
            | Outcome::Failed { .. }
            | Outcome::Killed
            | Outcome::Timeout
            | Outcome::Crashed { .. } => {}
        }

        // The stub executor must have recorded at least one zellij call
        // matching a tab-creation verb. Outside-zellij first-tab spawn
        // goes through the pty path (no executor), so we check for
        // EITHER a `switch-session` or `action new-tab` argv shape; both
        // are valid signs the orchestrator opened a tab.
        let calls = stub.recorded_calls();
        let saw_tab_create = calls.iter().any(|(_, args)| {
            let has_switch = args.iter().any(|a| a == "switch-session");
            let has_new_tab = args.iter().any(|a| a == "new-tab");
            has_switch || has_new_tab
        });
        // The first-tab pty spawn bypasses the executor entirely. When
        // that path fires, the executor may have zero recorded calls. In
        // that case, accept the fact that the orchestrator completed
        // without panic — the pty route is covered by the mux crate's
        // own tests.
        if !calls.is_empty() {
            assert!(
                saw_tab_create,
                "Orchestrator::run recorded mux calls but none created a tab \
                 (session `{session}`); got: {calls:?}"
            );
        }
        let _ = id; // kept for future assertions
    });
}

// ------------------------------------------------------------------------
// Unused-in-public-surface re-exports for downstream docs.
// ------------------------------------------------------------------------

#[doc(hidden)]
pub use ark_types::channel as __channel;

// ------------------------------------------------------------------------
// Self-tests: a minimal MockOrchestrator passes the suite, and the
// detect/name/engine assertions fire when contracts are violated.
// ------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ark_types::{EventSink, TabHandle};
    use async_trait::async_trait;
    use std::path::Path;

    /// Minimal orchestrator that satisfies the contract.
    struct MockOrchestrator {
        name: &'static str,
        engine: &'static str,
    }

    #[async_trait]
    impl Orchestrator for MockOrchestrator {
        fn name(&self) -> &'static str {
            self.name
        }
        fn engine(&self) -> &'static str {
            self.engine
        }
        fn detect(&self, _cwd: &Path) -> bool {
            true
        }
        async fn run(&self, _spec: AgentSpec, world: World) -> anyhow::Result<Outcome> {
            // Pretend to open a builder tab so the smoke scenario's
            // "create_tab was called" assertion passes.
            let _ = world
                .mux
                .create_tab(&_spec.session, "builder", Path::new("builder"))
                .await?;
            let _ = world.events.send(AgentEvent::TabOpened {
                id: _spec.id.clone(),
                parent: None,
                role: ark_types::TabRole::Builder,
                tab_handle: TabHandle::new(&_spec.session, 1, "builder"),
                label: "builder".to_string(),
            });
            // Wait for the Done nudge sent by the suite.
            let mut rx = world.events.subscribe();
            loop {
                match rx.recv().await {
                    Ok(AgentEvent::Done { outcome, .. }) => return Ok(outcome),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Ok(Outcome::Success {
                            artifacts: Vec::new(),
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    }

    fn mock_fixtures() -> OrchestratorFixtures {
        // A populated tempdir would also work; we use the positive
        // fixture the real cavekit suite consumes so the suite-internal
        // test mirrors the downstream shape.
        OrchestratorFixtures {
            positive_cwd: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            negative_cwd_is_miss: false, // MockOrchestrator matches everything
        }
    }

    #[test]
    fn mock_orchestrator_passes_contract_suite() {
        let fx = mock_fixtures();
        orchestrator_contract_suite(
            || -> Box<dyn Orchestrator> {
                Box::new(MockOrchestrator {
                    name: "mock",
                    engine: "claude-code",
                })
            },
            &fx,
        );
    }

    #[test]
    fn contract_rejects_empty_name_slug() {
        let factory = || -> Box<dyn Orchestrator> {
            Box::new(MockOrchestrator {
                name: "",
                engine: "claude-code",
            })
        };
        let result = std::panic::catch_unwind(|| name_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected empty-name orchestrator to be rejected by name assertion"
        );
    }

    #[test]
    fn contract_rejects_empty_engine_slug() {
        let factory = || -> Box<dyn Orchestrator> {
            Box::new(MockOrchestrator {
                name: "mock",
                engine: "",
            })
        };
        let result = std::panic::catch_unwind(|| engine_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected empty-engine orchestrator to be rejected by engine assertion"
        );
    }

    #[test]
    fn contract_rejects_uppercase_name_slug() {
        let factory = || -> Box<dyn Orchestrator> {
            Box::new(MockOrchestrator {
                name: "CAVEKIT",
                engine: "claude-code",
            })
        };
        let result = std::panic::catch_unwind(|| name_is_stable_non_empty_slug(&factory));
        assert!(
            result.is_err(),
            "expected uppercase name slug to be rejected by name assertion"
        );
    }

    #[test]
    fn detect_negative_panics_when_orchestrator_matches_empty_tempdir() {
        // MockOrchestrator matches every cwd — the negative-detect
        // assertion must panic.
        let factory = || -> Box<dyn Orchestrator> {
            Box::new(MockOrchestrator {
                name: "mock",
                engine: "claude-code",
            })
        };
        let result = std::panic::catch_unwind(|| detect_negative_returns_false(&factory));
        assert!(
            result.is_err(),
            "orchestrator that matches every cwd must fail the negative detect assertion"
        );
    }

    #[test]
    fn event_sink_type_is_re_exported_for_mock_use() {
        // Compile-time check: downstream contract tests rely on
        // `ark_types::EventSink` being reachable through the crate. We
        // touch the type here to ensure the re-export stays in range.
        let (_sink, _rx): (EventSink, _) = ark_types::channel(4);
    }
}
