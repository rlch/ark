---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Supervisor + Lifecycle

## Scope
The ephemeral per-agent supervisor process. Forked from the `ark spawn` CLI call, detaches from the controlling terminal, runs a tokio runtime owning the event bus, engine handle, orchestrator future, mux reference, and state directory writer. Responsible for fork/detach, crash resilience, kill semantics, and auto-close behavior.

## Requirements

### R1: Fork + detach
**Description:** `ark spawn` forks a supervisor child that survives the parent's exit.
**Acceptance Criteria:**
- [ ] Use the `nix` crate for platform-correct POSIX fork
- [ ] Double-fork + `setsid` to detach from controlling terminal (classic daemon pattern)
- [ ] Supervisor's stdin/stdout/stderr redirected to `$STATE/agents/{id}/supervisor.log` (append mode, tracing-subscriber formatted)
- [ ] Parent `ark spawn` returns promptly (< 1s typical) with the supervisor's PID in stdout
- [ ] `--no-detach` variant keeps supervisor as a child of parent, stays in foreground, streams events to parent's stderr
**Dependencies:** cavekit-cli

### R2: Event bus wiring
**Description:** Supervisor constructs the broadcast channel and attaches consumer tasks.
**Acceptance Criteria:**
- [ ] Uses `tokio::sync::broadcast::channel(capacity)`, capacity from `config.defaults.event_bus_capacity` (default 256)
- [ ] Consumer tasks spawned in the runtime:
  - `state_writer(rx)` — writes every event to `events.jsonl`, updates `status.json` atomically
  - `status_pipe(rx)` — forwards progress-relevant events to `mux.pipe("ark-status", json)` and to `mux.pipe("ark-picker", json)`
  - `hook_dispatcher(rx)` — fires configured `[[hooks]]` cmds on matching events
- [ ] All consumers are resilient to channel lag (drop-oldest + warn-log; never panic)
- [ ] Consumer tasks are JoinSet children and cancel on supervisor shutdown
**Dependencies:** cavekit-types-state-events

### R3: Orchestration sequence
**Description:** The precise order of operations from fork to done.
**Acceptance Criteria:**
- [ ] After detach, supervisor:
  1. Creates `StateDir` (writes `spec.json`, initial `status.json { phase: Starting }`)
  2. Acquires exclusive file lock `$STATE/locks/{id}.lock`
  3. Sets up logging (tracing → `supervisor.log`)
  4. Loads config (figment-layered per cavekit-config)
  5. Instantiates Engine, Orchestrator, Mux via a factory keyed on `spec.engine` and `spec.orchestrator`
  6. Calls `mux.ensure_session(spec.session).await?`
  7. Calls `engine.preflight(spec).await?`
  8. Spawns consumer tasks (state_writer, status_pipe, hook_dispatcher)
  9. Calls `engine.install_observability(cwd, tx.clone()).await?` → stores EngineHandle
  10. Emits `Started { spec }`
  11. Calls `orchestrator.run(spec, world).await` — long-running
  12. On return: awaits all consumer tasks to drain the final events
  13. `engine.teardown(handle).await`
  14. `state.finalize(&outcome)` — writes final status.json, moves agent dir to archive if configured
  15. Releases lock, exits with outcome-derived exit code
**Dependencies:** R2, cavekit-architecture

### R4: Kill semantics
**Description:** Handle SIGTERM and `ark kill` gracefully.
**Acceptance Criteria:**
- [ ] Supervisor registers a SIGTERM handler that:
  - Fires `world.cancel`
  - Waits up to 10s for orchestrator.run to return
  - If orchestrator stalls, sends `Kill` event, tears down engine, closes tabs via mux, exits with `Outcome::Killed`
- [ ] SIGKILL escapes the above — parent data loss minimized by event_writer's per-event flush
- [ ] `ark kill {id}` sends SIGTERM to the PID in `$STATE/agents/{id}/pid`
- [ ] `ark kill {id} --force` sends SIGKILL; `ark doctor --fix` later cleans orphans
- [ ] Kill cascades: if orchestrator opened child tabs (review, subagents), supervisor closes them all in `world.cancel` handling
**Dependencies:** R3, cavekit-cli

### R5: Crash recovery
**Description:** Detect and handle dead supervisors gracefully.
**Acceptance Criteria:**
- [ ] On crash (panic, OOM, laptop sleep): partial `events.jsonl` + stale `status.json` + `pid` pointing to a non-existent process
- [ ] `ark list` checks PID liveness via `kill(pid, 0)` (nix); marks `Crashed` phase in displayed status if pid dead
- [ ] `ark doctor` offers to archive crashed agents: move state dir to `$STATE/archive/{date}/{id}/`, remove lock
- [ ] No automatic restart — crash is user-surfaced, not retried
- [ ] If the agent was inside a live zellij session and the session still exists: `ark doctor` asks whether to close it
**Dependencies:** R1, cavekit-cli

### R6: Auto-close behavior
**Description:** Tabs/sessions close based on config and outcome.
**Acceptance Criteria:**
- [ ] On `Done { outcome: Success }`: if `config.defaults.auto_close_on_done`, close orchestrator's tabs via mux; if no tabs remain in session, session dies naturally
- [ ] On `Done { outcome: Failed | Crashed }`: if `config.defaults.auto_close_on_fail` (default false), close; otherwise leave tabs for user review
- [ ] On `Done { outcome: Killed }`: if `config.defaults.auto_close_on_kill` (default true), close
- [ ] Closing is per-orchestrator-tab, not session-level — leaves session intact if user manually opened other tabs in it
- [ ] Final `status.json` reflects `phase: Done|Failed|Crashed|Killed` regardless of close behavior
**Dependencies:** cavekit-config, cavekit-mux-zellij

## Error isolation
- Supervisor crashes do NOT affect other running supervisors — each is its own process
- A misbehaving hook cmd does not block the event bus; it runs on a detached task with a 30s timeout
- Engine teardown failures are logged but do not prevent supervisor exit

## Out of Scope
- Restart on crash — deferred to v2 (opt-in `auto_restart` policy)
- Pause/resume agents without killing — deferred (users can detach zellij session, same effect)
- Resource limits (CPU/memory quotas) — out of scope, covered by OS
- Signals beyond SIGTERM/SIGINT/SIGKILL — no SIGUSR* handlers v1

## Cross-References
- cavekit-architecture.md R5 — ownership rules
- cavekit-types-state-events.md R5 — state dir schema
- cavekit-cli.md R4 — `ark kill`
- cavekit-mux-zellij.md — tab close calls
- cavekit-engine-claude-code.md — engine preflight / install / teardown
