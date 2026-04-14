//! In-process event bus built on `tokio::sync::broadcast`.
//!
//! See cavekit-types-state-events.md R4. The supervisor owns the sender and
//! hands clones to the engine, orchestrator, state writer, and status piper.
//!
//! ## Lag semantics
//!
//! The underlying `tokio::sync::broadcast` channel inherently drops oldest
//! messages for slow receivers: once a subscriber falls behind by more than
//! the channel capacity, its next `recv().await` returns
//! [`tokio::sync::broadcast::error::RecvError::Lagged(n)`] exactly once,
//! reporting the count of skipped messages. Consumers **must** match
//! `Lagged(n)` and warn-log — they must not panic. Subsequent calls continue
//! from the oldest still-buffered message.

use crate::event::AgentEvent;

/// Broadcast sender for `AgentEvent` values. Clone freely.
pub type EventSink = tokio::sync::broadcast::Sender<AgentEvent>;

/// Broadcast receiver handed to a single subscriber.
pub type EventReceiver = tokio::sync::broadcast::Receiver<AgentEvent>;

/// Default channel capacity per cavekit-types-state-events R4. Can be
/// overridden via config; pass a custom value to [`channel`].
pub const DEFAULT_CAPACITY: usize = 256;

/// Construct an event bus with the given capacity.
///
/// Capacity is clamped to `>= 1` since `tokio::sync::broadcast::channel(0)`
/// panics; clamping lets callers pass user-supplied config without extra
/// validation.
pub fn channel(capacity: usize) -> (EventSink, EventReceiver) {
    tokio::sync::broadcast::channel(capacity.max(1))
}

/// Construct an event bus with the default capacity of
/// [`DEFAULT_CAPACITY`].
pub fn default_channel() -> (EventSink, EventReceiver) {
    channel(DEFAULT_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::AgentId;
    use crate::spec::AgentSpec;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    fn sample_started() -> AgentEvent {
        let id = AgentId::new("cavekit", "auth");
        let mut spec = AgentSpec::new(
            id,
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        spec.env = BTreeMap::new();
        AgentEvent::Started { spec }
    }

    #[test]
    fn capacity_zero_clamps_to_one() {
        let (tx, mut rx) = channel(0);
        let ev = sample_started();
        tx.send(ev.clone()).expect("send");
        let back = rx.try_recv().expect("recv");
        assert_eq!(back, ev);
    }

    #[test]
    fn default_channel_has_default_capacity() {
        let (tx, _rx) = default_channel();
        // broadcast::Sender does not expose capacity directly, but
        // receiver_count is a smoke check the channel exists and is empty.
        assert_eq!(tx.receiver_count(), 1);
        assert_eq!(DEFAULT_CAPACITY, 256);
    }

    #[tokio::test]
    async fn send_receive_roundtrip_started() {
        let (tx, mut rx) = default_channel();
        let ev = sample_started();
        tx.send(ev.clone()).expect("send");
        let got = rx.recv().await.expect("recv");
        assert_eq!(got, ev);
    }

    #[tokio::test]
    async fn lagged_receiver_gets_lagged_err() {
        let cap = 4usize;
        let (tx, mut rx) = channel(cap);
        let ev = sample_started();
        // Fill past capacity without draining — forces the slow-receiver path.
        for _ in 0..(cap + 10) {
            tx.send(ev.clone()).expect("send");
        }
        match rx.recv().await {
            Err(RecvError::Lagged(n)) => {
                assert!(n >= 10, "expected at least 10 skipped, got {n}");
            }
            other => panic!("expected Lagged, got {other:?}"),
        }
        // After consuming the Lagged report, the receiver resumes from the
        // oldest still-buffered message.
        let got = rx.recv().await.expect("post-lag recv");
        assert_eq!(got, ev);
    }

    #[test]
    fn no_subscribers_errors_on_send() {
        let (tx, rx) = default_channel();
        drop(rx);
        let ev = sample_started();
        assert!(tx.send(ev).is_err());
    }

    #[test]
    fn empty_try_recv_is_empty_error() {
        let (_tx, mut rx) = default_channel();
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }
}
