//! Turn-inflight tracker (cavekit-scene R14 / R17; T-ACP.2c).
//!
//! Tracks whether any ACP `session/prompt` request is still in-flight
//! across every live session this supervisor drives. Used by the
//! scene reload-gate (T-11.1) to decide whether a `reload_scene`
//! firing is safe — per R14, reloads MUST defer until the current
//! agent turn returns (or times out) so scene mutations cannot land
//! between the prompt and its response without the user-visible
//! model diverging mid-stream.
//!
//! # Wire contract
//!
//! * `mark_inflight(session_id, jsonrpc_id)` — called by the ACP
//!   client adapter the instant a `session/prompt` request is
//!   dispatched. The `(session_id, jsonrpc_id)` pair is the stable
//!   correlation key; both values are opaque strings owned by
//!   [`acp_client::AcpClient`].
//! * `clear(session_id, jsonrpc_id, reason)` — called when the
//!   matching `session/prompt` response lands. `reason` is one of
//!   the canonical ACP stop reasons, captured in [`StopReason`].
//!   Late responses (the entry has already been cleared, e.g. by a
//!   cancel-and-timeout path) drop silently with `tracing::debug!`.
//! * `any_inflight()` — returns whether ANY session has an
//!   outstanding entry. The reload gate queries this before
//!   applying scene deltas.
//!
//! # Concurrency model
//!
//! The wait table is `Arc<Mutex<HashMap<Key, TurnState>>>`. Calls
//! are short — insert / remove / len — and never held across an
//! `.await`, so a `std::sync::Mutex` is enough. The tracker itself
//! is `Send + Sync + Clone`: cloning bumps the inner `Arc`, giving
//! the same wait table to every live site (supervisor, ACP client
//! adapter, scene runtime) without cross-crate plumbing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Stable correlation key for an outstanding `session/prompt` request.
///
/// `session_id` is the ACP session id (opaque string the engine
/// returned from `session/new`); `jsonrpc_id` is a per-request key the
/// ACP client mints to pair the request with its response. Using an
/// owned `String` pair (rather than `&str`) keeps the tracker
/// independent of the caller's lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TurnKey {
    /// ACP session id.
    pub session_id: String,
    /// JSON-RPC id the ACP client assigned to the `session/prompt`
    /// request.
    pub jsonrpc_id: String,
}

impl TurnKey {
    /// Construct a key from two owned strings.
    pub fn new(session_id: impl Into<String>, jsonrpc_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            jsonrpc_id: jsonrpc_id.into(),
        }
    }
}

/// Why the turn ended. Mirrors `agent_client_protocol::StopReason`
/// (R14 + R17) kept here as a local enum so the tracker crate does
/// not depend on the ACP crate directly.
///
/// The `ToString` round-trip (`StopReason::Cancelled → "cancelled"`)
/// mirrors the wire shape of ACP's `stopReason` field so telemetry
/// can surface the exact wire value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopReason {
    /// The turn ended successfully.
    EndTurn,
    /// The agent hit its max-token budget.
    MaxTokens,
    /// The agent hit its max-turn-requests budget.
    MaxTurnRequests,
    /// The agent refused to continue.
    Refusal,
    /// The turn was cancelled by the client via `session/cancel`.
    Cancelled,
}

impl StopReason {
    /// Wire-shape rendering of the stop reason — matches ACP
    /// `StopReason`'s `#[serde(rename_all = "snake_case")]`.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::MaxTurnRequests => "max_turn_requests",
            StopReason::Refusal => "refusal",
            StopReason::Cancelled => "cancelled",
        }
    }
}

/// Per-entry state tracked alongside the correlation key. Kept as a
/// dedicated struct (rather than a bare `()`) so future telemetry
/// data (start timestamp, turn depth, …) can slot in without
/// churning the table's type.
#[derive(Debug, Clone)]
struct TurnState {
    /// Instant the entry was marked inflight. `std::time::Instant`
    /// is monotonic; `Duration` lets downstream telemetry compute
    /// per-turn latency once the response clears the entry.
    started_at: std::time::Instant,
}

impl TurnState {
    fn now() -> Self {
        Self {
            started_at: std::time::Instant::now(),
        }
    }
}

/// Tracker handle — clone to share.
///
/// Every site that needs to observe or mutate the wait table (ACP
/// client's prompt adapter, reload gate, scene runtime) holds a
/// clone. Cloning is cheap (Arc bump).
///
/// The tracker deliberately does not expose the full key set —
/// callers only need mark / clear / any_inflight. Finer-grained
/// queries (per-session inflight check, key enumeration) are
/// intentionally deferred until a consumer needs them, to keep the
/// shared surface small.
#[derive(Debug, Clone, Default)]
pub struct TurnInflightTracker {
    inner: Arc<Mutex<HashMap<TurnKey, TurnState>>>,
}

impl TurnInflightTracker {
    /// Construct an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a new turn as inflight. Idempotent: re-marking the same
    /// `(session_id, jsonrpc_id)` pair overwrites the prior
    /// timestamp and logs a warn-level trace (this should never
    /// happen in practice — the ACP client mints monotonic
    /// `jsonrpc_id`s).
    pub fn mark_inflight(&self, key: TurnKey) {
        let mut guard = self.inner.lock().expect("turn-inflight mutex poisoned");
        if guard.insert(key.clone(), TurnState::now()).is_some() {
            tracing::warn!(
                target = "supervisor::turn_inflight",
                session_id = %key.session_id,
                jsonrpc_id = %key.jsonrpc_id,
                "duplicate mark_inflight — key already tracked, prior timestamp overwritten",
            );
        }
    }

    /// Clear a turn. Returns `true` if the entry was present, or
    /// `false` if the response arrived late (after cancel / timeout
    /// pruned the entry). Late clears drop silently with
    /// `tracing::debug!`.
    pub fn clear(&self, key: &TurnKey, reason: StopReason) -> bool {
        let mut guard = self.inner.lock().expect("turn-inflight mutex poisoned");
        match guard.remove(key) {
            Some(state) => {
                tracing::debug!(
                    target = "supervisor::turn_inflight",
                    session_id = %key.session_id,
                    jsonrpc_id = %key.jsonrpc_id,
                    stop_reason = %reason.as_wire_str(),
                    elapsed_ms = state.started_at.elapsed().as_millis() as u64,
                    "turn cleared",
                );
                true
            }
            None => {
                tracing::debug!(
                    target = "supervisor::turn_inflight",
                    session_id = %key.session_id,
                    jsonrpc_id = %key.jsonrpc_id,
                    stop_reason = %reason.as_wire_str(),
                    "late turn response dropped (entry already cleared)",
                );
                false
            }
        }
    }

    /// Whether ANY session currently has an outstanding entry.
    /// Consumed by the reload gate (T-11.1) at
    /// `SupervisorHandle::any_turn_inflight`.
    pub fn any_inflight(&self) -> bool {
        let guard = self.inner.lock().expect("turn-inflight mutex poisoned");
        !guard.is_empty()
    }

    /// Number of outstanding entries. Exposed for telemetry and
    /// tests — reload-gate callers should use [`any_inflight`].
    ///
    /// [`any_inflight`]: Self::any_inflight
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("turn-inflight mutex poisoned");
        guard.len()
    }

    /// Whether the tracker is empty. Equivalent to `!any_inflight()`
    /// but named to match `std` conventions.
    pub fn is_empty(&self) -> bool {
        !self.any_inflight()
    }
}

// ---------------------------------------------------------------------------
// SupervisorHandle surface (T-11.1 reload-gate)
// ---------------------------------------------------------------------------

/// Subset of the supervisor-handle surface T-11.1's reload-gate
/// consumes. Kept as a dedicated trait so the scene runtime /
/// reload gate can depend on this narrow interface (rather than a
/// concrete `SupervisorHandle` type) — see cavekit-supervisor.md R3
/// step 12, and the placeholder in `ark_scene::intent::SupervisorHandle`.
///
/// T-11.1 will add more methods for scene-reload orchestration; the
/// trait grows there. For T-ACP.2c the only method is
/// [`any_turn_inflight`](Self::any_turn_inflight).
pub trait TurnInflightQuery: Send + Sync {
    /// Whether ANY session currently has an outstanding turn.
    /// Queries the shared [`TurnInflightTracker`]. Used by the
    /// reload-gate to defer a pending `reload_scene` until every
    /// inflight turn has either returned or been cancelled.
    fn any_turn_inflight(&self) -> bool;
}

impl TurnInflightQuery for TurnInflightTracker {
    fn any_turn_inflight(&self) -> bool {
        self.any_inflight()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn mark_and_clear_happy_path() {
        let t = TurnInflightTracker::new();
        assert!(!t.any_inflight());
        let k = TurnKey::new("sess-1", "prompt-1");
        t.mark_inflight(k.clone());
        assert!(t.any_inflight());
        assert_eq!(t.len(), 1);
        let cleared = t.clear(&k, StopReason::EndTurn);
        assert!(cleared, "first clear must report entry was present");
        assert!(!t.any_inflight());
    }

    /// Late responses (entry already cleared by cancel-timeout path)
    /// drop silently and return `false`.
    #[test]
    fn clear_without_mark_is_late_noop() {
        let t = TurnInflightTracker::new();
        let k = TurnKey::new("sess-1", "late-1");
        let cleared = t.clear(&k, StopReason::Cancelled);
        assert!(!cleared, "late clear must report entry was absent");
        assert!(!t.any_inflight());
    }

    /// Clearing an already-cleared entry is a noop (idempotent).
    #[test]
    fn double_clear_is_idempotent() {
        let t = TurnInflightTracker::new();
        let k = TurnKey::new("sess-1", "p-1");
        t.mark_inflight(k.clone());
        assert!(t.clear(&k, StopReason::EndTurn));
        assert!(!t.clear(&k, StopReason::EndTurn)); // second clear is the late path
    }

    /// Multiple concurrent sessions are tracked independently;
    /// clearing one does not clear the other.
    #[test]
    fn concurrent_sessions_are_independent() {
        let t = TurnInflightTracker::new();
        let a = TurnKey::new("sess-A", "p-1");
        let b = TurnKey::new("sess-B", "p-1");
        t.mark_inflight(a.clone());
        t.mark_inflight(b.clone());
        assert_eq!(t.len(), 2);
        t.clear(&a, StopReason::EndTurn);
        assert!(t.any_inflight(), "sess-B still inflight");
        assert_eq!(t.len(), 1);
        t.clear(&b, StopReason::EndTurn);
        assert!(!t.any_inflight());
    }

    /// A single session can have multiple outstanding prompts with
    /// distinct jsonrpc ids. Each entry is independent.
    #[test]
    fn same_session_different_ids_are_independent() {
        let t = TurnInflightTracker::new();
        let k1 = TurnKey::new("sess-1", "p-1");
        let k2 = TurnKey::new("sess-1", "p-2");
        t.mark_inflight(k1.clone());
        t.mark_inflight(k2.clone());
        assert_eq!(t.len(), 2);
        t.clear(&k1, StopReason::Cancelled);
        assert!(t.any_inflight());
        t.clear(&k2, StopReason::EndTurn);
        assert!(!t.any_inflight());
    }

    /// StopReason's wire-shape rendering matches ACP's snake_case convention.
    #[test]
    fn stop_reason_wire_shapes() {
        assert_eq!(StopReason::EndTurn.as_wire_str(), "end_turn");
        assert_eq!(StopReason::MaxTokens.as_wire_str(), "max_tokens");
        assert_eq!(StopReason::MaxTurnRequests.as_wire_str(), "max_turn_requests");
        assert_eq!(StopReason::Refusal.as_wire_str(), "refusal");
        assert_eq!(StopReason::Cancelled.as_wire_str(), "cancelled");
    }

    /// Cloning the tracker yields a shared view — marks on one
    /// handle show up on the other.
    #[test]
    fn cloning_shares_inner_table() {
        let t = TurnInflightTracker::new();
        let t2 = t.clone();
        let k = TurnKey::new("sess-1", "p-1");
        t.mark_inflight(k.clone());
        assert!(t2.any_inflight());
        t2.clear(&k, StopReason::EndTurn);
        assert!(!t.any_inflight());
    }

    /// The `TurnInflightQuery` trait surface returns the same
    /// answer as the concrete tracker. Confirms R14 reload-gate
    /// can consume via trait object without extra plumbing.
    #[test]
    fn trait_any_turn_inflight_matches_concrete() {
        let t: Arc<dyn TurnInflightQuery> =
            Arc::new(TurnInflightTracker::new());
        assert!(!t.any_turn_inflight());
    }

    /// Stress test — fan out marks + clears across threads and
    /// confirm the tracker settles to zero.
    #[test]
    fn concurrent_mark_clear_settles_to_zero() {
        let t = TurnInflightTracker::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let threads = 8usize;
        let per = 64usize;
        let mut handles = Vec::new();
        for tid in 0..threads {
            let t = t.clone();
            let counter = counter.clone();
            handles.push(thread::spawn(move || {
                for i in 0..per {
                    let k =
                        TurnKey::new(format!("sess-{tid}"), format!("p-{i}"));
                    t.mark_inflight(k.clone());
                    counter.fetch_add(1, Ordering::SeqCst);
                    t.clear(&k, StopReason::EndTurn);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panic");
        }
        assert_eq!(counter.load(Ordering::SeqCst), threads * per);
        assert!(
            !t.any_inflight(),
            "all marks must be cleared; residual len = {}",
            t.len()
        );
    }
}
