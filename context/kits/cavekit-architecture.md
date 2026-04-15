---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-15T00:00:00Z"
---

# Spec: Core Architecture

## Scope
Defines the two-layer adapter model that lets ark orchestrate any agent engine via any methodology. Specifies trait surfaces, ownership, and the flow from `ark spawn` to `ark kill`.

## Context

Agents used for AI coding split into two concerns:
- **Engine** — the underlying agent CLI (Claude Code, Aider, Codex). Knows how to extract structured signal (hooks, transcripts, exit status, permissions).
- **Orchestrator** — the methodology wrapping the engine (Cavekit Hunt phases, Ralph Loop iteration, raw passthrough). Knows higher-level workflow semantics.

An orchestrator may use one engine and spawn additional sibling panes (e.g., Cavekit spawns a codex review tab). Engines are per-pane, not per-orchestrator. Both emit into a shared event bus owned by the supervisor.

## Requirements

### R1: Engine trait
**Description:** Abstract interface for extracting structured signal from an underlying agent CLI.
**Acceptance Criteria:**
- [ ] `Engine` trait is `Send + Sync + 'static`, `async_trait`
- [ ] Methods: `name() -> &'static str`, `install_observability(cwd, sink) -> Result<EngineHandle>`, `teardown(handle) -> Result<()>`, `default_pane_cmd() -> Vec<String>`, `transcript_path(cwd) -> Option<PathBuf>`, `auto_approve_permissions(cwd, policy) -> Result<()>`
- [ ] `install_observability` is idempotent; safe to re-run on same cwd
- [ ] Engine is installed before orchestrator.run is called — hooks must be in place when agent launches or first events are lost
- [ ] `EngineHandle` is opaque; supervisor tracks it for teardown
- [ ] Single engine instance can serve multiple panes simultaneously (per-pane handle)
- [ ] Tests: contract suite validates every Engine impl against a common fixture cwd
**Dependencies:** cavekit-types-state-events (EventSink type)

### R2: Orchestrator trait
**Description:** Abstract interface for a methodology that drives an engine and owns a graph of panes.
**Acceptance Criteria:**
- [ ] `Orchestrator` trait is `Send + Sync + 'static`, `async_trait`
- [ ] Methods: `name() -> &'static str`, `detect(cwd) -> bool`, `engine() -> &'static str` (default engine slug), `run(spec, world) -> Result<Outcome>`
- [ ] Orchestrator owns its entire tab graph: builder pane(s), review pane, log pane, etc. Calls `world.mux.create_tab` as many times as needed
- [ ] Orchestrator must not spawn the primary agent process directly — that's supervised by the KDL layout launched by mux.create_tab
- [ ] `run` returns when all orchestrator-owned work has terminated (builder + any spawned review/child panes)
- [ ] Orchestrator receives engine events through the shared event bus; can filter or forward them
- [ ] Tests: contract suite validates every Orchestrator impl against fixture events + mock mux
**Dependencies:** R1, cavekit-types-state-events, cavekit-mux-zellij

### R3: World handle
**Description:** Capabilities handed to an orchestrator's `run` method.
**Acceptance Criteria:**
- [ ] `World` struct with fields: `mux: Arc<ZellijMux>`, `events: EventSink`, `cancel: CancellationToken`, `hooks_dir: PathBuf`, `state: Arc<StateDir>`, `config: Arc<Config>`
- [ ] `mux` is shared; orchestrator calls `.create_tab()`, `.close_tab()`, `.pipe()` freely
- [ ] `events` is a cloneable `tokio::sync::broadcast::Sender<AgentEvent>`
- [ ] `cancel` fires when supervisor receives SIGTERM or `ark kill`
- [ ] `hooks_dir` points to per-agent hooks directory under state dir
- [ ] Orchestrator must honor `cancel` within 5s or risk SIGKILL escalation
**Dependencies:** R1, R2

### R4: Zellij host integration
**Description:** `ZellijMux` — ark's concrete integration with zellij as the terminal multiplexer. No trait abstraction; the type is the API. Ark ships zellij-only; a second mux is not a planned capability.
**Acceptance Criteria:**
- [ ] `ZellijMux` is a concrete `Send + Sync` struct in `ark-mux-zellij`; no `Multiplexer` trait, no dyn dispatch
- [ ] Inherent methods: `kind() -> &'static str` (returns `"zellij"`), `ensure_session(name) -> Result<()>`, `create_tab(session, name, layout_path) -> Result<TabHandle>`, `close_tab(handle) -> Result<()>`, `rename_tab(handle, name) -> Result<()>`, `pipe(target_name, payload) -> Result<()>`
- [ ] Zellij-native capability expansion (floating panes, swap layouts, typed pipe source, pane titles, plugin-permission declarations) is deferred — add inherent methods when a consumer kit motivates them; do not pre-add
- [ ] Tests: `StubExecutor` records command sequences and is asserted in `ark-mux-zellij` unit tests (see `cavekit-testing` R3); no cross-impl contract suite
- [ ] No narrow injection traits for test mockability (no `TabOps`, `PluginPipe`, `StatusChannel`, etc.). Downstream tests use pure-function factoring (return `MuxOp` data) or `ZellijMux` backed by `StubExecutor`. See `cavekit-overview.md` principle 9 and `cavekit-testing.md` R1 for the reasoning.
**Dependencies:** cavekit-types-state-events

### R5: Ownership rules
**Description:** Who owns what at runtime.
**Acceptance Criteria:**
- [ ] One supervisor process per `ark spawn` — owns Engine, Orchestrator, Mux, event bus, state dir writes
- [ ] Supervisor forks from CLI invocation, detaches (double-fork + setsid), runs tokio runtime
- [ ] Engine and orchestrator run as tokio tasks inside the supervisor process
- [ ] Subtasks (file watchers, transcript tailers, hook listeners) run as JoinSet children of either engine or orchestrator task
- [ ] On supervisor exit, all subtasks cancel; state.finalize writes final status.json
- [ ] CLI client never holds references to running engines/orchestrators; interacts with state dir + control socket only
**Dependencies:** cavekit-supervisor

### R6: v1 scope lock
**Description:** Components shipped across the v1 milestone sequence (v0.1 → v1.0 per `plans/build-site-scene.md`).
**Acceptance Criteria:**
- [ ] **Engines (v0.1–v0.2, LEGACY):** `ClaudeCodeEngine` via hook-injection + transcript-tailing (see `cavekit-engine-claude-code.md`).
- [ ] **Engines (v0.3+):** engine abstraction collapses to an ACP launch spec (see `cavekit-scene.md` R17). Shipped specs: `claude`, `codex`, `gemini-cli`. Non-ACP engines (e.g., aider) arrive via adapter extensions (subprocess extension speaking both extension-protocol to ark and ACP to the wrapped tool).
- [ ] Orchestrators: `CavekitOrchestrator`, `ClaudeCodeOrchestrator`.
- [ ] Zellij integration: `ZellijMux` (concrete type, no mux trait).
- [ ] v1 compiles with one binary (`ark`), two wasm plugins (`ark-status.wasm`, `ark-picker.wasm` — shipped inline at v0.1, ported to ark-native extensions at v0.3), one hook sidecar (`ark-hook`, extended with `intent` / `emit` subcommands per scene R5/R6).
- [ ] **Extension system (v0.3):** ark-native extension protocol (JSON-RPC 2.0, three delivery modes — compiled-in, subprocess, wasm-component) per `cavekit-scene.md` R10 + R16. The "v2 subprocess NDJSON protocol" language in earlier drafts is obsolete; extensions ship in v0.3.
- [ ] **ACP client (v0.3):** ark bundles the `agent-client-protocol` crate as a first-class client; engines are ACP agents. See `cavekit-scene.md` R17.
- [ ] `--engine` CLI flag: v0.1–v0.2 accepts only `claude-code`; v0.3+ accepts any configured ACP engine name.
**Dependencies:** cavekit-scene R10, R16, R17

## Reference implementation sketches

### Engine trait (Rust pseudocode)
```rust
#[async_trait]
pub trait Engine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn install_observability(
        &self,
        cwd: &Path,
        sink: EventSink,
    ) -> Result<EngineHandle>;
    async fn teardown(&self, handle: EngineHandle) -> Result<()>;
    fn default_pane_cmd(&self) -> Vec<String>;
    fn transcript_path(&self, cwd: &Path) -> Option<PathBuf>;
    fn auto_approve_permissions(
        &self,
        cwd: &Path,
        policy: ApprovalPolicy,
    ) -> Result<()>;
}
```

### Orchestrator trait
```rust
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn detect(cwd: &Path) -> bool where Self: Sized;
    fn engine(&self) -> &'static str;
    async fn run(
        &self,
        spec: OrchestratorSpec,
        world: World,
    ) -> Result<Outcome>;
}
```

### Supervisor flow
```rust
async fn supervisor(
    spec: OrchestratorSpec,
    engine: Arc<dyn Engine>,
    orchestrator: Arc<dyn Orchestrator>,
    mux: Arc<ZellijMux>,
    config: Arc<Config>,
) {
    let state = StateDir::create(&spec.id)?;
    let (tx, rx) = broadcast::channel(256);
    let cancel = CancellationToken::new();

    // Persistent consumers.
    spawn(state_writer(rx.resubscribe(), state.clone()));
    spawn(status_pipe(rx.resubscribe(), mux.clone()));

    // Preflight.
    engine.install_observability(&spec.cwd, tx.clone()).await?;

    // Hand off to orchestrator.
    let world = World {
        mux: mux.clone(),
        events: tx.clone(),
        cancel: cancel.clone(),
        hooks_dir: state.hooks_dir(),
        state: state.clone(),
        config: config.clone(),
    };
    let outcome = orchestrator.run(spec, world).await?;

    tx.send(AgentEvent::Done { id: spec.id.clone(), outcome: outcome.clone() })?;
    engine.teardown(engine_handle).await?;
    state.finalize(&outcome);
}
```

## Out of Scope
- Multi-engine per pane (one engine per pane handle in v1)
- Hot-swapping engines mid-run
- Orchestrator-to-orchestrator handoff
- Orchestrator running without an engine (pure methodology observer without CLI) — all v1 orchestrators declare an engine

## Cross-References
- cavekit-types-state-events.md — AgentId, AgentEvent, Outcome, EventSink
- cavekit-engine-claude-code.md — R1's primary implementation
- cavekit-orchestrator-cavekit.md — richest R2 implementation
- cavekit-orchestrator-claude-code.md — minimal R2 implementation
- cavekit-mux-zellij.md — R4's implementation
- cavekit-supervisor.md — R5 in detail
