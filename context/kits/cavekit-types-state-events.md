---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Types, State Directory, Event Bus

## Scope
Foundational data types (AgentId, AgentSpec, AgentEvent, Outcome), the on-disk state directory schema under XDG paths, the in-process event bus wiring (tokio broadcast), and the append-only event log format. Used by every other domain.

## Requirements

### R1: Identifiers
**Description:** Every run has a deterministic, human-friendly ID used in state paths, session names, log references.
**Acceptance Criteria:**
- [ ] `AgentId` is a `String` with format `{orchestrator}-{name}-{ulid}` (e.g., `cavekit-auth-01JX7Z8K6X9Y2ZT4...`)
- [ ] Helper `AgentId::new(orchestrator, name) -> AgentId` generates ULID suffix
- [ ] `AgentId::session_name()` returns zellij-safe session name: `ark-{orchestrator}-{name}` (ULID suffix dropped for brevity; if collision, append `-{short-ulid}`)
- [ ] `AgentId::state_dir(base: &Path)` returns `$STATE/agents/{id}/`
- [ ] IDs are valid filesystem components and URL-safe
**Dependencies:** none

### R2: AgentSpec
**Description:** The immutable input to a spawn — what the user asked for, serialized to `spec.json` at spawn time.
**Acceptance Criteria:**
- [ ] `AgentSpec` is serde Serialize + Deserialize + Clone
- [ ] Fields:
  - `id: AgentId`
  - `name: String` (human label; visible in picker)
  - `orchestrator: String` (slug, e.g., `cavekit`)
  - `engine: String` (slug, e.g., `claude-code`)
  - `cwd: PathBuf` (worktree path)
  - `cmd: Vec<String>` (primary agent pane command)
  - `env: BTreeMap<String, String>`
  - `layout: Option<String>` (KDL stem; None = orchestrator's choice)
  - `session: String` (zellij session name, derived but persisted)
  - `created_at: DateTime<Utc>`
  - `runner_config: serde_json::Value` (orchestrator-specific, validated by orchestrator)
- [ ] `OrchestratorSpec` = `AgentSpec` (single alias; orchestrator's view is identical for v1)
- [ ] Written once to `$STATE/agents/{id}/spec.json` at spawn; never modified
**Dependencies:** R1

### R3: AgentEvent enum
**Description:** Every observable event during a run, serde-serializable for events.jsonl and zellij pipe payloads.
**Acceptance Criteria:**
- [ ] `#[non_exhaustive]` + `#[serde(tag = "kind", rename_all = "snake_case")]`
- [ ] Variants:
  - `Started { spec: AgentSpec }`
  - `TabOpened { id: AgentId, parent: Option<AgentId>, role: TabRole, tab_handle: TabHandle, label: String }`
  - `TabClosed { id: AgentId, tab_handle: TabHandle }`
  - `Progress { id: AgentId, done: u32, total: u32, label: Option<String> }`
  - `TaskDone { id: AgentId, task_id: String, label: Option<String> }`
  - `Iteration { id: AgentId, n: u32, max: Option<u32> }`
  - `PhaseTransition { id: AgentId, from: Option<String>, to: String }`
  - `ToolUse { id: AgentId, tool: String, input_summary: String }`
  - `Message { id: AgentId, role: MessageRole, summary: String }`
  - `FileEdited { id: AgentId, path: PathBuf, additions: u32, deletions: u32 }`
  - `ReviewComment { id: AgentId, reviewer: AgentId, severity: Severity, path: PathBuf, line: Option<u32>, body: String }`
  - `PermissionAsked { id: AgentId, tool: String, summary: String }`
  - `PermissionResolved { id: AgentId, tool: String, decision: PermissionDecision }`
  - `Stall { id: AgentId, since: DateTime<Utc> }`
  - `Log { id: AgentId, level: LogLevel, line: String }`
  - `Error { id: AgentId, message: String }`
  - `Done { id: AgentId, outcome: Outcome }`
- [ ] `TabRole` = `Builder | Subagent | Reviewer | Log | Custom(String)`
- [ ] `Outcome` = `Success { artifacts: Vec<PathBuf> } | Failed { reason: String } | Killed | Timeout | Crashed { reason: String }`
- [ ] `Severity` = `P0 | P1 | P2 | P3`
- [ ] `MessageRole` = `User | Assistant | System | Tool`
- [ ] `PermissionDecision` = `Allowed | Denied | Deferred`
- [ ] `LogLevel` = `Trace | Debug | Info | Warn | Error`
**Dependencies:** R2

### R4: EventSink + EventBus
**Description:** In-process pub-sub used by supervisor, engine, orchestrator.
**Acceptance Criteria:**
- [ ] `EventSink` = `tokio::sync::broadcast::Sender<AgentEvent>`
- [ ] Channel capacity: 256 (documented; override via config)
- [ ] Lagging subscribers drop oldest, log a warning, do not panic
- [ ] Supervisor owns the Sender; hands clones to engine, orchestrator, state writer, status piper
- [ ] Every event written to `events.jsonl` (by state writer task) and forwarded to zellij pipe (by status piper task)
**Dependencies:** R3

### R5: State directory schema
**Description:** On-disk layout under `$XDG_STATE_HOME/ark/`.
**Acceptance Criteria:**
- [ ] Base: `$XDG_STATE_HOME/ark/` (fallback `~/.local/state/ark/`)
- [ ] Per-agent directory: `$STATE/agents/{id}/` containing:
  - `spec.json` — frozen AgentSpec
  - `status.json` — current AgentStatus (atomic replace via temp-file + rename)
  - `events.jsonl` — append-only event log
  - `pid` — supervisor PID (plain integer in first line)
  - `supervisor.log` — tracing output
  - `hooks/` — directory for engine-specific hook artifacts (per-hook JSONL files for claude-code)
  - `artifacts/` — optional outputs captured by orchestrator
- [ ] Archive directory: `$STATE/archive/{YYYY-MM-DD}/{id}/` — `ark doctor --fix` moves completed/crashed agents here
- [ ] Locks directory: `$STATE/locks/{id}.lock` — file lock prevents double-spawn collisions
- [ ] Runtime dir: `$XDG_RUNTIME_DIR/ark/` containing:
  - `control.sock` — unix socket for picker→host commands (v1)
- [ ] Config dir: `$XDG_CONFIG_HOME/ark/` (covered in cavekit-config.md)
- [ ] Directory creation is idempotent; supervisor creates whatever is missing
- [ ] Permissions: agent dirs user-only (0700), sockets user-only
**Dependencies:** R1

### R6: AgentStatus
**Description:** Snapshot rolled up from the event stream, pushed to `status.json` after every event.
**Acceptance Criteria:**
- [ ] Fields:
  - `spec: AgentSpec` (duplicated for self-contained reads)
  - `phase: Phase`
  - `progress: Option<(u32, u32)>`
  - `last_event_at: DateTime<Utc>`
  - `last_event_summary: String`
  - `tab_handles: Vec<TabHandle>` (every open tab in this run)
  - `supervisor_pid: u32`
  - `stalled_since: Option<DateTime<Utc>>`
  - `findings: Findings` (review rollup: counts per severity)
- [ ] `Phase` = `Starting | Running | Idle | Prompting | Reviewing | Done | Failed | Crashed`
- [ ] Written atomically: write to `status.json.tmp`, `rename` to `status.json`
- [ ] Readable by any consumer without locking (rename is atomic)
- [ ] Phase transitions emit a `PhaseTransition` event as well
**Dependencies:** R3, R5

### R7: events.jsonl format
**Description:** Append-only log of AgentEvent values as newline-delimited JSON.
**Acceptance Criteria:**
- [ ] One JSON object per line, serde_json default encoding
- [ ] Each line: `{"ts": "2026-04-14T12:34:56.789Z", "event": { <AgentEvent> }}`
- [ ] Writer task buffers via tokio channel, flushes per event (no batching — agents are low-volume <100 events/sec)
- [ ] Rotating archival: none in v1. Single file grows until agent done, then archived with the run.
- [ ] Corruption recovery: malformed lines are skipped by readers with a warning; next write continues at end of file
- [ ] Readers can tail via `tokio::fs::File` + inotify or equivalent; `ark pane log` uses this pattern
**Dependencies:** R3, R5

## Out of Scope
- Remote state (cloud, SQLite, database) — all local flatfile
- Schema migrations between versions — handled by `ark doctor` heuristically; breaking changes bump major version
- Encryption at rest — not handled; state dir is user-only
- Event replay timing fidelity — v1 stores ts only, no monotonic clock for strict ordering

## Cross-References
- cavekit-architecture.md — consumes these types throughout traits
- cavekit-supervisor.md — owns EventBus, writes state
- cavekit-cli.md — `ark list` reads status.json, `ark pane log` tails events.jsonl
- cavekit-engine-claude-code.md — producers of Tool/Permission/Message events
- cavekit-orchestrator-cavekit.md — producers of Progress/Iteration/Phase events
