---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Implementation Tracking: architecture

Build site: context/plans/build-site.md

| Task | Status | Notes |
|------|--------|-------|
| T-013 | DONE | Engine trait (async_trait, Send+Sync+'static): name/install_observability/teardown/default_pane_cmd/transcript_path/auto_approve_permissions. ApprovalPolicy enum (Ask/AutoApproveRead/AutoApproveAll). EngineHandle wraps Box<dyn Any+Send+Sync> with downcast<S>(). crates/ark-core/src/engine.rs |
| T-014 | DONE | Orchestrator trait (async_trait, Send+Sync+'static): name/engine/detect/run → anyhow::Result<Outcome>. Stub World replaced in T-015. crates/ark-core/src/orchestrator.rs |
| T-015 | DONE | World struct: spec, mux: Arc<dyn Multiplexer>, events: EventSink, cancel: CancellationToken, hooks_dir: PathBuf, state: Arc<StateLayout>, config: Arc<Config>. World::new() ctor. config::Config placeholder until T-018. |
| T-016 | DONE | Multiplexer trait (async_trait, Send+Sync): kind/ensure_session/create_tab/close_tab/rename_tab/pipe. tmux-compatible shape. crates/ark-core/src/multiplexer.rs |
| T-017 | DONE | Tracked under impl-types-state-events.md (lives in ark-types/src/scope.rs) |
