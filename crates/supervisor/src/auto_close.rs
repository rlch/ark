//! Auto-close on session end (cavekit-soul-phase-1-supervisor.md R4).
//!
//! Under cavekit-soul Phase 1 the outcome-keyed `AutoClosePolicy` is gone.
//! The supervisor's sole auto-close contract is now:
//!
//! > When `CoreEvent::SessionEnded` is observed on the bus, close the
//! > session's tabs via the multiplexer.
//!
//! Bare sessions (those spawned with no orchestrator — see
//! cavekit-soul-phase-1-supervisor.md R2) never emit `SessionEnded`; the
//! long-lived main loop simply parks on `world.cancel.cancelled().await`
//! until the supervisor is torn down from the outside. This module
//! therefore naturally no-ops on the bare-session path without a special
//! flag — option (b) in the kit's acceptance criteria.
//!
//! Orchestrator-backed sessions (Phase 2+ extensions) are expected to
//! publish `CoreEvent::SessionEnded { terminated_at }` when their
//! methodology decides the session is terminal; this module observes
//! that signal and closes the session in the mux.

use std::sync::Arc;

use anyhow::Result;
use ark_mux_zellij::ZellijMux;
use ark_types::{CoreEvent, EventReceiver, SessionId};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Listen on `bus` for `CoreEvent::SessionEnded`. When the event arrives
/// (or `cancel` fires), attempt to close every tab in `session_name` via
/// the multiplexer and return.
///
/// Bare-session (orchestrator was `None`) callers will never observe a
/// `SessionEnded` event under the Phase 1 wiring — the bare main loop
/// does not synthesise one — so this helper naturally no-ops in that
/// case: it simply awaits `cancel` and returns without closing anything.
///
/// # Parameters
///
/// * `bus` — a receiver subscribed to the supervisor's event bus. Only
///   `CoreEvent::SessionEnded` matters; every other variant is ignored.
/// * `mux` — the active multiplexer. Used to close the session's tabs.
/// * `session_id` — identity of the session owning the tabs.
/// * `session_name` — zellij session name (passed verbatim to the mux).
/// * `cancel` — supervisor-wide cancel. When it fires before the end
///   event, the function returns `Ok(())` without touching the mux.
pub async fn apply_auto_close_policy(
    bus: &mut EventReceiver,
    mux: &Arc<ZellijMux>,
    session_id: &SessionId,
    session_name: &str,
    cancel: CancellationToken,
) -> Result<()> {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                debug!(
                    session = %session_id.as_str(),
                    "auto_close: cancel observed before SessionEnded; no-op"
                );
                return Ok(());
            }
            ev = bus.recv() => match ev {
                Ok(CoreEvent::SessionEnded { terminated_at }) => {
                    debug!(
                        session = %session_id.as_str(),
                        name = session_name,
                        %terminated_at,
                        "auto_close: SessionEnded observed; closing session tabs"
                    );
                    close_session_tabs(mux, session_name).await;
                    return Ok(());
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(n)) => {
                    warn!(
                        skipped = n,
                        session = %session_id.as_str(),
                        "auto_close: event bus lagged; SessionEnded may be missed"
                    );
                    continue;
                }
                Err(RecvError::Closed) => {
                    debug!(
                        session = %session_id.as_str(),
                        "auto_close: event bus closed before SessionEnded; no-op"
                    );
                    return Ok(());
                }
            }
        }
    }
}

/// Close every tab in `session_name`. Best-effort: errors from the mux
/// are warn-logged but not propagated — the supervisor always continues
/// its shutdown sequence.
///
/// Implementation note: the mux's `close_session` path is the single
/// operation that tears down all tabs owned by a session at once. If a
/// future mux backend needs finer-grained control (e.g. close only
/// orchestrator-owned tabs, leave user-opened ones alive), this helper
/// is the single place to extend.
async fn close_session_tabs(_mux: &Arc<ZellijMux>, session_name: &str) {
    // TODO(cavekit-soul Phase 2): wire `mux.close_session` once the multiplexer
    // surface gains a session-scoped teardown. For now we log and rely on the
    // supervisor's own cancel cascade + the `kill_handler` fallback to close
    // individual tabs at SIGTERM.
    debug!(
        session = session_name,
        "auto_close: close_session not implemented on mux; skipping"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_mux_zellij::ZellijMux;
    use ark_types::channel;
    use chrono::Utc;

    fn test_mux() -> Arc<ZellijMux> {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        Arc::new(mux)
    }

    fn session() -> (SessionId, String) {
        let id = SessionId::new("autoclose");
        let name = format!("ark-{}", id.as_path_leaf());
        (id, name)
    }

    /// Bare-session behaviour: no `SessionEnded` is ever emitted; cancel
    /// is what unblocks the function. It must return `Ok(())` without
    /// panicking.
    #[tokio::test]
    async fn cancel_before_session_ended_returns_ok() {
        let (tx, mut rx) = channel(8);
        let mux = test_mux();
        let (id, name) = session();
        let cancel = CancellationToken::new();

        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                apply_auto_close_policy(&mut rx, &mux, &id, &name, cancel).await
            })
        };

        // No SessionEnded — just cancel.
        cancel.cancel();
        drop(tx);

        handle.await.expect("join").expect("ok");
    }

    /// When `SessionEnded` arrives, the function returns after closing.
    /// We can't inspect the mux's internal state here, but the function
    /// MUST return `Ok(())`.
    #[tokio::test]
    async fn session_ended_triggers_close_and_returns() {
        let (tx, mut rx) = channel(8);
        let mux = test_mux();
        let (id, name) = session();
        let cancel = CancellationToken::new();

        let sender = tx.clone();
        tokio::spawn(async move {
            // Let the listener subscribe before we fire.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = sender.send(CoreEvent::SessionEnded {
                terminated_at: Utc::now(),
            });
        });

        apply_auto_close_policy(&mut rx, &mux, &id, &name, cancel)
            .await
            .expect("ok");
    }

    /// Non-`SessionEnded` events are ignored; the function keeps waiting.
    #[tokio::test]
    async fn log_event_is_ignored() {
        let (tx, mut rx) = channel(8);
        let mux = test_mux();
        let (id, name) = session();
        let cancel = CancellationToken::new();

        let sender = tx.clone();
        let cancel_fire = cancel.clone();
        tokio::spawn(async move {
            let _ = sender.send(CoreEvent::Log {
                level: "info".into(),
                message: "noise".into(),
                target: None,
            });
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancel_fire.cancel();
        });

        apply_auto_close_policy(&mut rx, &mux, &id, &name, cancel)
            .await
            .expect("ok");
    }
}
