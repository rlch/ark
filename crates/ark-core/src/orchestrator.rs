//! Orchestrator trait — abstract interface over a methodology driving an engine.
//!
//! Implements cavekit-architecture.md R2. The orchestrator owns its tab graph
//! (builder, reviewer, log panes) and drives them to an `Outcome`.
//!
//! The `World` capability bag (R3) is a stub here; T-015 fills it out with
//! `mux`, `events`, `cancel`, `hooks_dir`, `state`, and `config` fields.

use std::path::Path;

use ark_types::{AgentSpec, Outcome};
use async_trait::async_trait;

/// Capabilities passed to `Orchestrator::run`.
///
/// T-015 fills out the rest of R3's fields (mux, events, cancel, hooks_dir,
/// state, config). For now this carries only the spec so trait impls and
/// downstream crates can compile against a stable shape.
#[non_exhaustive]
pub struct World {
    pub spec: AgentSpec,
    // T-015 adds:
    //   pub mux: std::sync::Arc<dyn crate::Multiplexer>,
    //   pub events: crate::EventSink,
    //   pub cancel: tokio_util::sync::CancellationToken,
    //   pub hooks_dir: std::path::PathBuf,
    //   pub state: std::sync::Arc<crate::StateDir>,
    //   pub config: std::sync::Arc<crate::Config>,
}

impl World {
    /// Stub constructor used by tests and (for now) the supervisor until
    /// T-015 lands. Takes only the spec.
    pub fn new(spec: AgentSpec) -> Self {
        Self { spec }
    }
}

/// Abstract orchestrator interface. See cavekit-architecture.md R2.
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    /// Stable slug identifying this orchestrator (e.g. `"cavekit"`).
    fn name(&self) -> &'static str;

    /// Default engine slug this orchestrator pairs with (e.g. `"claude-code"`).
    fn engine(&self) -> &'static str;

    /// Cheap check: does `cwd` look like something this orchestrator can drive?
    fn detect(&self, cwd: &Path) -> bool;

    /// Long-running drive function. Returns once all orchestrator-owned panes
    /// have terminated.
    async fn run(&self, spec: AgentSpec, world: World) -> anyhow::Result<Outcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentId, AgentSpec};
    use std::path::PathBuf;

    fn sample_spec() -> AgentSpec {
        AgentSpec::new(
            AgentId::new("cavekit", "auth"),
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".to_string()],
        )
    }

    struct MockOrchestrator;

    #[async_trait]
    impl Orchestrator for MockOrchestrator {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn engine(&self) -> &'static str {
            "claude-code"
        }

        fn detect(&self, _cwd: &Path) -> bool {
            true
        }

        async fn run(&self, _spec: AgentSpec, _world: World) -> anyhow::Result<Outcome> {
            Ok(Outcome::Success { artifacts: vec![] })
        }
    }

    #[tokio::test]
    async fn mock_orchestrator_trait_object() {
        let orch: Box<dyn Orchestrator> = Box::new(MockOrchestrator);
        assert_eq!(orch.name(), "mock");
        assert_eq!(orch.engine(), "claude-code");
        assert!(orch.detect(Path::new("/anywhere")));

        let spec = sample_spec();
        let world = World::new(spec.clone());
        let outcome = orch.run(spec, world).await.unwrap();
        match outcome {
            Outcome::Success { artifacts } => assert!(artifacts.is_empty()),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn world_stub_carries_spec() {
        let spec = sample_spec();
        let world = World::new(spec.clone());
        assert_eq!(world.spec.id, spec.id);
    }
}
