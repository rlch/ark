//! Orchestrator trait — abstract interface over a methodology driving an engine.
//!
//! Implements cavekit-architecture.md R2. The orchestrator owns its tab graph
//! (builder, reviewer, log panes) and drives them to an `Outcome`.
//!
//! `World` (R3) is the capability bag handed to `run`: a shared mux handle, a
//! cloneable event sink, a cancellation token, and references to the on-disk
//! state layout / hooks dir / config.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_mux_zellij::ZellijMux;
use ark_types::{AgentSpec, CancellationToken, EventSink, Outcome, StateLayout};
use async_trait::async_trait;

use crate::config::Config;

/// Capabilities passed to `Orchestrator::run`.
///
/// See cavekit-architecture.md R3. `mux` is `Arc<ZellijMux>` concrete.
/// Consumers call inherent methods on `ZellijMux` directly; tests use
/// `ZellijMux::for_test(...)` to inject a scripted `StubExecutor`.
#[non_exhaustive]
pub struct World {
    pub spec: AgentSpec,
    pub mux: Arc<ZellijMux>,
    pub events: EventSink,
    pub cancel: CancellationToken,
    pub hooks_dir: PathBuf,
    pub state: Arc<StateLayout>,
    pub config: Arc<Config>,
}

impl World {
    /// Construct a fully-populated `World`. All fields are required; there is
    /// no default mux/state/config in the runtime — the supervisor wires them
    /// up before handing off to the orchestrator.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        spec: AgentSpec,
        mux: Arc<ZellijMux>,
        events: EventSink,
        cancel: CancellationToken,
        hooks_dir: PathBuf,
        state: Arc<StateLayout>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            spec,
            mux,
            events,
            cancel,
            hooks_dir,
            state,
            config,
        }
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
    use ark_types::{AgentEvent, AgentId};
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

    fn make_world(spec: AgentSpec) -> World {
        // Empty scripted queue — these assertions only check World wiring,
        // never invoke a mux method.
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let (events, _rx) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        World::new(spec, mux, events, cancel, hooks_dir, state, config)
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
        let world = make_world(spec.clone());
        let outcome = orch.run(spec, world).await.unwrap();
        match outcome {
            Outcome::Success { artifacts } => assert!(artifacts.is_empty()),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn world_new_populates_all_fields() {
        let spec = sample_spec();
        let world = make_world(spec.clone());
        assert_eq!(world.spec.id, spec.id);
        assert_eq!(world.mux.kind(), "zellij");
        assert_eq!(world.hooks_dir, PathBuf::from("/tmp/hooks"));
        assert_eq!(world.state.base(), Path::new("/tmp/state"));
        assert!(!world.cancel.is_cancelled());
        // events is a broadcast::Sender — we can clone it.
        let _events_clone = world.events.clone();
        // config is an Arc<Config>.
        assert!(Arc::strong_count(&world.config) >= 1);
    }

    #[tokio::test]
    async fn world_cancel_token_propagates() {
        let spec = sample_spec();
        let world = make_world(spec);
        let cancel = world.cancel.clone();
        assert!(!cancel.is_cancelled());
        world.cancel.cancel();
        assert!(cancel.is_cancelled());
    }
}
