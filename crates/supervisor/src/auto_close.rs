//! Auto-close policy (T-072).
//!
//! Implements cavekit-supervisor.md R6:
//!
//! > - On `Done { outcome: Success }`: if `config.defaults.auto_close_on_done`,
//! >   close orchestrator's tabs via mux; if no tabs remain in session,
//! >   session dies naturally.
//! > - On `Done { outcome: Failed | Crashed }`: if
//! >   `config.defaults.auto_close_on_fail` (default false), close;
//! >   otherwise leave tabs for user review.
//! > - On `Done { outcome: Killed }`: if
//! >   `config.defaults.auto_close_on_kill` (default true), close.
//! > - Closing is per-orchestrator-tab, not session-level — leaves session
//! >   intact if user manually opened other tabs in it.
//! > - Final `status.json` reflects `phase: Done|Failed|Crashed|Killed`
//! >   regardless of close behavior.
//!
//! # What this module owns
//!
//! * [`AutoClosePolicy`] — the three `on_done`/`on_fail`/`on_kill` booleans
//!   with the kit-defined defaults.
//! * [`apply_auto_close_policy`] — map `Outcome` -> policy bool -> close
//!   every tab in the supplied `tabs` list via the mux.
//! * [`collect_opened_tabs`] — helper used by the caller (orchestration.rs)
//!   to accumulate the tab set from the event bus. Pairs naturally with
//!   the same pattern used by the T-070 kill handler.
//!
//! # What this module does **not** own
//!
//! * Status writing. `status.json` finalisation is T-069's
//!   `finalize_state` — this module never touches it.
//! * Session close. Tabs are closed per-handle; if the mux's session has
//!   no tabs left it self-terminates per zellij semantics — that is the
//!   mux layer's concern.
//! * Archiving the state dir (R6 overlap with finalize_state / T-062).
//! * Wiring into orchestration.rs — a later Tier 4 pass (or follow-up
//!   T-072 wiring) connects `apply_auto_close_policy` into the run loop.

use std::time::Instant;

use anyhow::Result;
use ark_core::Multiplexer;
use ark_types::{AgentEvent, AgentId, EventReceiver, EventSink, Outcome, TabHandle};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tracing::{debug, warn};

use ark_config::schema::{
    DEFAULT_AUTO_CLOSE_ON_DONE, DEFAULT_AUTO_CLOSE_ON_FAIL, DEFAULT_AUTO_CLOSE_ON_KILL,
};

/// Auto-close policy sourced from `config.defaults.auto_close_on_{done,fail,kill}`.
///
/// See cavekit-supervisor.md R6 + cavekit-config.md R3.
///
/// Defaults (per kit):
/// - `on_done = true`
/// - `on_fail = false`  (leave Failed/Crashed tabs up for user review)
/// - `on_kill = true`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoClosePolicy {
    /// Close tabs on `Outcome::Success`.
    pub on_done: bool,
    /// Close tabs on `Outcome::Failed`, `Outcome::Crashed`, `Outcome::Timeout`.
    pub on_fail: bool,
    /// Close tabs on `Outcome::Killed`.
    pub on_kill: bool,
}

impl Default for AutoClosePolicy {
    fn default() -> Self {
        Self {
            on_done: DEFAULT_AUTO_CLOSE_ON_DONE,
            on_fail: DEFAULT_AUTO_CLOSE_ON_FAIL,
            on_kill: DEFAULT_AUTO_CLOSE_ON_KILL,
        }
    }
}

impl AutoClosePolicy {
    /// Whether the policy calls for closing tabs on this outcome.
    ///
    /// `Outcome::Timeout` is treated as the fail branch per R6.
    pub fn should_close(&self, outcome: &Outcome) -> bool {
        match outcome {
            Outcome::Success { .. } => self.on_done,
            Outcome::Failed { .. } | Outcome::Crashed { .. } | Outcome::Timeout => self.on_fail,
            Outcome::Killed => self.on_kill,
        }
    }
}

/// Apply the auto-close policy for a completed agent.
///
/// Consults `policy.should_close(outcome)`. When true, closes every tab
/// in `tabs` via `mux.close_tab` and emits a matching `TabClosed` event
/// for each successful close.
///
/// # Behaviour guarantees
///
/// * **Per-tab, not session-level.** The mux trait's `close_tab` takes a
///   `TabHandle`; it does not touch the session. When the last tab in a
///   session closes, zellij itself reaps the session — R6 bullet 3.
/// * **Never panics.** A failed `close_tab` is logged at `warn!` and the
///   remaining tabs are still attempted. The function returns `Ok(())`.
/// * **Empty `tabs`** → no-op, no events, `Ok(())`.
/// * **Event emission is best-effort.** If the bus has no receivers the
///   send error is logged at `warn!` and iteration continues — we never
///   drop the close because the bus is quiet.
/// * **`session` argument is informational.** It is retained to match the
///   kit's signature and to give future muxes a session-scoped close path
///   without a trait churn. v1 ignores it; each `TabHandle` carries its
///   own `session` field.
///
/// Parameters mirror cavekit-supervisor.md R6 bullet 1 verbatim.
pub async fn apply_auto_close_policy(
    outcome: &Outcome,
    config: &AutoClosePolicy,
    mux: &dyn Multiplexer,
    session: &str,
    tabs: &[TabHandle],
    event_bus: &EventSink,
    agent_id: &AgentId,
) -> Result<()> {
    if !config.should_close(outcome) {
        debug!(
            agent = agent_id.as_str(),
            ?outcome,
            policy = ?config,
            "auto-close: policy declines; leaving tabs open"
        );
        return Ok(());
    }

    if tabs.is_empty() {
        debug!(
            agent = agent_id.as_str(),
            session, "auto-close: no tabs to close"
        );
        return Ok(());
    }

    debug!(
        agent = agent_id.as_str(),
        session,
        tab_count = tabs.len(),
        ?outcome,
        "auto-close: closing tabs"
    );

    for handle in tabs {
        match mux.close_tab(handle).await {
            Ok(()) => {
                // Emit TabClosed for each successfully-closed tab. Bus
                // may have no consumers post-shutdown; that's fine.
                let ev = AgentEvent::TabClosed {
                    id: agent_id.clone(),
                    tab_handle: handle.clone(),
                };
                if let Err(err) = event_bus.send(ev) {
                    // Not fatal — bus may be winding down.
                    debug!(
                        agent = agent_id.as_str(),
                        tab = %handle,
                        %err,
                        "auto-close: could not emit TabClosed (bus likely closed)"
                    );
                }
            }
            Err(err) => {
                warn!(
                    agent = agent_id.as_str(),
                    tab = %handle,
                    %err,
                    "auto-close: close_tab failed; continuing with remaining tabs"
                );
            }
        }
    }

    Ok(())
}

/// Drain the event bus up to `cutoff` and return every tab the agent opened
/// but did not close.
///
/// Used by orchestration.rs to hand `apply_auto_close_policy` a concrete
/// tab list without plumbing a dedicated tracker. The caller provides its
/// own `EventReceiver` (typically `event_bus.subscribe()` captured before
/// the engine started) so no events are missed.
///
/// Semantics:
/// * Processes `AgentEvent::TabOpened` / `TabClosed` events whose `id`
///   matches `agent_id`. Other agents' events are ignored — supervisor
///   buses are per-agent but we defend against accidental sharing.
/// * Stops when either (a) `Instant::now() >= cutoff`, (b) the receiver
///   reports `Closed`, or (c) `try_recv` drains and the deadline is met.
/// * Logs at `warn!` on `Lagged(n)` — the returned list may be stale;
///   callers treat it as best-effort.
///
/// # Note on usage
///
/// This is a convenience for the common case ("grab everything so far").
/// For strict tracking — e.g. the kill_handler's grace-window behaviour —
/// callers may prefer an explicit event loop.
pub async fn collect_opened_tabs(
    bus: &mut EventReceiver,
    agent_id: &AgentId,
    cutoff: Instant,
) -> Vec<TabHandle> {
    let mut open: Vec<TabHandle> = Vec::new();

    // First drain anything already queued without blocking.
    loop {
        match bus.try_recv() {
            Ok(ev) => merge_tab_event(&mut open, &ev, agent_id),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Lagged(n)) => {
                warn!(
                    skipped = n,
                    "auto-close: event bus lagged during tab collection; tab set may be incomplete"
                );
            }
            Err(TryRecvError::Closed) => return open,
        }
    }

    // Then wait for the remainder of the window, reading as events arrive.
    loop {
        let now = Instant::now();
        if now >= cutoff {
            return open;
        }
        let remaining = cutoff - now;
        match tokio::time::timeout(remaining, bus.recv()).await {
            Err(_) => return open, // deadline
            Ok(Ok(ev)) => merge_tab_event(&mut open, &ev, agent_id),
            Ok(Err(RecvError::Lagged(n))) => {
                warn!(
                    skipped = n,
                    "auto-close: event bus lagged during tab collection; tab set may be incomplete"
                );
            }
            Ok(Err(RecvError::Closed)) => return open,
        }
    }
}

fn merge_tab_event(open: &mut Vec<TabHandle>, ev: &AgentEvent, agent_id: &AgentId) {
    match ev {
        AgentEvent::TabOpened { id, tab_handle, .. } if id == agent_id => {
            if !open.iter().any(|h| h == tab_handle) {
                open.push(tab_handle.clone());
            }
        }
        AgentEvent::TabClosed { id, tab_handle, .. } if id == agent_id => {
            open.retain(|h| h != tab_handle);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{TabRole, channel};
    use async_trait::async_trait;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    /// Mux that records every `close_tab` call, optionally failing on a
    /// named tab to exercise the warn-and-continue branch.
    struct StubMux {
        closed: Mutex<Vec<TabHandle>>,
        fail_on: Option<String>,
    }

    impl StubMux {
        fn new() -> Self {
            Self {
                closed: Mutex::new(Vec::new()),
                fail_on: None,
            }
        }

        fn failing_on(name: impl Into<String>) -> Self {
            Self {
                closed: Mutex::new(Vec::new()),
                fail_on: Some(name.into()),
            }
        }

        fn closed(&self) -> Vec<TabHandle> {
            self.closed.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Multiplexer for StubMux {
        fn kind(&self) -> &'static str {
            "stub-auto-close"
        }

        async fn ensure_session(&self, _name: &str) -> Result<()> {
            Ok(())
        }

        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            _layout_path: &Path,
        ) -> Result<TabHandle> {
            Ok(TabHandle::new(session, 1, name))
        }

        async fn close_tab(&self, handle: &TabHandle) -> Result<()> {
            if let Some(bad) = &self.fail_on
                && handle.name == *bad
            {
                return Err(anyhow::anyhow!(
                    "synthetic close failure on {}",
                    handle.name
                ));
            }
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
        AgentId::new("cavekit", "ac")
    }

    fn tab(name: &str, idx: u32) -> TabHandle {
        TabHandle::new("ark-cavekit-ac", idx, name)
    }

    fn success() -> Outcome {
        Outcome::Success {
            artifacts: vec![PathBuf::from("/tmp/art")],
        }
    }

    fn failed() -> Outcome {
        Outcome::Failed {
            reason: "boom".into(),
        }
    }

    fn crashed() -> Outcome {
        Outcome::Crashed {
            reason: "panic".into(),
        }
    }

    // ---------- AutoClosePolicy defaults ----------

    #[test]
    fn default_matches_kit_defaults() {
        let p = AutoClosePolicy::default();
        assert_eq!(
            p,
            AutoClosePolicy {
                on_done: true,
                on_fail: false,
                on_kill: true,
            }
        );
        // And they match the config-crate constants exactly.
        assert_eq!(p.on_done, DEFAULT_AUTO_CLOSE_ON_DONE);
        assert_eq!(p.on_fail, DEFAULT_AUTO_CLOSE_ON_FAIL);
        assert_eq!(p.on_kill, DEFAULT_AUTO_CLOSE_ON_KILL);
    }

    #[test]
    fn should_close_branches() {
        let p = AutoClosePolicy::default();
        assert!(p.should_close(&success()));
        assert!(!p.should_close(&failed()));
        assert!(!p.should_close(&crashed()));
        assert!(!p.should_close(&Outcome::Timeout));
        assert!(p.should_close(&Outcome::Killed));

        // Flip on_fail and verify all three fail-branch variants close.
        let p2 = AutoClosePolicy {
            on_fail: true,
            ..Default::default()
        };
        assert!(p2.should_close(&failed()));
        assert!(p2.should_close(&crashed()));
        assert!(p2.should_close(&Outcome::Timeout));
    }

    // ---------- apply_auto_close_policy ----------

    #[tokio::test]
    async fn success_default_policy_closes_all_and_emits_tabclosed() {
        let mux = StubMux::new();
        let (tx, mut rx) = channel(16);
        let id = agent();
        let tabs = vec![tab("builder", 1), tab("log", 2)];

        apply_auto_close_policy(
            &success(),
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");

        let closed = mux.closed();
        assert_eq!(closed.len(), 2);
        assert!(closed.iter().any(|t| t.name == "builder"));
        assert!(closed.iter().any(|t| t.name == "log"));

        // Drain bus and look for a TabClosed for each.
        let mut closed_names: Vec<String> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::TabClosed { tab_handle, .. } = ev {
                closed_names.push(tab_handle.name);
            }
        }
        closed_names.sort();
        assert_eq!(closed_names, vec!["builder", "log"]);
    }

    #[tokio::test]
    async fn success_on_done_false_skips_all() {
        let mux = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1)];

        let p = AutoClosePolicy {
            on_done: false,
            ..Default::default()
        };
        apply_auto_close_policy(&success(), &p, &mux, "ark-cavekit-ac", &tabs, &tx, &id)
            .await
            .expect("ok");

        assert!(mux.closed().is_empty());
    }

    #[tokio::test]
    async fn failed_default_policy_skips_all() {
        let mux = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1), tab("log", 2)];

        apply_auto_close_policy(
            &failed(),
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");

        assert!(mux.closed().is_empty(), "default on_fail=false → no close");
    }

    #[tokio::test]
    async fn failed_on_fail_true_closes_tabs() {
        let mux = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1), tab("log", 2)];

        let p = AutoClosePolicy {
            on_fail: true,
            ..Default::default()
        };
        apply_auto_close_policy(&failed(), &p, &mux, "ark-cavekit-ac", &tabs, &tx, &id)
            .await
            .expect("ok");

        assert_eq!(mux.closed().len(), 2);
    }

    #[tokio::test]
    async fn killed_default_policy_closes_tabs() {
        let mux = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1)];

        apply_auto_close_policy(
            &Outcome::Killed,
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");

        assert_eq!(mux.closed().len(), 1);
    }

    #[tokio::test]
    async fn crashed_default_policy_skips() {
        let mux = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1)];

        apply_auto_close_policy(
            &crashed(),
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");

        assert!(mux.closed().is_empty());
    }

    #[tokio::test]
    async fn timeout_follows_fail_branch() {
        let mux_default = StubMux::new();
        let (tx, _rx) = channel(8);
        let id = agent();
        let tabs = vec![tab("builder", 1)];

        // Default on_fail=false → Timeout should NOT close.
        apply_auto_close_policy(
            &Outcome::Timeout,
            &AutoClosePolicy::default(),
            &mux_default,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");
        assert!(mux_default.closed().is_empty());

        // on_fail=true → Timeout SHOULD close.
        let mux_on = StubMux::new();
        let p = AutoClosePolicy {
            on_fail: true,
            ..Default::default()
        };
        apply_auto_close_policy(
            &Outcome::Timeout,
            &p,
            &mux_on,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("ok");
        assert_eq!(mux_on.closed().len(), 1);
    }

    #[tokio::test]
    async fn empty_tabs_is_noop_no_panic() {
        let mux = StubMux::new();
        let (tx, mut rx) = channel(8);
        let id = agent();

        apply_auto_close_policy(
            &success(),
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &[],
            &tx,
            &id,
        )
        .await
        .expect("ok");

        assert!(mux.closed().is_empty());
        // No events emitted.
        match rx.try_recv() {
            Err(_) => {}
            Ok(ev) => panic!("expected no events, got {ev:?}"),
        }
    }

    #[tokio::test]
    async fn close_failure_on_one_tab_continues_with_others() {
        // Mux fails on the "builder" tab; "log" and "review" still closed.
        let mux = StubMux::failing_on("builder");
        let (tx, mut rx) = channel(16);
        let id = agent();
        let tabs = vec![tab("builder", 1), tab("log", 2), tab("review", 3)];

        // Returns Ok(()) despite one close failing.
        apply_auto_close_policy(
            &success(),
            &AutoClosePolicy::default(),
            &mux,
            "ark-cavekit-ac",
            &tabs,
            &tx,
            &id,
        )
        .await
        .expect("returns Ok even if one close fails");

        // Only the successful closes were recorded.
        let closed = mux.closed();
        assert_eq!(closed.len(), 2);
        assert!(!closed.iter().any(|t| t.name == "builder"));
        assert!(closed.iter().any(|t| t.name == "log"));
        assert!(closed.iter().any(|t| t.name == "review"));

        // TabClosed events only emitted for the successful closes.
        let mut closed_names: Vec<String> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::TabClosed { tab_handle, .. } = ev {
                closed_names.push(tab_handle.name);
            }
        }
        closed_names.sort();
        assert_eq!(closed_names, vec!["log", "review"]);
    }

    // ---------- collect_opened_tabs ----------

    #[tokio::test]
    async fn collect_opened_tabs_accumulates_and_removes() {
        let (tx, mut rx) = channel(32);
        let id = agent();

        let tab_a = tab("builder", 1);
        let tab_b = tab("log", 2);

        // Emit one opened before we start collecting.
        tx.send(AgentEvent::TabOpened {
            id: id.clone(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab_a.clone(),
            label: "builder".into(),
        })
        .unwrap();

        let tx2 = tx.clone();
        let id2 = id.clone();
        let tab_b2 = tab_b.clone();
        let tab_a2 = tab_a.clone();
        let emitter = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx2.send(AgentEvent::TabOpened {
                id: id2.clone(),
                parent: None,
                role: TabRole::Log,
                tab_handle: tab_b2,
                label: "log".into(),
            })
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx2.send(AgentEvent::TabClosed {
                id: id2,
                tab_handle: tab_a2,
            })
            .unwrap();
        });

        let cutoff = Instant::now() + Duration::from_millis(120);
        let tabs = collect_opened_tabs(&mut rx, &id, cutoff).await;
        emitter.await.unwrap();

        // tab_a was opened then closed — dropped. tab_b remains.
        assert_eq!(tabs.len(), 1, "got {tabs:?}");
        assert_eq!(tabs[0].name, "log");
    }

    #[tokio::test]
    async fn collect_opened_tabs_ignores_other_agents() {
        let (tx, mut rx) = channel(16);
        let me = agent();
        let other = AgentId::new("cavekit", "other");

        tx.send(AgentEvent::TabOpened {
            id: other,
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab("foreign", 99),
            label: "foreign".into(),
        })
        .unwrap();
        tx.send(AgentEvent::TabOpened {
            id: me.clone(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab("mine", 1),
            label: "mine".into(),
        })
        .unwrap();

        let cutoff = Instant::now() + Duration::from_millis(30);
        let tabs = collect_opened_tabs(&mut rx, &me, cutoff).await;

        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "mine");
    }

    #[tokio::test]
    async fn collect_opened_tabs_returns_empty_on_cutoff_past() {
        let (_tx, mut rx) = channel(4);
        let id = agent();
        // Cutoff already elapsed.
        let cutoff = Instant::now() - Duration::from_millis(5);
        let tabs = collect_opened_tabs(&mut rx, &id, cutoff).await;
        assert!(tabs.is_empty());
    }
}
