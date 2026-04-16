//! Tool-permission dispatch router (T-108).
//!
//! On ACP `session/request_permission`, the host emits
//! `UserEvent:ark.acp.permission_requested`. Scene reactions respond via
//! the `acp.permit` op, which calls [`PermissionRouter::resolve`] with
//! the matching `request_id`. The router correlates request → response
//! via a one-shot channel so the ACP transport can await the decision.
//!
//! Stale requests (no scene reaction within [`PermissionRouter::timeout`])
//! are auto-rejected by [`PermissionRouter::expire_stale`], which the
//! event-loop tick should call periodically.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use crate::error::SceneError;

/// Default permission timeout: 300 000 ms (5 minutes).
pub const DEFAULT_PERMISSION_TIMEOUT_MS: u64 = 300_000;

/// Outcome of a permission decision, returned to the ACP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// Tool invocation is allowed for this request.
    Allow,
    /// Tool invocation is rejected for this single request.
    RejectOnce,
    /// Tool invocation is rejected; the agent should not re-ask for
    /// this tool during the current session.
    RejectAlways,
    /// No scene reaction responded before the timeout elapsed.
    Timeout,
}

/// Routes permission requests from ACP to scene reactions and back.
///
/// Each incoming `session/request_permission` is [`register`](Self::register)ed
/// with a unique `request_id` and `tool` name. The caller receives a
/// [`oneshot::Receiver`] that resolves once a scene reaction calls
/// `acp.permit` (routed to [`resolve`](Self::resolve)), or when
/// [`expire_stale`](Self::expire_stale) fires the timeout.
pub struct PermissionRouter {
    /// Pending permission requests awaiting a scene response.
    pending: HashMap<String, PendingPermission>,
    /// Default timeout for permission decisions.
    pub timeout: Duration,
}

/// Internal bookkeeping for one in-flight permission request.
struct PendingPermission {
    /// Correlation key from the ACP protocol.
    #[allow(dead_code)]
    request_id: String,
    /// Tool name the agent wants to invoke.
    #[allow(dead_code)]
    tool: String,
    /// Wall-clock time the request was registered.
    created_at: Instant,
    /// One-shot sender — fires exactly once when the outcome is known.
    response_tx: oneshot::Sender<PermissionOutcome>,
}

impl PermissionRouter {
    /// Create a new router with the given default timeout.
    pub fn new(timeout: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            timeout,
        }
    }

    /// Create a router with the default 300 000 ms timeout.
    pub fn with_default_timeout() -> Self {
        Self::new(Duration::from_millis(DEFAULT_PERMISSION_TIMEOUT_MS))
    }

    /// Register a new permission request. Returns a receiver that will
    /// yield the [`PermissionOutcome`] once the request is resolved or
    /// times out.
    pub fn register(
        &mut self,
        request_id: String,
        tool: String,
    ) -> oneshot::Receiver<PermissionOutcome> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(
            request_id.clone(),
            PendingPermission {
                request_id,
                tool,
                created_at: Instant::now(),
                response_tx: tx,
            },
        );
        rx
    }

    /// Resolve a pending permission request (called by the `acp.permit`
    /// op handler). Returns `Err` if `request_id` is not in the pending
    /// set (already resolved, expired, or never registered).
    pub fn resolve(
        &mut self,
        request_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), SceneError> {
        let pending = self
            .pending
            .remove(request_id)
            .ok_or_else(|| SceneError::PermissionNotFound {
                request_id: request_id.to_string(),
            })?;
        // If the receiver was dropped, the caller no longer cares —
        // silently discard the send failure.
        let _ = pending.response_tx.send(outcome);
        Ok(())
    }

    /// Check for timed-out requests and auto-reject them with
    /// [`PermissionOutcome::Timeout`]. Returns the `request_id`s of
    /// all expired entries.
    pub fn expire_stale(&mut self) -> Vec<String> {
        let now = Instant::now();
        let timeout = self.timeout;

        let expired_ids: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, p)| now.duration_since(p.created_at) >= timeout)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &expired_ids {
            if let Some(pending) = self.pending.remove(id) {
                let _ = pending.response_tx.send(PermissionOutcome::Timeout);
            }
        }

        expired_ids
    }

    /// Number of currently pending permission requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn register_and_resolve_allow() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));
        let rx = router.register("req-1".into(), "bash".into());

        assert_eq!(router.pending_count(), 1);
        router
            .resolve("req-1", PermissionOutcome::Allow)
            .expect("resolve should succeed");
        assert_eq!(router.pending_count(), 0);

        let outcome = rx.await.expect("channel should not be closed");
        assert_eq!(outcome, PermissionOutcome::Allow);
    }

    #[tokio::test]
    async fn register_and_resolve_reject_once() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));
        let rx = router.register("req-2".into(), "file_write".into());

        router
            .resolve("req-2", PermissionOutcome::RejectOnce)
            .expect("resolve should succeed");

        let outcome = rx.await.expect("channel should not be closed");
        assert_eq!(outcome, PermissionOutcome::RejectOnce);
    }

    #[tokio::test]
    async fn register_and_resolve_reject_always() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));
        let rx = router.register("req-3".into(), "rm_rf".into());

        router
            .resolve("req-3", PermissionOutcome::RejectAlways)
            .expect("resolve should succeed");

        let outcome = rx.await.expect("channel should not be closed");
        assert_eq!(outcome, PermissionOutcome::RejectAlways);
    }

    #[tokio::test]
    async fn resolve_unknown_request_id_errors() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));

        let result = router.resolve("nonexistent", PermissionOutcome::Allow);
        assert!(result.is_err(), "resolving unknown id should fail");
    }

    #[tokio::test]
    async fn expire_stale_auto_rejects() {
        // Use a zero timeout so entries expire immediately.
        let mut router = PermissionRouter::new(Duration::from_millis(0));
        let rx = router.register("req-stale".into(), "bash".into());

        // Give the clock a nudge (Instant::now() is monotonic, even 0ms
        // timeout will expire because created_at < now after the HashMap
        // insert).
        let expired = router.expire_stale();
        assert_eq!(expired, vec!["req-stale".to_string()]);
        assert_eq!(router.pending_count(), 0);

        let outcome = rx.await.expect("channel should deliver Timeout");
        assert_eq!(outcome, PermissionOutcome::Timeout);
    }

    #[tokio::test]
    async fn non_expired_entries_survive() {
        let mut router = PermissionRouter::new(Duration::from_secs(3600));
        let _rx = router.register("req-fresh".into(), "bash".into());

        let expired = router.expire_stale();
        assert!(expired.is_empty(), "fresh entry should not expire");
        assert_eq!(router.pending_count(), 1);
    }

    #[tokio::test]
    async fn multiple_concurrent_permissions() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));
        let rx1 = router.register("r1".into(), "bash".into());
        let rx2 = router.register("r2".into(), "file_read".into());
        let rx3 = router.register("r3".into(), "net".into());

        assert_eq!(router.pending_count(), 3);

        router
            .resolve("r2", PermissionOutcome::RejectOnce)
            .expect("r2 resolve");
        router
            .resolve("r1", PermissionOutcome::Allow)
            .expect("r1 resolve");
        router
            .resolve("r3", PermissionOutcome::RejectAlways)
            .expect("r3 resolve");

        assert_eq!(rx1.await.unwrap(), PermissionOutcome::Allow);
        assert_eq!(rx2.await.unwrap(), PermissionOutcome::RejectOnce);
        assert_eq!(rx3.await.unwrap(), PermissionOutcome::RejectAlways);
        assert_eq!(router.pending_count(), 0);
    }

    #[tokio::test]
    async fn double_resolve_errors() {
        let mut router = PermissionRouter::new(Duration::from_secs(60));
        let _rx = router.register("req-once".into(), "bash".into());

        router
            .resolve("req-once", PermissionOutcome::Allow)
            .expect("first resolve succeeds");

        let result = router.resolve("req-once", PermissionOutcome::Allow);
        assert!(result.is_err(), "second resolve should fail");
    }

    #[test]
    fn default_timeout_is_300_seconds() {
        let router = PermissionRouter::with_default_timeout();
        assert_eq!(router.timeout, Duration::from_millis(300_000));
    }
}
