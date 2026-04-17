//! Orchestrator trait — abstract interface over a methodology driving an engine.
//!
//! Implements cavekit-soul-phase-1-supervisor.md R8. The orchestrator owns
//! whatever long-lived task the extension-managed session needs (scene
//! reactions, ralph loops, watchers). It receives a `&SessionSpec` and
//! returns `Result<(), anyhow::Error>`.
//!
//! `World` is the capability bag handed to `run`: a shared mux handle, a
//! cloneable event sink, a cancellation token, and references to the
//! on-disk state layout / hooks dir / config.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ark_mux_zellij::ZellijMux;
use ark_types::{CancellationToken, EventSink, SessionSpec, StateLayout};
use async_trait::async_trait;

use crate::config::Config;

/// Capabilities passed to `Orchestrator::run`.
///
/// See cavekit-soul-phase-1-supervisor.md R8. `mux` is
/// `Arc<ZellijMux>` concrete. Consumers call inherent methods on
/// `ZellijMux` directly; tests use `ZellijMux::for_test(...)` to inject
/// a scripted `StubExecutor`.
#[non_exhaustive]
pub struct World {
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
    pub fn new(
        mux: Arc<ZellijMux>,
        events: EventSink,
        cancel: CancellationToken,
        hooks_dir: PathBuf,
        state: Arc<StateLayout>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            mux,
            events,
            cancel,
            hooks_dir,
            state,
            config,
        }
    }
}

/// Abstract orchestrator interface.
///
/// See cavekit-soul-phase-1-supervisor.md R8. Under soul phase 1 the
/// trait surface is deliberately narrow: no `engine()` slug, no
/// `Outcome`. `run` takes `&SessionSpec` (so the session's identity +
/// spawn-time config is visible) plus the shared `World` bag.
/// Methodology-specific outcome semantics re-home inside the extension
/// surface.
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    /// Stable slug identifying this orchestrator (e.g. `"cavekit"`).
    fn name(&self) -> &'static str;

    /// Cheap check: does `cwd` look like something this orchestrator can drive?
    fn detect(&self, cwd: &Path) -> bool;

    /// Long-running drive function. Returns once the orchestrator decides
    /// the session is terminal (typically by broadcasting
    /// `CoreEvent::SessionEnded` on `world.events` and returning) or when
    /// `world.cancel` fires.
    async fn run(&self, spec: &SessionSpec, world: World) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{CoreEvent, SessionId};
    use std::collections::BTreeMap;

    fn sample_spec() -> SessionSpec {
        SessionSpec {
            id: SessionId::new("auth"),
            name: "auth".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp/worktree"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: BTreeMap::new(),
        }
    }

    fn make_world() -> World {
        let (mux, _stub) = ZellijMux::for_test(Vec::new());
        let mux = Arc::new(mux);
        let (events, _rx) = tokio::sync::broadcast::channel::<CoreEvent>(8);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        World::new(mux, events, cancel, hooks_dir, state, config)
    }

    struct MockOrchestrator;

    #[async_trait]
    impl Orchestrator for MockOrchestrator {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn detect(&self, _cwd: &Path) -> bool {
            true
        }

        async fn run(&self, _spec: &SessionSpec, _world: World) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn mock_orchestrator_trait_object() {
        let orch: Box<dyn Orchestrator> = Box::new(MockOrchestrator);
        assert_eq!(orch.name(), "mock");
        assert!(orch.detect(Path::new("/anywhere")));

        let spec = sample_spec();
        let world = make_world();
        orch.run(&spec, world).await.expect("ok");
    }

    #[test]
    fn world_new_populates_all_fields() {
        let world = make_world();
        assert_eq!(world.mux.kind(), "zellij");
        assert_eq!(world.hooks_dir, PathBuf::from("/tmp/hooks"));
        assert_eq!(world.state.base(), Path::new("/tmp/state"));
        assert!(!world.cancel.is_cancelled());
        let _events_clone = world.events.clone();
        assert!(Arc::strong_count(&world.config) >= 1);
    }

    #[tokio::test]
    async fn world_cancel_token_propagates() {
        let world = make_world();
        let cancel = world.cancel.clone();
        assert!(!cancel.is_cancelled());
        world.cancel.cancel();
        assert!(cancel.is_cancelled());
    }
}
