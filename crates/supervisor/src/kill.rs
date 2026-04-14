//! SIGTERM kill handler (T-070).
//!
//! Implements the process-neutral half of cavekit-supervisor.md R4:
//!
//! > Supervisor registers a SIGTERM handler that:
//! > - Fires `world.cancel`
//! > - Waits up to 10s for orchestrator.run to return
//! > - If orchestrator stalls, sends `Kill` event, tears down engine,
//! >   closes tabs via mux, exits with `Outcome::Killed`
//!
//! # What this module owns
//!
//! * [`kill_handler`] — cancel + grace timeout + tab teardown + Outcome.
//!
//! # What this module does **not** own
//!
//! * Process signals. The caller (orchestration.rs / signals.rs) still
//!   runs its existing `nix::sys::signal` flow — `kill_handler` only
//!   needs the `CancellationToken` to signal unwind.
//! * Orchestrator futures. The caller races `orchestrator.run(..)` against
//!   [`kill_handler`] via `tokio::select!`; if kill_handler returns first
//!   (grace expired) the caller treats the run as killed.
//!
//! # Tab teardown
//!
//! `kill_handler` does NOT subscribe to the event bus itself — if it did,
//! every `TabOpened` emitted *before* kill time would be invisible and the
//! R4 "close every still-open tab" contract would break (see F-086).
//!
//! Instead the caller (orchestration.rs `run_supervisor_with`) maintains a
//! long-lived `Arc<Mutex<Vec<TabHandle>>>` populated by its persistent bus
//! subscriber: `TabOpened` appends, `TabClosed` removes. At SIGTERM the
//! caller hands the same Arc into [`kill_handler`] and the grace-expiry
//! path closes whatever remains open. Callers MUST keep the registry's
//! feeding subscriber alive for the lifetime of the supervisor so tabs
//! opened *any time* before kill are represented.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ark_core::Multiplexer;
use ark_types::{AgentEvent, AgentId, EventSink, LogLevel, Outcome, TabHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Default SIGTERM → SIGKILL grace period per cavekit-supervisor.md R4.
///
/// Overridable from `config.defaults.kill_grace` (Tier 4 wiring).
pub const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(10);

/// Shared tab registry feeding [`kill_handler`]: a [`Vec<TabHandle>`] behind
/// an `Arc<Mutex>` that the caller mutates on every `TabOpened` /
/// `TabClosed` event observed on its persistent bus subscriber.
///
/// The caller (orchestration.rs `run_supervisor_with`) spawns a long-lived
/// task that owns a `broadcast::Receiver` for the full life of the run,
/// mutating this registry as events arrive. At kill time the same Arc is
/// handed into [`kill_handler`] which iterates whatever is still open.
///
/// This replaces the pre-F-086 design where `kill_handler` subscribed to
/// the bus at kill time — which was unable to see `TabOpened` events
/// emitted earlier in the run.
pub type TabRegistry = Arc<Mutex<Vec<TabHandle>>>;

/// Construct an empty [`TabRegistry`] suitable for feeding [`kill_handler`].
pub fn new_tab_registry() -> TabRegistry {
    Arc::new(Mutex::new(Vec::new()))
}

/// Apply a single [`AgentEvent`] to the shared registry: append on
/// [`AgentEvent::TabOpened`], remove on [`AgentEvent::TabClosed`], ignore
/// everything else. Safe to call from the caller's persistent bus loop.
pub fn apply_tab_event(registry: &TabRegistry, ev: &AgentEvent) {
    match ev {
        AgentEvent::TabOpened { tab_handle, .. } => {
            let mut g = registry.lock().expect("tab_registry lock poisoned");
            if !g.iter().any(|h| h == tab_handle) {
                g.push(tab_handle.clone());
            }
        }
        AgentEvent::TabClosed { tab_handle, .. } => {
            let mut g = registry.lock().expect("tab_registry lock poisoned");
            g.retain(|h| h != tab_handle);
        }
        _ => {}
    }
}

/// Handle a SIGTERM / "gracefully kill" request.
///
/// Flow (per kit R4):
/// 1. Cancel `cancel` — signals orchestrator + consumers to unwind.
/// 2. Wait up to `grace` for the orchestrator to return via an
///    externally-owned signal (the `orchestrator_done` token).
/// 3. If grace expires: emit `Log { level: Warn, line: "grace expired" }`
///    and a synthetic `Done { outcome: Killed }` (the "Kill event" per
///    kit) on the bus. Close every tab in `tab_registry` (populated by
///    the caller's persistent bus subscriber).
/// 4. Return `Outcome::Killed`.
///
/// # Parameters
///
/// * `cancel` — the supervisor-wide cancel token. Cancelled on entry.
/// * `orchestrator_done` — a second token the *caller* cancels when
///   `orchestrator.run` returns normally. If it trips before `grace`
///   expires, this handler shortcuts the tab-teardown path and simply
///   returns `Outcome::Killed`.
/// * `event_bus` — sender half of the supervisor event bus. Used to
///   publish the grace-expired `Log` + synthetic `Done` events.
/// * `mux` — the active multiplexer; `close_tab` is called on every
///   still-open tab after grace expires.
/// * `tab_registry` — shared open-tab state populated by the caller's
///   long-running bus subscriber. See [`TabRegistry`] / F-086.
/// * `agent_id` — supervisor's agent id (for synthesised events).
/// * `grace` — max wait before escalating. Use [`DEFAULT_KILL_GRACE`]
///   for R4's 10s default.
#[allow(clippy::too_many_arguments)]
pub async fn kill_handler(
    cancel: CancellationToken,
    orchestrator_done: CancellationToken,
    event_bus: EventSink,
    mux: Arc<dyn Multiplexer>,
    tab_registry: TabRegistry,
    agent_id: AgentId,
    grace: Duration,
) -> Result<Outcome> {
    // Step 1: signal cancel to orchestrator + consumers.
    cancel.cancel();
    debug!(
        agent = agent_id.as_str(),
        grace_secs = grace.as_secs(),
        "kill_handler: cancel fired; awaiting orchestrator"
    );

    let grace_expired = tokio::select! {
        biased;
        _ = orchestrator_done.cancelled() => {
            debug!(agent = agent_id.as_str(), "kill_handler: orchestrator returned before grace");
            false
        }
        _ = tokio::time::sleep(grace) => true,
    };

    if !grace_expired {
        // Orchestrator returned cleanly — no tab teardown needed. The
        // outer run_supervisor path will run step 15+ to tear down the
        // engine and close tabs via the normal flow.
        return Ok(Outcome::Killed);
    }

    // Step 3a: emit a Log Warn "grace expired".
    let warn_ev = AgentEvent::Log {
        id: agent_id.clone(),
        level: LogLevel::Warn,
        line: "grace expired".to_string(),
    };
    if let Err(err) = event_bus.send(warn_ev) {
        warn!(%err, "kill_handler: could not emit Log(grace expired)");
    }

    // Step 3b: emit the synthetic "Kill" event. The kit spec says "emit
    // Kill event" — we use the canonical AgentEvent::Done { Killed } as
    // there is no dedicated Kill variant in AgentEvent (see
    // cavekit-types-state-events.md R3 event list).
    let kill_ev = AgentEvent::Done {
        id: agent_id.clone(),
        outcome: Outcome::Killed,
    };
    if let Err(err) = event_bus.send(kill_ev) {
        warn!(%err, "kill_handler: could not emit Kill (Done/Killed) event");
    }

    // Step 4: close every still-open tab (snapshot the registry under the
    // lock, then release before the async close_tab calls so the lock is
    // not held across await points).
    let open_tabs: Vec<TabHandle> = {
        let g = tab_registry.lock().expect("tab_registry lock poisoned");
        g.clone()
    };
    if open_tabs.is_empty() {
        debug!(
            agent = agent_id.as_str(),
            "kill_handler: no tabs to close after grace"
        );
    } else {
        for handle in &open_tabs {
            if let Err(err) = mux.close_tab(handle).await {
                warn!(
                    agent = agent_id.as_str(),
                    tab = %handle,
                    %err,
                    "kill_handler: close_tab failed"
                );
            }
        }
    }

    Ok(Outcome::Killed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{TabRole, channel};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Mux that records every `close_tab` call.
    struct StubMux {
        closed: Mutex<Vec<TabHandle>>,
    }

    impl StubMux {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                closed: Mutex::new(Vec::new()),
            })
        }
        fn closed(&self) -> Vec<TabHandle> {
            self.closed.lock().unwrap().clone()
        }
    }

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
        async fn close_tab(&self, handle: &TabHandle) -> Result<()> {
            self.closed.lock().unwrap().push(handle.clone());
            Ok(())
        }
        async fn rename_tab(&self, _handle: &TabHandle, _name: &str) -> Result<()> {
            Ok(())
        }
        async fn pipe(&self, _target: &str, _payload: &str) -> Result<()> {
            Ok(())
        }
    }

    fn agent() -> AgentId {
        AgentId::new("cavekit", "kill")
    }

    #[tokio::test]
    async fn orchestrator_returns_before_grace_yields_killed() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(16);
        let mux = StubMux::new();
        let registry = new_tab_registry();

        // Trip orchestrator_done immediately — kill_handler should
        // shortcut and return Killed without waiting.
        done.cancel();

        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx,
            mux.clone(),
            registry,
            agent(),
            Duration::from_secs(5),
        )
        .await
        .expect("ok");

        assert!(matches!(outcome, Outcome::Killed));
        assert!(cancel.is_cancelled(), "cancel must fire on entry");
        assert!(
            mux.closed().is_empty(),
            "no tabs to close on fast-path Killed"
        );
    }

    /// F-086: kill with empty tab_registry returns Killed with zero
    /// `close_tab` calls.
    #[tokio::test]
    async fn kill_with_empty_registry_closes_nothing() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(8);
        let mux = StubMux::new();
        let registry = new_tab_registry();

        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx,
            mux.clone(),
            registry,
            agent(),
            Duration::from_millis(120),
        )
        .await
        .expect("ok");

        assert!(matches!(outcome, Outcome::Killed));
        assert!(cancel.is_cancelled());
        assert!(
            mux.closed().is_empty(),
            "empty registry → no close calls, got {:?}",
            mux.closed()
        );
    }

    /// F-086: kill with a pre-populated registry containing TabOpened-
    /// derived handles closes each of them and emits both the Warn log
    /// and the synthetic Done/Killed event.
    #[tokio::test]
    async fn kill_with_two_open_tabs_closes_both_and_emits_events() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, mut rx) = channel(32);
        let mux = StubMux::new();

        let tab_a = TabHandle::new("ark-cavekit-kill", 1, "builder");
        let tab_b = TabHandle::new("ark-cavekit-kill", 2, "log");

        // Pre-populate registry via apply_tab_event to match how the
        // caller's persistent bus loop would feed it (simulating events
        // seen BEFORE kill fires).
        let registry = new_tab_registry();
        let id = agent();
        apply_tab_event(
            &registry,
            &AgentEvent::TabOpened {
                id: id.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: tab_a.clone(),
                label: "builder".into(),
            },
        );
        apply_tab_event(
            &registry,
            &AgentEvent::TabOpened {
                id: id.clone(),
                parent: None,
                role: TabRole::Log,
                tab_handle: tab_b.clone(),
                label: "log".into(),
            },
        );
        assert_eq!(registry.lock().unwrap().len(), 2);

        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx.clone(),
            mux.clone(),
            registry,
            id.clone(),
            Duration::from_millis(120),
        )
        .await
        .expect("ok");

        assert!(matches!(outcome, Outcome::Killed));

        let closed = mux.closed();
        assert_eq!(closed.len(), 2, "both tabs closed, got {closed:?}");
        assert!(closed.iter().any(|t| t.name == "builder"));
        assert!(closed.iter().any(|t| t.name == "log"));

        // Drain the bus and look for Log(Warn, "grace expired") + Done/Killed.
        let mut saw_warn = false;
        let mut saw_kill = false;
        for _ in 0..64 {
            match rx.try_recv() {
                Ok(AgentEvent::Log {
                    level: LogLevel::Warn,
                    line,
                    ..
                }) if line == "grace expired" => saw_warn = true,
                Ok(AgentEvent::Done {
                    outcome: Outcome::Killed,
                    ..
                }) => saw_kill = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_warn, "expected Log(Warn, \"grace expired\")");
        assert!(saw_kill, "expected synthetic Done/Killed event");
    }

    /// F-086: A TabClosed event removes the handle from the registry, so
    /// kill_handler does not re-close it when grace expires.
    #[tokio::test]
    async fn tab_closed_before_kill_removed_from_registry_not_closed_again() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(16);
        let mux = StubMux::new();
        let registry = new_tab_registry();
        let id = agent();
        let tab = TabHandle::new("ark-cavekit-kill", 1, "builder");

        // Pre-populate: open then close.
        apply_tab_event(
            &registry,
            &AgentEvent::TabOpened {
                id: id.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: tab.clone(),
                label: "builder".into(),
            },
        );
        apply_tab_event(
            &registry,
            &AgentEvent::TabClosed {
                id: id.clone(),
                tab_handle: tab.clone(),
            },
        );
        assert!(registry.lock().unwrap().is_empty());

        let outcome = kill_handler(
            cancel,
            done,
            tx,
            mux.clone(),
            registry,
            id,
            Duration::from_millis(80),
        )
        .await
        .expect("ok");
        assert!(matches!(outcome, Outcome::Killed));
        assert!(
            mux.closed().is_empty(),
            "already-closed tab must NOT be re-closed, got {:?}",
            mux.closed()
        );
    }

    /// F-086 regression: tabs opened BEFORE kill_handler is called MUST
    /// still be closed. Pre-fix, kill_handler subscribed inside and could
    /// not see historical events.
    #[tokio::test]
    async fn tabs_opened_before_kill_are_still_closed_at_grace_expiry() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(16);
        let mux = StubMux::new();
        let registry = new_tab_registry();
        let id = agent();

        // Simulate the caller's long-running bus subscriber observing a
        // TabOpened event BEFORE kill_handler is called.
        apply_tab_event(
            &registry,
            &AgentEvent::TabOpened {
                id: id.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: TabHandle::new("ark-cavekit-kill", 1, "builder"),
                label: "builder".into(),
            },
        );

        // Kill fires AFTER the event has already been consumed.
        let outcome = kill_handler(
            cancel,
            done,
            tx,
            mux.clone(),
            registry,
            id,
            Duration::from_millis(60),
        )
        .await
        .expect("ok");
        assert!(matches!(outcome, Outcome::Killed));
        assert_eq!(mux.closed().len(), 1, "pre-kill TabOpened must be closed");
    }

    #[tokio::test]
    async fn integration_with_slow_orchestrator_escalates_to_kill() {
        // Integration harness: a fake orchestrator future races against
        // kill_handler. If the orchestrator is slower than `grace`,
        // kill_handler returns first with Outcome::Killed.
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(16);
        let mux = StubMux::new();
        let registry = new_tab_registry();

        let slow_orchestrator = {
            let cancel = cancel.clone();
            let done = done.clone();
            async move {
                // Simulate an orchestrator that DOESN'T respect cancel.
                // It just sleeps longer than the grace window.
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
                done.cancel();
                Outcome::Success { artifacts: vec![] }
            }
        };

        let id = agent();
        let killer = kill_handler(
            cancel,
            done,
            tx,
            mux,
            registry,
            id,
            Duration::from_millis(100),
        );

        let (killer_out, orch_out) = tokio::join!(killer, slow_orchestrator);
        assert!(matches!(killer_out.expect("ok"), Outcome::Killed));
        // Orchestrator eventually finishes with Success — kill_handler
        // already reported Killed, which is the authoritative outcome.
        assert!(matches!(orch_out, Outcome::Success { .. }));
    }
}
