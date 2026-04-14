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
//! The handler subscribes to the event bus (via the provided `EventSink`
//! → a new receiver) and tracks every `TabOpened` / `TabClosed` pair it
//! observed up to the point `kill_handler` was called. Callers MUST
//! construct `kill_handler` by first taking the full receiver stream so
//! the grace-expiry path can enumerate still-open tabs.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ark_core::Multiplexer;
use ark_types::{AgentEvent, AgentId, EventSink, LogLevel, Outcome, TabHandle};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Default SIGTERM → SIGKILL grace period per cavekit-supervisor.md R4.
///
/// Overridable from `config.defaults.kill_grace` (Tier 4 wiring).
pub const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(10);

/// Handle a SIGTERM / "gracefully kill" request.
///
/// Flow (per kit R4):
/// 1. Cancel `cancel` — signals orchestrator + consumers to unwind.
/// 2. Wait up to `grace` for the orchestrator to return via an
///    externally-owned signal (the `orchestrator_done` token).
/// 3. If grace expires: emit `Log { level: Warn, line: "grace expired" }`
///    and a synthetic `Done { outcome: Killed }` (the "Kill event" per
///    kit) on the bus. Close every tab the agent opened via `TabOpened`
///    that was not subsequently `TabClosed`.
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
///   publish the grace-expired `Log` + synthetic `Done` events, AND
///   subscribed to *before* step 1 to track `TabOpened` / `TabClosed`.
/// * `mux` — the active multiplexer; `close_tab` is called on every
///   still-open tab after grace expires.
/// * `agent_id` — supervisor's agent id (for synthesised events).
/// * `grace` — max wait before escalating. Use [`DEFAULT_KILL_GRACE`]
///   for R4's 10s default.
#[allow(clippy::too_many_arguments)]
pub async fn kill_handler(
    cancel: CancellationToken,
    orchestrator_done: CancellationToken,
    event_bus: EventSink,
    mux: Arc<dyn Multiplexer>,
    agent_id: AgentId,
    grace: Duration,
) -> Result<Outcome> {
    // Snapshot already-subscribed receiver so TabOpened / TabClosed events
    // flowing in while we wait for grace are observed.
    let mut rx = event_bus.subscribe();

    // Step 1: signal cancel to orchestrator + consumers.
    cancel.cancel();
    debug!(
        agent = agent_id.as_str(),
        grace_secs = grace.as_secs(),
        "kill_handler: cancel fired; awaiting orchestrator"
    );

    // Track open tabs we observe from the event stream.
    let mut open_tabs: Vec<TabHandle> = Vec::new();

    let grace_expired = tokio::select! {
        biased;
        _ = orchestrator_done.cancelled() => {
            debug!(agent = agent_id.as_str(), "kill_handler: orchestrator returned before grace");
            false
        }
        _ = collect_tabs_until(&mut rx, &mut open_tabs, grace) => {
            true
        }
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

    // Step 4: close every still-open tab.
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

/// Subscribe-loop helper: drains events from `rx` for `window`, mutating
/// `open_tabs` on every observed `TabOpened` / `TabClosed` pair.
///
/// Returns when `window` elapses, regardless of recv activity.
async fn collect_tabs_until(
    rx: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
    open_tabs: &mut Vec<TabHandle>,
    window: Duration,
) {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => return, // timeout reached
            Ok(Ok(ev)) => merge_tab_event(open_tabs, &ev),
            Ok(Err(RecvError::Lagged(n))) => {
                warn!(
                    skipped = n,
                    "kill_handler: event bus lag during grace window; tab set may be incomplete"
                );
            }
            Ok(Err(RecvError::Closed)) => return,
        }
    }
}

fn merge_tab_event(open_tabs: &mut Vec<TabHandle>, ev: &AgentEvent) {
    match ev {
        AgentEvent::TabOpened { tab_handle, .. } => {
            if !open_tabs.iter().any(|h| h == tab_handle) {
                open_tabs.push(tab_handle.clone());
            }
        }
        AgentEvent::TabClosed { tab_handle, .. } => {
            open_tabs.retain(|h| h != tab_handle);
        }
        _ => {}
    }
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

        // Trip orchestrator_done immediately — kill_handler should
        // shortcut and return Killed without waiting.
        done.cancel();

        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx,
            mux.clone(),
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

    #[tokio::test]
    async fn grace_expires_closes_tabs_and_emits_events() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, mut rx) = channel(32);
        let mux = StubMux::new();

        let id = agent();
        let tab_a = TabHandle::new("ark-cavekit-kill", 1, "builder");
        let tab_b = TabHandle::new("ark-cavekit-kill", 2, "log");

        // Fire TabOpened for two tabs BEFORE kill_handler subscribes —
        // kill_handler subscribes inside, so those won't be seen. Fire
        // them during the grace window instead.
        let tx2 = tx.clone();
        let id2 = id.clone();
        let emit = tokio::spawn(async move {
            // small delay so kill_handler has subscribed.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx2.send(AgentEvent::TabOpened {
                id: id2.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: tab_a.clone(),
                label: "builder".into(),
            });
            let _ = tx2.send(AgentEvent::TabOpened {
                id: id2.clone(),
                parent: None,
                role: TabRole::Log,
                tab_handle: tab_b.clone(),
                label: "log".into(),
            });
        });

        // Short grace so the test finishes fast.
        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx.clone(),
            mux.clone(),
            id.clone(),
            Duration::from_millis(300),
        )
        .await
        .expect("ok");
        emit.await.unwrap();

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

    #[tokio::test]
    async fn grace_expires_with_no_tabs_still_returns_killed() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(8);
        let mux = StubMux::new();

        let outcome = kill_handler(
            cancel.clone(),
            done,
            tx,
            mux.clone(),
            agent(),
            Duration::from_millis(120),
        )
        .await
        .expect("ok");

        assert!(matches!(outcome, Outcome::Killed));
        assert!(cancel.is_cancelled());
        assert!(
            mux.closed().is_empty(),
            "no tabs opened → no close calls, got {:?}",
            mux.closed()
        );
    }

    #[tokio::test]
    async fn tab_closed_during_grace_removes_from_close_set() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(32);
        let mux = StubMux::new();

        let id = agent();
        let tab = TabHandle::new("ark-cavekit-kill", 1, "builder");
        let tab_clone = tab.clone();

        let tx2 = tx.clone();
        let id2 = id.clone();
        let emit = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx2.send(AgentEvent::TabOpened {
                id: id2.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: tab_clone.clone(),
                label: "builder".into(),
            });
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = tx2.send(AgentEvent::TabClosed {
                id: id2.clone(),
                tab_handle: tab_clone,
            });
        });

        let outcome = kill_handler(
            cancel,
            done,
            tx.clone(),
            mux.clone(),
            id,
            Duration::from_millis(200),
        )
        .await
        .expect("ok");
        emit.await.unwrap();

        assert!(matches!(outcome, Outcome::Killed));
        assert!(
            mux.closed().is_empty(),
            "tab was closed during grace → not re-closed, got {:?}",
            mux.closed()
        );
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
        let killer = kill_handler(cancel, done, tx, mux, id, Duration::from_millis(100));

        let (killer_out, orch_out) = tokio::join!(killer, slow_orchestrator);
        assert!(matches!(killer_out.expect("ok"), Outcome::Killed));
        // Orchestrator eventually finishes with Success — kill_handler
        // already reported Killed, which is the authoritative outcome.
        assert!(matches!(orch_out, Outcome::Success { .. }));
    }
}
