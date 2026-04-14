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
  3. **Binds control socket** at `${XDG_RUNTIME_DIR:-/tmp}/ark-$UID/agents/{id}.sock` (creates parent dir 0700 if absent). Listener installed on tokio runtime; serves protocol per cavekit-hook-ipc.md R4 + R5. See R7 below for lifecycle.
  4. Sets up logging (tracing → `supervisor.log`)
  5. Loads config (figment-layered per cavekit-config)
  6. Instantiates Engine, Orchestrator, Mux via a factory keyed on `spec.engine` and `spec.orchestrator`
  7. Calls `mux.ensure_session(spec.session).await?`
  8. Calls `engine.preflight(spec).await?`
  9. Spawns consumer tasks (state_writer, status_pipe, hook_dispatcher)
  10. Calls `engine.install_observability(cwd, tx.clone()).await?` → stores EngineHandle
  11. Emits `Started { spec }`
  12. Signals readiness to parent CLI (parent `ark spawn` returns at this point; agent-id printed to its stdout)
  13. Calls `orchestrator.run(spec, world).await` — long-running
  14. On return: awaits all consumer tasks to drain the final events
  15. `engine.teardown(handle).await`
  16. `state.finalize(&outcome)` — writes final status.json, moves agent dir to archive if configured
  17. **Unlinks control socket** (Drop guard fires; SIGTERM/SIGINT handler covers signal paths — see R7)
  18. Releases lock, exits with outcome-derived exit code
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

### R7: Control socket lifecycle
**Description:** Each supervisor owns its own per-agent unix socket for picker/CLI commands (Kill, Rename, Forget, Status, Ping). No daemon. See cavekit-hook-ipc.md R4 for the full kakoune-model rationale.
**Acceptance Criteria:**
- [ ] Socket bound in step 3 of R3, immediately after StateDir + lock acquisition (before any potentially-slow engine work) so picker can reach the agent as soon as `ark spawn` returns
- [ ] Path: `${XDG_RUNTIME_DIR:-/tmp}/ark-$UID/agents/{id}.sock`. Parent dir mode 0700, socket mode 0700.
- [ ] Crate pin: `interprocess = { version = "2.4", features = ["tokio"] }` (latest 2.x; 8M+ downloads; used by zellij itself, mistral.rs, caligula, ssh-agent-lib)
- [ ] Listener constructed via `ListenerOptions::new().name(name).mode(0o600).try_overwrite(true).reclaim_name(true).create_tokio()?`
  - `try_overwrite(true)`: unlinks stale socket from a crashed prior supervisor on `AddrInUse` (bounded by `.max_spin_time()`)
  - `reclaim_name(true)` (default): Drop guard unlinks the socket file on normal exit
  - `mode(0o600)` (Unix-only via `ListenerOptionsExt`): peer-cred narrowed to current user
- [ ] Types live under `interprocess::local_socket::tokio::{Listener, Stream, RecvHalf, SendHalf}`; **panic if used outside a Tokio runtime context** — bind only inside `#[tokio::main]` body
- [ ] Serves connections on supervisor's tokio runtime as a JoinSet child (one task per connection; NDJSON loop)
- [ ] Each connection: newline-delimited JSON request → JSON response. Connection stays open until peer closes or supervisor shutdown
- [ ] **Cleanup paths:**
  - Normal exit: `reclaim_name(true)` Drop guard on listener calls `unlink()` (R3 step 17)
  - SIGTERM/SIGINT: `signal_hook` handler explicitly `std::fs::remove_file`s socket path before triggering `world.cancel` (R4) — Drop does NOT run on signals
  - Panic with `panic = "abort"`: Drop also skipped — `signal_hook` SIGABRT handler covers this
  - SIGKILL or hard crash: socket file remains stale; GC'd by next picker/CLI scan via reachability check (cavekit-hook-ipc.md R4)
- [ ] No file lock around bind — agent-id uniqueness (ULID per cavekit-types-state-events) prevents collision; if collision somehow occurs, bind fails fast and parent CLI exits with error
- [ ] Bind failure is fatal — supervisor exits with non-zero; parent CLI surfaces error to user
- [ ] Auth: socket file mode 0700 (local user only). No tokens.
**Dependencies:** R3, cavekit-hook-ipc R4

## Error isolation
- Supervisor crashes do NOT affect other running supervisors — each is its own process
- A misbehaving hook cmd does not block the event bus; it runs on a detached task with a 30s timeout
- Engine teardown failures are logged but do not prevent supervisor exit
- Stale control sockets after crash do not block recovery — picker GCs them on next scan

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
