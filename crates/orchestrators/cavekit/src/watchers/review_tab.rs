//! T-080: Phase detection + review-tab spawn/close watcher
//! (cavekit-orchestrator-cavekit R6).
//!
//! Runs as a sibling task to the T-079 ralph-loop watcher. Consumes
//! `PhaseTransition` events off the shared event bus and drives the multiplexer:
//!
//! - Phase transitions to a "review-like" phase → spawn a review tab via
//!   `mux.create_tab(session, "review", layout_path)` and emit
//!   `TabOpened { role: Reviewer, label: "review" }`.
//! - Phase transitions away from a review phase (while a review tab is open)
//!   → `mux.close_tab(handle)` + emit `TabClosed`.
//! - Transitions between two review-like phases (e.g. `reviewing` → `check`)
//!   are no-ops (dedupe).
//!
//! ## Review-phase heuristic
//!
//! A phase string is treated as a review phase when its lowercase form
//! contains any of `"review"`, `"check"`, or `"inspect"`. This covers the
//! cavekit Hunt-lifecycle terms (`Check`, `Revise`, `Review`) and common
//! synonyms used in ralph-loop writers. The matcher is swappable via the
//! `review_phase_matcher` parameter so future callers can tighten or relax it
//! (e.g. add `"audit"`) without touching the watcher core.
//!
//! ## Layout resolution
//!
//! The kit (R6) defines the review-tab layout as the stem `"review"`. The
//! `create_tab` caller resolves that stem against the layout directory (see
//! cavekit-mux-zellij R2), so we pass a bare `PathBuf::from("review")` as the
//! `layout_path` just like the builder-tab code in `lib.rs` does for
//! `"builder"`.
//!
//! Disabled by default; activated only when the `enabled` gate is true
//! (maps to `config.orchestrator.cavekit.spawn_review_tab`).
//!
//! This watcher does NOT emit `PhaseTransition` events — it consumes them.
//! The T-079 ralph-loop watcher is the sole producer in v1.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use ark_core::Multiplexer;
use ark_types::{
    AgentEvent, AgentId, CancellationToken, EventReceiver, EventSink, TabHandle, TabRole,
};

/// Default "is this a review phase?" predicate. Lowercases the input and
/// tests for any of the three substrings `"review"`, `"check"`, `"inspect"`.
pub fn default_review_phase_matcher(phase: &str) -> bool {
    let lowered = phase.to_ascii_lowercase();
    lowered.contains("review") || lowered.contains("check") || lowered.contains("inspect")
}

/// Public entry point — see module docs.
///
/// Spawned alongside the ralph-loop watcher. Listens for `PhaseTransition`
/// events matching `id` and drives the mux accordingly. Returns `Ok(())` on
/// cancel or on channel close.
pub async fn watch_phase_and_review(
    _cwd: PathBuf,
    id: AgentId,
    bus_rx: EventReceiver,
    mux: Arc<dyn Multiplexer>,
    session: String,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
) -> Result<()> {
    watch_phase_and_review_with(
        id,
        bus_rx,
        mux,
        session,
        tx,
        cancel,
        enabled,
        default_review_phase_matcher,
    )
    .await
}

/// Variant of [`watch_phase_and_review`] with an injectable phase matcher.
/// Exposed primarily for tests and for future callers that want to tighten
/// or loosen the review-phase heuristic (e.g. add `"audit"`).
pub async fn watch_phase_and_review_with<F>(
    id: AgentId,
    mut bus_rx: EventReceiver,
    mux: Arc<dyn Multiplexer>,
    session: String,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
    is_review_phase: F,
) -> Result<()>
where
    F: Fn(&str) -> bool + Send + 'static,
{
    if !enabled {
        return Ok(());
    }

    let review_layout = PathBuf::from("review");
    let mut open_tab: Option<TabHandle> = None;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break;
            }
            res = bus_rx.recv() => {
                match res {
                    Ok(AgentEvent::PhaseTransition { id: ev_id, to, .. }) => {
                        // Filter by agent id — the bus is shared across agents.
                        if ev_id != id {
                            continue;
                        }
                        let now_review = is_review_phase(&to);
                        match (open_tab.as_ref(), now_review) {
                            // Entering review phase and no tab yet: spawn.
                            (None, true) => {
                                match mux
                                    .create_tab(&session, "review", &review_layout)
                                    .await
                                {
                                    Ok(handle) => {
                                        let _ = tx.send(AgentEvent::TabOpened {
                                            id: id.clone(),
                                            parent: None,
                                            role: TabRole::Reviewer,
                                            tab_handle: handle.clone(),
                                            label: "review".to_string(),
                                        });
                                        open_tab = Some(handle);
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            session = %session,
                                            "watch_phase_and_review: create_tab failed",
                                        );
                                    }
                                }
                            }
                            // Review phase → review phase: dedupe, no-op.
                            (Some(_), true) => {}
                            // Leaving review phase with a tab open: close it.
                            //
                            // F-423: do NOT clear local state or emit TabClosed
                            // unless mux.close_tab succeeds. Otherwise supervisors
                            // (see F-086 tab_registry) would drop a tab that's
                            // still live in the mux, leaking it. On failure we
                            // keep the handle and log; the next review→non-review
                            // transition will retry the close.
                            (Some(_), false) => {
                                let handle = open_tab.as_ref().expect("some").clone();
                                match mux.close_tab(&handle).await {
                                    Ok(()) => {
                                        // Clear local state only on success.
                                        open_tab = None;
                                        let _ = tx.send(AgentEvent::TabClosed {
                                            id: id.clone(),
                                            tab_handle: handle,
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            tab = %handle,
                                            "watch_phase_and_review: close_tab failed; keeping handle for retry, NOT emitting TabClosed",
                                        );
                                        // open_tab intentionally preserved.
                                    }
                                }
                            }
                            // No tab, non-review phase: nothing to do.
                            (None, false) => {}
                        }
                    }
                    // Non-PhaseTransition events are ignored.
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "watch_phase_and_review lagged on event bus",
                        );
                        continue;
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{Outcome, channel};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_id() -> AgentId {
        AgentId::new("cavekit", "review-watch")
    }

    // ------- Stub mux -------------------------------------------------------

    struct StubMux {
        created: Mutex<Vec<(String, String, PathBuf)>>,
        closed: Mutex<Vec<TabHandle>>,
        next_index: Mutex<u32>,
        // F-423: when non-empty, each call to close_tab pops one from the
        // front. An entry of `true` means "fail this call".
        close_failures: Mutex<std::collections::VecDeque<bool>>,
    }

    impl StubMux {
        fn new() -> Self {
            Self {
                created: Mutex::new(Vec::new()),
                closed: Mutex::new(Vec::new()),
                next_index: Mutex::new(1),
                close_failures: Mutex::new(std::collections::VecDeque::new()),
            }
        }

        fn created(&self) -> Vec<(String, String, PathBuf)> {
            self.created.lock().unwrap().clone()
        }

        fn closed(&self) -> Vec<TabHandle> {
            self.closed.lock().unwrap().clone()
        }

        /// Schedule per-invocation close_tab outcomes. `[true, false]` means
        /// the first call errors, the second succeeds.
        fn set_close_failures(&self, failures: Vec<bool>) {
            *self.close_failures.lock().unwrap() = failures.into();
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
            layout_path: &Path,
        ) -> Result<TabHandle> {
            let mut idx = self.next_index.lock().unwrap();
            let tab_index = *idx;
            *idx += 1;
            self.created.lock().unwrap().push((
                session.to_string(),
                name.to_string(),
                layout_path.to_path_buf(),
            ));
            Ok(TabHandle::new(session, tab_index, name))
        }
        async fn close_tab(&self, handle: &TabHandle) -> Result<()> {
            let fail = self
                .close_failures
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(false);
            if fail {
                return Err(anyhow::anyhow!("stub close_tab: simulated failure"));
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

    // ------- helpers --------------------------------------------------------

    fn phase(id: &AgentId, from: Option<&str>, to: &str) -> AgentEvent {
        AgentEvent::PhaseTransition {
            id: id.clone(),
            from: from.map(|s| s.to_string()),
            to: to.to_string(),
        }
    }

    fn drain(rx: &mut ark_types::EventReceiver) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        out
    }

    async fn wait_for<F: Fn(&AgentEvent) -> bool>(
        rx: &mut ark_types::EventReceiver,
        pred: F,
        timeout: Duration,
    ) -> Vec<AgentEvent> {
        let start = std::time::Instant::now();
        let mut collected = Vec::new();
        while start.elapsed() < timeout {
            match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                Ok(Ok(ev)) => {
                    let matched = pred(&ev);
                    collected.push(ev);
                    if matched {
                        collected.extend(drain(rx));
                        return collected;
                    }
                }
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        collected
    }

    // ------- tests ----------------------------------------------------------

    #[tokio::test]
    async fn disabled_returns_ok_immediately() {
        let mux: Arc<dyn Multiplexer> = Arc::new(StubMux::new());
        let (tx, rx) = channel(16);
        let cancel = CancellationToken::new();
        watch_phase_and_review(
            PathBuf::from("/tmp"),
            make_id(),
            rx,
            mux.clone(),
            "ark".to_string(),
            tx,
            cancel,
            false,
        )
        .await
        .expect("disabled ok");
    }

    #[tokio::test]
    async fn building_to_reviewing_spawns_review_tab() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark-cavekit-review-watch".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        tx.send(phase(&id, Some("building"), "reviewing"))
            .expect("send");

        let got = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { role, .. } if *role == TabRole::Reviewer),
            Duration::from_secs(2),
        )
        .await;

        assert!(
            got.iter().any(|e| matches!(
                e,
                AgentEvent::TabOpened { role, label, .. }
                    if *role == TabRole::Reviewer && label == "review"
            )),
            "expected TabOpened(role=Reviewer, label=review); got {got:?}"
        );

        let created = mux.created();
        assert_eq!(created.len(), 1, "one create_tab call; got {created:?}");
        assert_eq!(created[0].0, "ark-cavekit-review-watch");
        assert_eq!(created[0].1, "review");
        assert_eq!(created[0].2, PathBuf::from("review"));

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn reviewing_to_reviewing_dedupes() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        tx.send(phase(&id, None, "reviewing")).expect("send 1");
        let _ = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { .. }),
            Duration::from_secs(2),
        )
        .await;

        // Second "reviewing" (even though from→to is identical, this watcher
        // still sees it as a discrete bus event — it must dedupe anyway).
        tx.send(phase(&id, Some("reviewing"), "reviewing"))
            .expect("send 2");
        tokio::time::sleep(Duration::from_millis(100)).await;

        let created = mux.created();
        assert_eq!(
            created.len(),
            1,
            "expected exactly one create_tab despite two review events; got {created:?}",
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn reviewing_to_building_closes_review_tab() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Enter review first.
        tx.send(phase(&id, Some("building"), "reviewing"))
            .expect("send 1");
        let _ = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { .. }),
            Duration::from_secs(2),
        )
        .await;

        // Leave review.
        tx.send(phase(&id, Some("reviewing"), "building"))
            .expect("send 2");
        let got = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabClosed { .. }),
            Duration::from_secs(2),
        )
        .await;
        assert!(
            got.iter()
                .any(|e| matches!(e, AgentEvent::TabClosed { .. })),
            "expected TabClosed; got {got:?}"
        );

        let closed = mux.closed();
        assert_eq!(closed.len(), 1, "one close_tab; got {closed:?}");
        assert_eq!(closed[0].name, "review");

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn review_to_check_keeps_tab_open() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        tx.send(phase(&id, Some("building"), "reviewing"))
            .expect("send 1");
        let _ = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { .. }),
            Duration::from_secs(2),
        )
        .await;

        tx.send(phase(&id, Some("reviewing"), "check"))
            .expect("send 2");
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Both "review" and "check" count as review phases — no spawn, no close.
        assert_eq!(mux.created().len(), 1);
        assert_eq!(mux.closed().len(), 0);

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn check_to_done_closes_review_tab() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        tx.send(phase(&id, None, "check")).expect("send 1");
        let _ = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { .. }),
            Duration::from_secs(2),
        )
        .await;

        tx.send(phase(&id, Some("check"), "done")).expect("send 2");
        let got = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabClosed { .. }),
            Duration::from_secs(2),
        )
        .await;
        assert!(
            got.iter()
                .any(|e| matches!(e, AgentEvent::TabClosed { .. })),
            "expected TabClosed; got {got:?}"
        );
        assert_eq!(mux.closed().len(), 1);

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn non_phase_events_are_ignored() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, _rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Throw a variety of non-PhaseTransition events at the watcher.
        tx.send(AgentEvent::Iteration {
            id: id.clone(),
            n: 1,
            max: None,
        })
        .expect("send iter");
        tx.send(AgentEvent::Progress {
            id: id.clone(),
            done: 1,
            total: 10,
            label: None,
        })
        .expect("send prog");
        tx.send(AgentEvent::Done {
            id: id.clone(),
            outcome: Outcome::Success {
                artifacts: Vec::new(),
            },
        })
        .expect("send done");

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(mux.created().is_empty(), "no tabs should be created");
        assert!(mux.closed().is_empty(), "no tabs should be closed");

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn foreign_agent_id_is_ignored() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, _rx_out) = channel(32);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();
        let other = AgentId::new("cavekit", "someone-else");

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        tx.send(phase(&other, None, "reviewing")).expect("send");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(mux.created().is_empty());

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn cancel_returns_ok() {
        let mux: Arc<dyn Multiplexer> = Arc::new(StubMux::new());
        let (tx, _rx_out) = channel(16);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            make_id(),
            rx_in,
            mux,
            "ark".to_string(),
            tx,
            cancel.clone(),
            true,
        ));

        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("join timeout")
            .expect("join");
        result.expect("watcher ok");
    }

    #[tokio::test]
    async fn lagged_continues_without_error() {
        let mux = Arc::new(StubMux::new());
        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        // Tiny capacity to force a Lagged report.
        let (tx, _rx_out) = channel(2);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Flood with harmless (non-review) transitions so the watcher's own
        // broadcast receiver overflows and reports Lagged on next recv. We
        // stick to non-review phases here so no mux interactions are expected.
        for _ in 0..32 {
            let _ = tx.send(phase(&id, Some("building"), "building"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now send a real review transition — watcher must have continued past
        // the Lagged report and still handle it.
        let _ = tx.send(phase(&id, Some("building"), "reviewing"));

        // Give it time to handle the review event.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if !mux.created().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            mux.created().len(),
            1,
            "expected the watcher to recover from Lagged and still spawn",
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // ------- F-423 regression ----------------------------------------------

    /// F-423: when `mux.close_tab` fails, the watcher must NOT emit
    /// `TabClosed` (that would desync supervisors from F-086 tab_registry),
    /// must keep the handle in local state, and must retry on the next
    /// review→non-review transition. Re-entering review while the handle is
    /// still tracked must NOT spawn a second tab.
    #[tokio::test]
    async fn close_tab_failure_retries_without_emitting_tabclosed() {
        let mux = Arc::new(StubMux::new());
        // First close_tab errors, second succeeds.
        mux.set_close_failures(vec![true, false]);

        let mux_dyn: Arc<dyn Multiplexer> = mux.clone();
        let (tx, mut rx_out) = channel(64);
        let rx_in = tx.subscribe();
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_phase_and_review(
            PathBuf::from("/tmp"),
            id.clone(),
            rx_in,
            mux_dyn,
            "ark".to_string(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // 1. building -> reviewing: spawn the review tab.
        tx.send(phase(&id, Some("building"), "reviewing"))
            .expect("send 1");
        let _ = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabOpened { .. }),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(mux.created().len(), 1);

        // 2. reviewing -> building: close_tab returns Err. MUST NOT emit
        //    TabClosed. Drain briefly and assert.
        tx.send(phase(&id, Some("reviewing"), "building"))
            .expect("send 2");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events_after_first_close = drain(&mut rx_out);
        assert!(
            !events_after_first_close
                .iter()
                .any(|e| matches!(e, AgentEvent::TabClosed { .. })),
            "expected NO TabClosed after failed close_tab; got {events_after_first_close:?}"
        );
        // No successful close was recorded.
        assert_eq!(
            mux.closed().len(),
            0,
            "closed list should only contain successful closes; got {:?}",
            mux.closed()
        );

        // 3. building -> reviewing again: handle still tracked → MUST NOT
        //    spawn a new tab (existing one still "live" from watcher's POV).
        tx.send(phase(&id, Some("building"), "reviewing"))
            .expect("send 3");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            mux.created().len(),
            1,
            "no new tab should spawn while handle is still tracked",
        );

        // 4. reviewing -> building: close_tab succeeds this time → emit
        //    TabClosed, clear handle.
        tx.send(phase(&id, Some("reviewing"), "building"))
            .expect("send 4");
        let got = wait_for(
            &mut rx_out,
            |e| matches!(e, AgentEvent::TabClosed { .. }),
            Duration::from_secs(2),
        )
        .await;
        assert!(
            got.iter()
                .any(|e| matches!(e, AgentEvent::TabClosed { .. })),
            "expected TabClosed after successful retry; got {got:?}"
        );
        assert_eq!(
            mux.closed().len(),
            1,
            "one successful close; got {:?}",
            mux.closed()
        );

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    }

    // ------- matcher unit tests --------------------------------------------

    #[test]
    fn matcher_accepts_review_check_inspect() {
        assert!(default_review_phase_matcher("review"));
        assert!(default_review_phase_matcher("reviewing"));
        assert!(default_review_phase_matcher("Check"));
        assert!(default_review_phase_matcher("CHECKING"));
        assert!(default_review_phase_matcher("inspect"));
        assert!(default_review_phase_matcher("inspecting"));
        assert!(default_review_phase_matcher("Review Phase"));
    }

    #[test]
    fn matcher_rejects_unrelated_phases() {
        assert!(!default_review_phase_matcher("building"));
        assert!(!default_review_phase_matcher("done"));
        assert!(!default_review_phase_matcher("draft"));
        assert!(!default_review_phase_matcher(""));
    }
}
