//! Test [`Multiplexer`] + [`SupervisorSpawner`] impls.
//!
//! Both mocks record every call they receive so tests can assert on
//! the exact sequence of operations the launch pipeline performed.
//! Call logs are behind `Mutex` to match the `&self` contract the
//! traits impose — tests never hit contention in practice because
//! launch is sequential, but the lock keeps the impls `Send + Sync`
//! for free.
//!
//! Scripted return values let tests simulate failure paths (preflight
//! error, supervisor ready timeout, zellij exit code). The default is
//! success.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ark_types::{SessionSpec, StateLayout};

use super::traits::{Multiplexer, SupervisorSpawner};
use crate::error::CliError;

// --------------------------------------------------------- multiplexer ----

/// A call to a [`Multiplexer`] method, captured by [`MockMultiplexer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiplexerCall {
    Preflight,
    IsInside,
    RunSession {
        session: String,
        layout: Option<PathBuf>,
    },
}

/// Scriptable test multiplexer.
///
/// Default behaviour: `preflight` + `run_session` return `Ok`;
/// `is_inside` returns `false`. Use the builder methods to override.
pub struct MockMultiplexer {
    calls: Mutex<Vec<MultiplexerCall>>,
    is_inside: bool,
    preflight_result: Mutex<Option<Result<(), CliError>>>,
    run_session_result: Mutex<Option<Result<(), CliError>>>,
}

impl Default for MockMultiplexer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockMultiplexer {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            is_inside: false,
            preflight_result: Mutex::new(None),
            run_session_result: Mutex::new(None),
        }
    }

    /// Force `is_inside` to return `true` (simulate running inside
    /// an existing zellij session).
    pub fn inside(mut self) -> Self {
        self.is_inside = true;
        self
    }

    /// Script the next `preflight` call to return `Err(err)`.
    pub fn fail_preflight(self, err: CliError) -> Self {
        *self.preflight_result.lock().unwrap() = Some(Err(err));
        self
    }

    /// Script the next `run_session` call to return `Err(err)`.
    pub fn fail_run_session(self, err: CliError) -> Self {
        *self.run_session_result.lock().unwrap() = Some(Err(err));
        self
    }

    /// Snapshot the call log in order. Reads under a lock; the
    /// returned `Vec` is owned.
    pub fn calls(&self) -> Vec<MultiplexerCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Multiplexer for MockMultiplexer {
    fn preflight(&self) -> Result<(), CliError> {
        self.calls.lock().unwrap().push(MultiplexerCall::Preflight);
        self.preflight_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(()))
    }

    fn is_inside(&self) -> bool {
        self.calls.lock().unwrap().push(MultiplexerCall::IsInside);
        self.is_inside
    }

    fn run_session(&self, session: &str, layout: Option<&Path>) -> Result<(), CliError> {
        self.calls
            .lock()
            .unwrap()
            .push(MultiplexerCall::RunSession {
                session: session.to_string(),
                layout: layout.map(PathBuf::from),
            });
        self.run_session_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(()))
    }
}

// ----------------------------------------------------------- supervisor ----

/// A call to a [`SupervisorSpawner`] method, captured by
/// [`InlineSupervisor`]. Records the spec passed in and the state
/// layout's base dir so tests can assert what the CLI would have
/// forked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnCall {
    pub spec: SessionSpec,
    pub state_base: PathBuf,
}

/// In-process supervisor spawner stub.
///
/// Records the `AgentSpec` + `StateLayout` the CLI hands it, then
/// returns `Ok` immediately — simulating a successful ready-ack
/// without forking a real supervisor. Swap to
/// [`InlineSupervisor::new().fail(err)`] to exercise the failure
/// path (supervisor-died, ready-timeout).
///
/// Never forks, never spawns threads, never builds a tokio runtime —
/// safe to call from any test harness.
pub struct InlineSupervisor {
    calls: Mutex<Vec<SpawnCall>>,
    result: Mutex<Option<Result<(), CliError>>>,
}

impl Default for InlineSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineSupervisor {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(None),
        }
    }

    /// Script the next `spawn_and_wait_for_ready` call to return
    /// `Err(err)` — simulates a supervisor that died or timed out
    /// before sending the ready byte.
    pub fn fail(self, err: CliError) -> Self {
        *self.result.lock().unwrap() = Some(Err(err));
        self
    }

    /// Snapshot the recorded spawn calls. Under a lock; returned
    /// `Vec` is owned.
    pub fn calls(&self) -> Vec<SpawnCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl SupervisorSpawner for InlineSupervisor {
    fn spawn_and_wait_for_ready(
        &self,
        spec: SessionSpec,
        state_layout: &StateLayout,
    ) -> Result<(), CliError> {
        self.calls.lock().unwrap().push(SpawnCall {
            spec,
            state_base: state_layout.base().to_path_buf(),
        });
        self.result.lock().unwrap().take().unwrap_or(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_multiplexer_records_calls_in_order() {
        let mux = MockMultiplexer::new();
        mux.preflight().unwrap();
        assert!(!mux.is_inside());
        mux.run_session("work", Some(Path::new("/tmp/layout.kdl")))
            .unwrap();

        assert_eq!(
            mux.calls(),
            vec![
                MultiplexerCall::Preflight,
                MultiplexerCall::IsInside,
                MultiplexerCall::RunSession {
                    session: "work".to_string(),
                    layout: Some(PathBuf::from("/tmp/layout.kdl")),
                },
            ]
        );
    }

    #[test]
    fn mock_multiplexer_inside_flag_honoured() {
        let mux = MockMultiplexer::new().inside();
        assert!(mux.is_inside());
    }

    #[test]
    fn mock_multiplexer_fail_preflight() {
        let mux = MockMultiplexer::new().fail_preflight(CliError::PreflightFail {
            reason: "zellij missing".to_string(),
        });
        let err = mux.preflight().unwrap_err();
        assert!(matches!(err, CliError::PreflightFail { .. }));
    }

    #[test]
    fn mock_multiplexer_run_session_captures_no_layout() {
        let mux = MockMultiplexer::new();
        mux.run_session("sess", None).unwrap();
        assert_eq!(
            mux.calls(),
            vec![MultiplexerCall::RunSession {
                session: "sess".to_string(),
                layout: None,
            }]
        );
    }

    fn sample_spec() -> SessionSpec {
        use ark_types::SessionId;
        use chrono::Utc;
        use std::collections::BTreeMap;
        SessionSpec {
            id: SessionId::new("test"),
            name: "test".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp/cwd"),
            env: BTreeMap::new(),
            created_at: Utc::now(),
            ext_config: BTreeMap::new(),
        }
    }

    #[test]
    fn inline_supervisor_records_spec() {
        let spawner = InlineSupervisor::new();
        let spec = sample_spec();
        let id = spec.id.clone();
        let layout = StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/rt"),
            PathBuf::from("/tmp/cfg"),
        );

        spawner
            .spawn_and_wait_for_ready(spec.clone(), &layout)
            .unwrap();

        let calls = spawner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].spec.id, id);
        assert_eq!(calls[0].state_base, PathBuf::from("/tmp/state"));
    }

    #[test]
    fn inline_supervisor_fail_returns_err() {
        let spawner = InlineSupervisor::new().fail(CliError::Internal {
            reason: "supervisor died before ack".to_string(),
        });
        let spec = sample_spec();
        let layout = StateLayout::new(
            PathBuf::from("/tmp/s"),
            PathBuf::from("/tmp/r"),
            PathBuf::from("/tmp/c"),
        );
        let err = spawner.spawn_and_wait_for_ready(spec, &layout).unwrap_err();
        assert!(matches!(err, CliError::Internal { .. }));
    }
}
