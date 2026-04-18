pub mod env_paths;
pub mod event;
pub mod event_bus;
pub mod id;
// Cleanup P4-R7: `permission` module removed — policy types were
// salvaged to `extensions/claude-code/src/lib.rs` (re-declared locally)
// per the 2026-04-18 v0.1 pivot. ark-core no longer defines permission
// policy.
pub mod scope;
pub mod spec;
pub mod state_dir;
pub mod status;

pub use env_paths::{EnvPaths, EnvPathsError};
pub use event::{CoreEvent, ExitReason, ExtEvent, FlatEvent, LogLevel};
pub use event_bus::{DEFAULT_CAPACITY, EventReceiver, EventSink, channel, default_channel};
pub use id::SessionId;
pub use scope::{MUX_V1, is_v1_mux};
pub use spec::SessionSpec;
pub use state_dir::{StateLayout, StateLayoutError};
pub use status::SessionStatus;

/// Re-export of [`tokio_util::sync::CancellationToken`] for cooperative
/// cancellation across supervisor / engine / orchestrator tasks.
///
/// See cavekit-architecture.md R3 & R4.
pub use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests {
    #[test]
    fn cancellation_token_reexport_is_reachable() {
        let t: crate::CancellationToken = crate::CancellationToken::new();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }
}
