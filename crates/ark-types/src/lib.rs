pub mod env_paths;
pub mod event;
pub mod event_bus;
pub mod id;
pub mod scope;
pub mod spec;
pub mod state_dir;
pub mod status;

pub use env_paths::{EnvPaths, EnvPathsError};
pub use event::{
    AgentEvent, LogLevel, MessageRole, Outcome, PermissionDecision, Severity, TabHandle, TabRole,
};
pub use event_bus::{DEFAULT_CAPACITY, EventReceiver, EventSink, channel, default_channel};
pub use id::{AgentId, AgentIdParseError};
pub use scope::{
    ENGINES_V1, MUX_V1, ORCHESTRATORS_V1, is_v1_engine, is_v1_mux, is_v1_orchestrator,
};
pub use spec::{AgentSpec, OrchestratorSpec};
pub use state_dir::{StateLayout, StateLayoutError};
pub use status::{AgentStatus, Findings, Phase};

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
