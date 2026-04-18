//! In-process event bus built on `tokio::sync::broadcast`.
//!
//! See cavekit-soul-phase-1-types.md R6. The supervisor owns the sender
//! and hands clones to the orchestrator, state writer, status piper, and
//! every extension that wants to observe core events.
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

use crate::event::CoreEvent;

/// Broadcast sender for `CoreEvent` values. Clone freely.
pub type EventSink = tokio::sync::broadcast::Sender<CoreEvent>;

/// Broadcast receiver handed to a single subscriber.
pub type EventReceiver = tokio::sync::broadcast::Receiver<CoreEvent>;

/// Default channel capacity per cavekit-soul-phase-1-types R6. Can be
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
    use chrono::Utc;
    use tokio::sync::broadcast::error::TryRecvError;

    fn sample_event() -> CoreEvent {
        CoreEvent::SessionEnded {
            terminated_at: Utc::now(),
            exit: crate::event::ExitReason::Normal,
        }
    }

    #[test]
    fn capacity_zero_clamps_to_one() {
        let (tx, mut rx) = channel(0);
        tx.send(sample_event()).expect("send");
        assert!(matches!(rx.try_recv(), Ok(CoreEvent::SessionEnded { .. })));
    }

    #[test]
    fn default_channel_has_default_capacity() {
        let (tx, _rx) = default_channel();
        assert_eq!(tx.receiver_count(), 1);
        assert_eq!(DEFAULT_CAPACITY, 256);
    }

    #[tokio::test]
    async fn send_receive_roundtrip() {
        let (tx, mut rx) = default_channel();
        tx.send(sample_event()).expect("send");
        let got = rx.recv().await.expect("recv");
        assert!(matches!(got, CoreEvent::SessionEnded { .. }));
    }

    #[test]
    fn no_subscribers_errors_on_send() {
        let (tx, rx) = default_channel();
        drop(rx);
        assert!(tx.send(sample_event()).is_err());
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
