---
title: "Event Bus"
description: "AgentEvent and UserEvent routing"
---

Every observable thing that happens during an ark run is an event. The event bus is a `tokio::sync::broadcast` channel owned by the supervisor, shared with the engine, orchestrator, and consumer tasks. This page covers the event types, the broadcast channel mechanics, the EventSink API, and how events flow from producers to consumers.

## Event routing overview

```d2
direction: right

producers: "Producers" {
  engine: "Engine\n(hook payloads,\ntranscript changes)"
  orchestrator: "Orchestrator\n(phase transitions,\nprogress, iterations)"
  supervisor: "Supervisor\n(Started, Done)"
  hook: "ark-hook\n(external events\nvia control socket)"
}

bus: "EventSink\n(broadcast channel\ncapacity: 256)" {
  shape: cylinder
}

consumers: "Consumers" {
  state-writer: "state_writer\n→ events.jsonl\n→ status.json"
  status-pipe: "status_pipe\n→ ark-status plugin\n→ ark-picker plugin"
  reaction: "reaction_dispatcher\n→ scene reactions"
}

producers.engine -> bus
producers.orchestrator -> bus
producers.supervisor -> bus
producers.hook -> bus

bus -> consumers.state-writer
bus -> consumers.status-pipe
bus -> consumers.reaction
```

## AgentEvent

`AgentEvent` is the core event enum. It is `#[non_exhaustive]` and serialized with `#[serde(tag = "kind", rename_all = "snake_case")]` for the JSONL log and zellij pipe payloads.

### Variants

| Variant | Fields | Emitted by |
|---|---|---|
| `Started` | `spec: AgentSpec` | Supervisor |
| `TabOpened` | `id, parent, role, tab_handle, label` | Orchestrator |
| `TabClosed` | `id, tab_handle` | Orchestrator |
| `Progress` | `id, done, total, label` | Orchestrator |
| `TaskDone` | `id, task_id, label` | Orchestrator |
| `Iteration` | `id, n, max` | Orchestrator |
| `PhaseTransition` | `id, from, to` | Supervisor / Orchestrator |
| `ToolUse` | `id, tool, input_summary` | Engine |
| `Message` | `id, role, summary` | Engine |
| `FileEdited` | `id, path, additions, deletions` | Engine |
| `ReviewComment` | `id, reviewer, severity, path, line, body` | Orchestrator |
| `PermissionAsked` | `id, tool, summary` | Engine |
| `PermissionResolved` | `id, tool, decision` | Engine / Supervisor |
| `Stall` | `id, since` | Supervisor |
| `Log` | `id, level, line` | Any |
| `Error` | `id, message` | Any |
| `Done` | `id, outcome` | Supervisor |
| `UserEvent` | `event, payload, source` | Any (via `ark-hook emit` or scene reaction) |

`AgentEvent` is `#[non_exhaustive]`: match arms must include a wildcard to handle future variants.

### Supporting types

```rust
pub enum TabRole {
    Builder,
    Subagent,
    Reviewer,
    Log,
    Custom(String),
}

pub enum Outcome {
    Success { artifacts: Vec<PathBuf> },
    Failed { reason: String },
    Killed,
    Timeout,
    Crashed { reason: String },
}

pub enum Severity { P0, P1, P2, P3 }

pub enum MessageRole { User, Assistant, System, Tool }

pub enum PermissionDecision { Allowed, Denied, Deferred }

pub enum LogLevel { Trace, Debug, Info, Warn, Error }
```

All types derive `Serialize`, `Deserialize`, and `Clone`.

## UserEvent

Scene reactions and extensions produce `UserEvent` values that also flow through the bus. A `UserEvent` wraps a string event name, a payload map, and a source attribution:

```rust
pub struct UserEvent {
    pub event: String,        // e.g., "ark.acp.turn_start"
    pub payload: serde_json::Map<String, Value>,
    pub source: String,       // "core", "ext:myext", "plugin:picker", "scene"
}
```

Source attribution follows a canonical convention:
- `core` -- emitted by ark's own supervisor
- `ext:<name>` -- emitted by an extension subprocess
- `plugin:<name>` -- emitted by a wasm plugin
- `hook:<name>` -- emitted by a hook command
- `scene` -- emitted by a scene reaction

`UserEvent` values enter the bus via the `Emit` command on the control socket (sent by `ark-hook emit`), allowing external processes to inject events.

## EventSink

`EventSink` is a type alias:

```rust
pub type EventSink = tokio::sync::broadcast::Sender<AgentEvent>;
```

The supervisor creates the channel and hands `EventSink` clones to every producer and consumer:

```rust
let (tx, _rx) = broadcast::channel::<AgentEvent>(capacity);

// Producers get tx.clone()
engine.install_observability(&spec.cwd, tx.clone()).await?;

// Consumers subscribe
let state_rx = tx.subscribe();
let status_rx = tx.subscribe();
let reaction_rx = tx.subscribe();

spawn(state_writer(state_rx, state.clone()));
spawn(status_pipe(status_rx, mux.clone()));
spawn(reaction_dispatcher(reaction_rx, scene.clone()));
```

### Channel capacity

Default capacity is 256 events, overridable via `config.defaults.event_bus_capacity`. Agents are low-volume (< 100 events/sec), so 256 provides ample headroom.

### Lag handling

When a subscriber falls behind, `tokio::sync::broadcast` returns `RecvError::Lagged(n)`. Consumers handle this by:

1. Logging a warning with the number of dropped events.
2. Continuing to receive from the current position.
3. Never panicking.

This is safe because the primary durable record is `events.jsonl` (written by `state_writer`). If `status_pipe` lags, the plugin misses a transient update but catches the next one. If `reaction_dispatcher` lags, dropped events simply do not trigger their reactions -- an acceptable degradation.

## Consumer tasks

### state_writer

Receives every event and performs two writes:

1. **events.jsonl** -- appends one JSON line per event with a timestamp wrapper:
   ```json
   {"ts": "2026-04-14T12:34:56.789Z", "event": {"kind": "tool_use", "id": "...", "tool": "Edit", "input_summary": "..."}}
   ```
   Flushed per event (no batching). Malformed lines from prior crashes are skipped by readers.

2. **status.json** -- updates the `AgentStatus` snapshot atomically (write to `status.json.tmp`, rename to `status.json`). Any process can read this without locking because rename is atomic.

### status_pipe

Forwards progress-relevant events to the zellij plugins:

- `mux.pipe("ark-status", json)` -- status bar plugin renders phase, progress, and agent name.
- `mux.pipe("ark-picker", json)` -- picker plugin updates its agent list incrementally.

Pipe failures are non-fatal. If a plugin is missing, the supervisor degrades to tab-rename for progress display.

### reaction_dispatcher

Evaluates scene reactions against incoming events. Each reaction specifies a filter (event kind + optional field match) and an action (run a command, emit another event, trigger an intent). Reactions run on detached tasks with a 30-second timeout so a misbehaving reaction cannot block the bus.

## Event lifecycle

A complete event lifecycle from engine to user:

1. Claude Code fires a `PostToolUse` hook.
2. `ark-hook` receives the JSON on stdin, translates it to `AgentEvent::ToolUse`, writes to `hooks/PostToolUse.jsonl`, and pipes to `ark-status`.
3. The engine's hook-file tailer picks up the new line, parses it, and sends `ToolUse` on the `EventSink`.
4. `state_writer` appends the event to `events.jsonl` and updates `status.json`.
5. `status_pipe` forwards the event to both zellij plugins.
6. `reaction_dispatcher` checks if any scene reaction matches `tool_use` events and fires matching actions.
7. The status bar plugin re-renders with the new tool use info.
8. The picker plugin updates its agent card.

## events.jsonl format

The append-only log uses newline-delimited JSON:

- One JSON object per line, serde_json default encoding.
- Each line: `{"ts": "<ISO8601>", "event": { <AgentEvent> }}`.
- No batching -- each event is flushed immediately.
- No rotation in v1. The file grows until the agent completes, then is archived with the run.
- Corruption recovery: malformed lines are skipped by readers with a warning.
- Readers can tail via `tokio::fs::File` + platform file-watch (inotify/kqueue). `ark pane log` uses this pattern.

## AgentStatus

The rolled-up status snapshot written to `status.json`:

```rust
pub struct AgentStatus {
    pub spec: AgentSpec,
    pub phase: Phase,
    pub progress: Option<(u32, u32)>,
    pub last_event_at: DateTime<Utc>,
    pub last_event_summary: String,
    pub tab_handles: Vec<TabHandle>,
    pub supervisor_pid: u32,
    pub stalled_since: Option<DateTime<Utc>>,
    pub findings: Findings,
}

pub enum Phase {
    Starting,
    Running,
    Idle,
    Prompting,
    Reviewing,
    Done,
    Failed,
    Crashed,
    Killed,
    Timeout,
}
```

`AgentStatus` is self-contained -- it duplicates the `AgentSpec` so readers do not need to open a second file. `Findings` holds a review rollup with counts per severity level.
