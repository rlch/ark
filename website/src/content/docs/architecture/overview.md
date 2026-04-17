---
title: "Overview"
description: "Crate graph and lifecycle"
---

Ark is a Rust monorepo that orchestrates AI coding agents inside zellij terminal sessions. This page covers the crate dependency graph, the two-layer abstraction model (Engine + Orchestrator), ownership rules, and the lifecycle flow from a bare `ark` invocation to completion.

## Crate graph

The workspace lives under `crates/` with clear dependency boundaries. Binary crates sit at the leaves; shared types sit at the root.

```d2
direction: down

ark-cli: "ark-cli\n(binary)" {
  shape: hexagon
}
ark-hook: "ark-hook\n(binary)" {
  shape: hexagon
}
ark-core: "ark-core\n(supervisor, event bus,\nstate dir, traits)"
ark-mux-zellij: "ark-mux-zellij\n(ZellijMux)"
ark-engines-claude-code: "ark-engines-\nclaude-code"
ark-orchestrators-cavekit: "ark-orchestrators-\ncavekit"
ark-orchestrators-claude-code: "ark-orchestrators-\nclaude-code"
ark-types: "ark-types\n(AgentId, AgentEvent,\nOutcome, EventSink)"
ark-config: "ark-config\n(figment layers)"
ark-scene: "ark-scene\n(scene compiler,\nreload, watcher)"
ark-scene-v2-archive: "ark-scene-v2-archive\n(archived)" {
  style.stroke-dash: 5
}
ark-ext-proto: "ark-ext-proto\n(extension protocol)"
ark-ext-metadata-types: "ark-ext-metadata-types\n(metadata types)"
ark-ext-metadata: "ark-ext-metadata\n(metadata impl)"
ark-ext-derive: "ark-ext-derive\n(proc-macro)" {
  shape: diamond
}
ark-plugin-status: "ark-plugin-status\n(wasm)" {
  shape: diamond
}
ark-plugin-picker: "ark-plugin-picker\n(wasm)" {
  shape: diamond
}
ark-plugin-ark-bus: "ark-plugin-ark-bus\n(wasm)" {
  shape: diamond
}
ark-test-fixtures: "ark-test-fixtures" {
  style.stroke-dash: 3
}

ark-cli -> ark-core
ark-cli -> ark-config
ark-cli -> ark-mux-zellij
ark-cli -> ark-engines-claude-code
ark-cli -> ark-orchestrators-cavekit
ark-cli -> ark-orchestrators-claude-code
ark-cli -> ark-scene

ark-hook -> ark-types

ark-core -> ark-types
ark-core -> ark-config

ark-mux-zellij -> ark-types
ark-engines-claude-code -> ark-core
ark-orchestrators-cavekit -> ark-core
ark-orchestrators-cavekit -> ark-mux-zellij
ark-orchestrators-claude-code -> ark-core
ark-orchestrators-claude-code -> ark-mux-zellij

ark-scene -> ark-types
ark-scene -> ark-config
ark-ext-metadata -> ark-ext-metadata-types
ark-ext-metadata -> ark-ext-proto

ark-test-fixtures -> ark-types
```

**Binary crates:** `ark-cli` compiles to `ark`, `ark-hook` compiles to `ark-hook`. The `ark pane` subcommand routes into `ark-pane` logic inside `ark-cli` (no separate binary).

**Wasm crates:** `ark-plugin-status`, `ark-plugin-picker`, and `ark-plugin-ark-bus` target `wasm32-wasip1`. They are compiled separately and embedded into the `ark` binary via `include_bytes!` in a `build.rs` step. Users extract them with `ark doctor --fix`.

**Extension crates:** `ark-ext-proto` defines the wire protocol for extension subprocesses. `ark-ext-metadata-types` and `ark-ext-metadata` handle wasm artifact metadata. `ark-ext-derive` is a proc-macro crate for deriving extension traits. `ark-scene-v2-archive` is kept for reference only and is not compiled into any shipped binary.

## Two-layer abstraction

Ark splits agent orchestration into two concerns:

| Layer | Responsibility | Examples |
|---|---|---|
| **Engine** | Extracts structured signal from an agent CLI — hooks, transcripts, exit status, permissions. | `ClaudeCodeEngine` |
| **Orchestrator** | Drives a methodology that wraps the engine — phases, iteration, review. Owns all tabs for a run. | `CavekitOrchestrator`, `ClaudeCodeOrchestrator` |

An orchestrator declares which engine it uses. Engines are per-pane (one engine handle per pane), while orchestrators own the full tab graph for a run.

### Engine trait

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

Key properties:

- `install_observability` is idempotent. It injects hooks and transcript tailers before the agent process launches so no early events are lost.
- `EngineHandle` is opaque. The supervisor stores it and passes it back to `teardown` at shutdown.
- A single engine instance can serve multiple panes simultaneously; each pane gets its own handle.

### Orchestrator trait

```rust
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    fn detect(cwd: &Path) -> bool
    where
        Self: Sized;

    fn engine(&self) -> &'static str;

    async fn run(
        &self,
        spec: OrchestratorSpec,
        world: World,
    ) -> Result<Outcome>;
}
```

Key properties:

- `detect` checks whether a given working directory matches this orchestrator's conventions (e.g., CavekitOrchestrator looks for `sites/`, `impl/`, `ralph-loop/`).
- The orchestrator owns its entire tab graph. It calls `world.mux.create_tab` freely to open builder panes, review panes, log panes, or subagent panes.
- The orchestrator does not spawn the primary agent process directly. The agent process launches from the KDL layout materialized by `mux.create_tab`.
- `run` returns when all orchestrator-owned work has terminated.

## World handle

The `World` struct is the capability bundle handed to an orchestrator's `run` method:

```rust
pub struct World {
    pub mux: Arc<ZellijMux>,
    pub events: EventSink,
    pub cancel: CancellationToken,
    pub hooks_dir: PathBuf,
    pub state: Arc<StateDir>,
    pub config: Arc<Config>,
}
```

| Field | Purpose |
|---|---|
| `mux` | Shared reference to the zellij integration. Orchestrator calls `.create_tab()`, `.close_tab()`, `.pipe()` freely. |
| `events` | Cloneable `tokio::sync::broadcast::Sender<AgentEvent>`. Engines, orchestrators, and consumers all share this channel. |
| `cancel` | Fires when the supervisor receives SIGTERM or `ark kill`. Orchestrator must honor it within 5 seconds or risk SIGKILL escalation. |
| `hooks_dir` | Per-agent hooks directory under the state dir, where engine hook artifacts land. |
| `state` | Handle to the on-disk state directory for this run. |
| `config` | Layered configuration (defaults, user, project, env, flags). |

## Ownership rules

One supervisor process per launched session. The supervisor owns everything:

```d2
direction: down

supervisor: "Supervisor Process" {
  engine: "Engine Task"
  orchestrator: "Orchestrator Task"
  event-bus: "Event Bus\n(broadcast channel)"
  consumers: "Consumer Tasks" {
    state-writer: "state_writer"
    status-pipe: "status_pipe"
    reaction-dispatcher: "reaction_dispatcher"
  }
  mux: "Arc<ZellijMux>"
  state-dir: "StateDir"

  engine -> event-bus: "emits events"
  orchestrator -> event-bus: "emits events"
  event-bus -> consumers
  orchestrator -> mux: "create/close tabs"
}

cli: "CLI (ark)" {
  shape: rectangle
  style.stroke-dash: 3
}

cli -> supervisor: "fork + detach"
cli -- state-dir-read: "reads state dir\n(no live refs)" {
  style.stroke-dash: 3
}
```

Rules:

1. The supervisor forks from the CLI invocation, detaches (double-fork + `setsid`), and runs a tokio runtime.
2. Engine and orchestrator run as tokio tasks inside the supervisor process.
3. Subtasks (file watchers, transcript tailers, hook listeners) run as `JoinSet` children of either the engine or orchestrator task.
4. On supervisor exit, all subtasks cancel. `state.finalize` writes the final `status.json`.
5. The CLI client never holds references to running engines or orchestrators. It interacts with the state directory and control socket only.

## Lifecycle flow

The full sequence from launch to completion:

1. **CLI parses** the top-level flags (`--scene`, `--session`) and constructs an `AgentSpec`.
2. **Fork + detach** — supervisor becomes a daemon process.
3. **StateDir created** — writes `spec.json`, initial `status.json { phase: Starting }`.
4. **Control socket bound** — unix socket at `<runtime_root>/agents/{id}.sock`.
5. **Config loaded** — figment-layered from defaults, user, project, env, flags.
6. **Scene resolved** — `--scene` flag, `ARK_SCENE` env, `.ark/scene.kdl`, or built-in default.
7. **Scene compiled** — parse, resolve extensions, validate, render layout KDL.
8. **Engine resolved** — from scene, config, or `--engine` flag.
9. **Zellij session created** — `mux.ensure_session(spec.session)`.
10. **Engine preflight** — installs hooks and observability before agent launch.
11. **Consumer tasks spawned** — state_writer, status_pipe, reaction dispatcher.
12. **Readiness signaled** — parent CLI returns, prints agent-id.
13. **Orchestrator runs** — long-lived `orchestrator.run(spec, world).await`.
14. **Shutdown** — engine teardown, consumer drain, state finalize, socket unlink, exit.

## Scope boundaries

The following are explicitly out of scope for v1:

- Multi-engine per pane (one engine per pane handle)
- Hot-swapping engines mid-run
- Orchestrator-to-orchestrator handoff
- Orchestrator running without an engine
