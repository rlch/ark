//! Turn-inflight tracker for ACP sessions (T-107).
//!
//! Maintains an [`AtomicBool`] per tracker that is `true` whenever at least one
//! ACP turn is in-flight (i.e. a `session/prompt` has been dispatched but no
//! response with a `stopReason` has arrived yet). The
//! [`TurnInflightTracker::any_inflight`] method is the reload gate —
//! hot-reload waits for all turns to complete before applying a new scene.
//!
//! Pending turns are tracked in a wait-table keyed by `(session_id, jsonrpc_id)`.
//! Late completions for unknown keys are silently dropped with a `debug!` log.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Key for the pending-turn wait table.
type TurnKey = (String, u64);

/// Tracks whether any ACP turn is in-flight across all sessions.
/// Used as a reload gate — hot-reload waits for turns to complete.
#[derive(Debug)]
pub struct TurnInflightTracker {
    /// Fast-path flag: `true` while at least one turn is pending.
    inflight: AtomicBool,
    /// Wait table: `(session_id, jsonrpc_id) -> start instant`.
    pending: Mutex<HashMap<TurnKey, Instant>>,
}

impl TurnInflightTracker {
    /// Create a new tracker with no pending turns.
    pub fn new() -> Self {
        Self {
            inflight: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Mark a turn as started. Inserts the `(session_id, request_id)` pair
    /// into the wait table and sets the inflight flag.
    pub async fn start_turn(&self, session_id: String, request_id: u64) {
        let mut pending = self.pending.lock().await;
        pending.insert((session_id, request_id), Instant::now());
        self.inflight.store(true, Ordering::Release);
    }

    /// Called when a response with a `stopReason` arrives. Removes the
    /// pending entry and clears the inflight flag if no turns remain.
    ///
    /// Late completions for unknown keys are dropped with a `debug!` log.
    pub async fn complete_turn(&self, session_id: &str, request_id: u64) {
        let mut pending = self.pending.lock().await;
        let key = (session_id.to_owned(), request_id);
        if pending.remove(&key).is_none() {
            tracing::debug!(
                session_id,
                request_id,
                "late or duplicate turn completion — key not in wait table"
            );
            return;
        }
        if pending.is_empty() {
            self.inflight.store(false, Ordering::Release);
        }
    }

    /// Returns `true` if any turn is currently in-flight.
    ///
    /// This is the reload gate check — callers should wait for this to return
    /// `false` before applying a hot-reload.
    pub fn any_inflight(&self) -> bool {
        self.inflight.load(Ordering::Acquire)
    }

    /// Number of turns currently pending.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

impl Default for TurnInflightTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_and_complete_single_turn() {
        let tracker = TurnInflightTracker::new();
        assert!(!tracker.any_inflight());

        tracker.start_turn("s1".into(), 1).await;
        assert!(tracker.any_inflight());
        assert_eq!(tracker.pending_count().await, 1);

        tracker.complete_turn("s1", 1).await;
        assert!(!tracker.any_inflight());
        assert_eq!(tracker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn multiple_concurrent_turns() {
        let tracker = TurnInflightTracker::new();

        tracker.start_turn("s1".into(), 1).await;
        tracker.start_turn("s2".into(), 2).await;
        tracker.start_turn("s1".into(), 3).await;
        assert!(tracker.any_inflight());
        assert_eq!(tracker.pending_count().await, 3);

        tracker.complete_turn("s1", 1).await;
        assert!(tracker.any_inflight()); // still 2 pending
        assert_eq!(tracker.pending_count().await, 2);

        tracker.complete_turn("s2", 2).await;
        assert!(tracker.any_inflight()); // still 1 pending
        assert_eq!(tracker.pending_count().await, 1);

        tracker.complete_turn("s1", 3).await;
        assert!(!tracker.any_inflight());
        assert_eq!(tracker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn late_completion_no_panic() {
        let tracker = TurnInflightTracker::new();

        // Complete a turn that was never started — should not panic.
        tracker.complete_turn("unknown", 99).await;
        assert!(!tracker.any_inflight());
        assert_eq!(tracker.pending_count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_completion() {
        let tracker = TurnInflightTracker::new();

        tracker.start_turn("s1".into(), 1).await;
        tracker.complete_turn("s1", 1).await;
        assert!(!tracker.any_inflight());

        // Second completion of same key — should not panic.
        tracker.complete_turn("s1", 1).await;
        assert!(!tracker.any_inflight());
    }
}
