//! SIGTERM kill handler (cavekit-soul-phase-1-supervisor.md R5).
//!
//! Soul phase 1 rewrite of T-070. The original shape emitted a synthetic
//! outcome-keyed "done / killed" envelope on the bus at grace expiry.
//! Under soul phase 1 the outcome-keyed event surface is gone; the kill
//! path now broadcasts the narrow `CoreEvent::SessionEnded` envelope
//! (with `terminated_at = Utc::now()`) to signal the session is terminal.
//!
//! # Flow (per kit R5)
//!
//! 1. Cancel `cancel` — signals orchestrator + consumers to unwind.
//! 2. Wait up to `grace` for the orchestrator's done token, `orchestrator_done`,
//!    to trip — indicating a clean return from whatever long-lived task owns
//!    the main loop.
//! 3. If grace expires:
//!    - Emit a `CoreEvent::Log { level: "warn", message: "grace expired" }`
//!      so downstream consumers observe the escalation.
//!    - Emit `CoreEvent::SessionEnded { terminated_at: Utc::now() }`.
//!    - Close every tab in the shared [`TabRegistry`] via the mux.
//! 4. Return `Ok(())`. The caller's finalisation path observes
//!    `SessionEnded` on the bus (or times out on its own schedule) and
//!    drives step 15+ of the R3 boot sequence.
//!
//! # Tab teardown
//!
//! `kill_handler` does NOT subscribe to the event bus itself — if it
//! did, every `TabOpened` emitted *before* kill time would be invisible
//! and the R5 "close every still-open tab" contract would break.
//!
//! Instead the caller (orchestration.rs `run_supervisor_with`) maintains
//! a long-lived `Arc<Mutex<Vec<TabHandle>>>` populated by its persistent
//! bus subscriber. At SIGTERM the caller hands the same Arc into
//! [`kill_handler`] and the grace-expiry path closes whatever remains
//! open. Callers MUST keep the registry's feeding subscriber alive for
//! the lifetime of the supervisor so tabs opened *any time* before kill
//! are represented.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use ark_mux_zellij::{TabHandle, ZellijMux};
use ark_types::{CoreEvent, EventSink, SessionId};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Default SIGTERM → SIGKILL grace period per cavekit-supervisor.md R5.
pub const DEFAULT_KILL_GRACE: Duration = Duration::from_secs(10);

/// Shared tab registry feeding [`kill_handler`]: a `Vec<TabHandle>` behind
/// an `Arc<Mutex>` that the caller mutates on every `TabOpened` /
/// `TabClosed` event observed on its persistent bus subscriber.
///
/// The caller (orchestration.rs `run_supervisor_with`) spawns a long-lived
/// task that owns a `broadcast::Receiver` for the full life of the run,
/// mutating this registry as events arrive. At kill time the same Arc is
/// handed into [`kill_handler`] which iterates whatever is still open.
pub type TabRegistry = Arc<Mutex<Vec<TabHandle>>>;

/// Construct an empty [`TabRegistry`] suitable for feeding [`kill_handler`].
pub fn new_tab_registry() -> TabRegistry {
    Arc::new(Mutex::new(Vec::new()))
}

/// Apply a single tab-scoped event to the shared registry: append on an
/// "opened" signal, remove on a "closed" signal. Under soul phase 1 the
/// tab-scoped events re-home inside extensions (Phase 4+); callers feed
/// this registry from whichever extension event surface opens/closes
/// tabs on their behalf. The function takes `&TabHandle` + an `opened`
/// flag so it stays stable across that re-homing.
pub fn apply_tab_event(registry: &TabRegistry, handle: &TabHandle, opened: bool) {
    let mut g = registry.lock().expect("tab_registry lock poisoned");
    if opened {
        if !g.iter().any(|h| h == handle) {
            g.push(handle.clone());
        }
    } else {
        g.retain(|h| h != handle);
    }
}

/// Handle a SIGTERM / "gracefully kill" request.
///
/// See the module docs for the full kit R5 flow.
///
/// # Parameters
///
/// * `cancel` — the supervisor-wide cancel token. Cancelled on entry.
/// * `orchestrator_done` — a second token the *caller* cancels when the
///   long-lived main loop returns normally. If it trips before `grace`
///   expires, this handler shortcuts the tab-teardown path and returns
///   `Ok(())` without broadcasting `SessionEnded`.
/// * `event_bus` — sender half of the supervisor event bus. Used to
///   publish the `Log` + `SessionEnded` envelopes.
/// * `mux` — the active multiplexer; `close_tab` is called on every
///   still-open tab after grace expires.
/// * `tab_registry` — shared open-tab state populated by the caller's
///   long-running bus subscriber. See [`TabRegistry`].
/// * `_session_id` — supervisor's session id (currently used only for
///   tracing; retained in the signature for forward compatibility with
///   session-aware event routing).
/// * `grace` — max wait before escalating. Use [`DEFAULT_KILL_GRACE`]
///   for R5's 10s default.
#[allow(clippy::too_many_arguments)]
pub async fn kill_handler(
    cancel: CancellationToken,
    orchestrator_done: CancellationToken,
    event_bus: EventSink,
    mux: Arc<ZellijMux>,
    tab_registry: TabRegistry,
    _session_id: SessionId,
    grace: Duration,
) -> Result<()> {
    // Step 1: signal cancel to orchestrator + consumers.
    cancel.cancel();
    debug!(
        grace_secs = grace.as_secs(),
        "kill_handler: cancel fired; awaiting orchestrator"
    );

    let grace_expired = tokio::select! {
        biased;
        _ = orchestrator_done.cancelled() => {
            debug!("kill_handler: orchestrator returned before grace");
            false
        }
        _ = tokio::time::sleep(grace) => true,
    };

    if !grace_expired {
        // Orchestrator returned cleanly — no tab teardown needed. The
        // outer run_supervisor path will run its finalisation flow and
        // will broadcast SessionEnded itself (or not, on the bare-session
        // path). kill_handler has no further work.
        return Ok(());
    }

    // Step 3a: emit a Log Warn "grace expired".
    let warn_ev = CoreEvent::Log {
        level: "warn".to_string(),
        message: "grace expired".to_string(),
        target: Some("ark::supervisor::kill".to_string()),
    };
    if let Err(err) = event_bus.send(warn_ev) {
        warn!(%err, "kill_handler: could not emit Log(grace expired)");
    }

    // Step 3b: broadcast the canonical "session terminal" signal.
    let ended_ev = CoreEvent::SessionEnded {
        terminated_at: Utc::now(),
    };
    if let Err(err) = event_bus.send(ended_ev) {
        warn!(%err, "kill_handler: could not emit CoreEvent::SessionEnded");
    }

    // Step 4: close every still-open tab (snapshot the registry under
    // the lock, then release before the async close_tab calls so the
    // lock is not held across await points).
    let open_tabs: Vec<TabHandle> = {
        let g = tab_registry.lock().expect("tab_registry lock poisoned");
        g.clone()
    };
    if open_tabs.is_empty() {
        debug!("kill_handler: no tabs to close after grace");
    } else {
        for handle in &open_tabs {
            if let Err(err) = mux.close_tab(handle).await {
                warn!(
                    tab = %handle,
                    %err,
                    "kill_handler: close_tab failed"
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_mux_zellij::ZellijMux;
    use ark_mux_zellij::executor::{CommandOutput, StubExecutor};
    use ark_types::channel;

    async fn mux_with_n_ok_closes(n: usize) -> (Arc<ZellijMux>, Arc<StubExecutor>) {
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let responses: Vec<CommandOutput> = (0..n)
            .map(|_| CommandOutput {
                status: ok_status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .collect();
        let (mux, stub) = ZellijMux::for_test(responses);
        (Arc::new(mux), stub)
    }

    fn count_close_tab_calls(stub: &StubExecutor) -> usize {
        stub.recorded_calls()
            .iter()
            .filter(|(_, args)| args.iter().any(|a| a == "close-tab-at-index"))
            .count()
    }

    fn session() -> SessionId {
        SessionId::new("kill")
    }

    #[tokio::test]
    async fn orchestrator_returns_before_grace_no_session_ended() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, mut rx) = channel(16);
        let (mux, stub) = mux_with_n_ok_closes(0).await;
        let registry = new_tab_registry();

        done.cancel();

        kill_handler(
            cancel.clone(),
            done,
            tx,
            mux.clone(),
            registry,
            session(),
            Duration::from_secs(5),
        )
        .await
        .expect("ok");

        assert!(cancel.is_cancelled(), "cancel must fire on entry");
        assert_eq!(
            count_close_tab_calls(&stub),
            0,
            "no tabs to close on fast-path"
        );

        // No SessionEnded must have been broadcast on the fast path.
        match rx.try_recv() {
            Err(_) => {}
            Ok(ev) => match ev {
                CoreEvent::SessionEnded { .. } => {
                    panic!("fast-path must not emit SessionEnded")
                }
                _ => {}
            },
        }
    }

    #[tokio::test]
    async fn grace_expiry_emits_session_ended_and_closes_tabs() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, mut rx) = channel(32);
        let (mux, stub) = mux_with_n_ok_closes(2).await;

        let tab_a = TabHandle::new("ark-kill", 1, "builder");
        let tab_b = TabHandle::new("ark-kill", 2, "log");

        let registry = new_tab_registry();
        apply_tab_event(&registry, &tab_a, true);
        apply_tab_event(&registry, &tab_b, true);
        assert_eq!(registry.lock().unwrap().len(), 2);

        kill_handler(
            cancel.clone(),
            done,
            tx.clone(),
            mux.clone(),
            registry,
            session(),
            Duration::from_millis(60),
        )
        .await
        .expect("ok");

        assert_eq!(count_close_tab_calls(&stub), 2);

        // Drain the bus and look for Log(grace expired) + SessionEnded.
        let mut saw_log = false;
        let mut saw_ended = false;
        for _ in 0..64 {
            match rx.try_recv() {
                Ok(CoreEvent::Log { level, message, .. })
                    if level == "warn" && message == "grace expired" =>
                {
                    saw_log = true;
                }
                Ok(CoreEvent::SessionEnded { .. }) => saw_ended = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_log, "expected Log(grace expired)");
        assert!(saw_ended, "expected CoreEvent::SessionEnded");
    }

    #[tokio::test]
    async fn closed_tab_is_removed_from_registry() {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let (tx, _rx) = channel(16);
        let (mux, stub) = mux_with_n_ok_closes(0).await;
        let registry = new_tab_registry();
        let tab = TabHandle::new("ark-kill", 1, "builder");

        apply_tab_event(&registry, &tab, true);
        apply_tab_event(&registry, &tab, false);
        assert!(registry.lock().unwrap().is_empty());

        kill_handler(
            cancel,
            done,
            tx,
            mux.clone(),
            registry,
            session(),
            Duration::from_millis(30),
        )
        .await
        .expect("ok");
        assert_eq!(
            count_close_tab_calls(&stub),
            0,
            "already-closed tab must NOT be re-closed"
        );
    }
}
