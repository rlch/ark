---
title: "Supervisor"
description: "18-step lifecycle and ready-signal protocol"
---

Every `ark` launch creates exactly one supervisor process. It forks from the CLI, detaches from the terminal, and runs a tokio runtime that owns the event bus, engine handle, orchestrator future, mux reference, state directory, and control socket. This page covers the full startup sequence, the ready-signal protocol, cancellation semantics, and outcome types.

The supervisor is implemented across 23 modules inside `ark-core`: startup, ready_signal, state_dir, lock, socket, logging, config_loader, scene_resolver, scene_compiler, layout_writer, engine_factory, session_factory, preflight, consumer, plugin_registry, observability, event_emitter, orchestrator_runner, drain, teardown, finalizer, socket_cleanup, and lock_release.

## Fork and detach

The supervisor uses the classic POSIX daemon pattern:

1. The `ark` CLI calls `nix::unistd::fork()`.
2. The child calls `setsid()` to become a session leader.
3. The child forks again (double-fork). The grandchild is the supervisor.
4. The intermediate child exits immediately.
5. The supervisor redirects stdin/stdout/stderr to `$STATE/agents/{id}/supervisor.log` (append mode, formatted by `tracing-subscriber`).

The parent `ark` returns promptly (< 1 second typical) with the supervisor's PID on stdout.

## Startup sequence

After detaching, the supervisor executes these steps in order:

```d2
direction: down

s1: "1. Create StateDir" {
  tooltip: "spec.json + status.json { phase: Starting }"
}
s2: "2. Acquire file lock"
s3: "3. Bind control socket"
s4: "4. Set up logging"
s5: "5. Load config"
s6: "6. Resolve scene path"
s7: "7. Compile scene"
s8: "8. Write rendered layout"
s9: "9. Resolve engine"
s10: "10. Create zellij session"
s11: "11. Engine preflight"
s12: "12. Spawn consumer tasks"
s13: "13. Register plugins"
s14: "14. Install observability"
s15: "15. Emit Started"
s16: "16. Signal readiness"
s17: "17. Run orchestrator"
s18: "18. Shutdown sequence"

s1 -> s2 -> s3 -> s4 -> s5 -> s6 -> s7 -> s8
s8 -> s9 -> s10 -> s11 -> s12 -> s13 -> s14
s14 -> s15 -> s16 -> s17 -> s18
```

### Steps in detail

**Step 1 -- Create StateDir.** Writes `spec.json` (frozen `AgentSpec`) and an initial `status.json` with `phase: Starting` to `$XDG_STATE_HOME/ark/agents/{id}/`.

**Step 2 -- Acquire file lock.** Takes an exclusive `flock` on `$STATE/locks/{id}.lock` to prevent double-spawn collisions.

**Step 3 -- Bind control socket.** Creates a unix socket at `<runtime_root>/agents/{id}.sock` with mode 0700. This happens early -- before any slow engine work -- so the picker can reach the agent as soon as `ark` returns. See [Hook IPC](/architecture/hook-ipc/) for protocol details.

**Step 4 -- Set up logging.** Configures `tracing-subscriber` to write to `supervisor.log` in the agent's state directory.

**Step 5 -- Load config.** Reads the figment-layered configuration: `config.toml` at `$XDG_CONFIG_HOME/ark/`, with defaults, user overrides, project overrides, env vars, and CLI flags.

**Step 6 -- Resolve scene path.** Precedence: `--scene` flag (name or path), `ARK_SCENE` env, `./.ark/scene.kdl`, `$XDG_CONFIG_HOME/ark/scenes/default.kdl`, built-in default.

**Step 7 -- Compile scene.** Parse the KDL scene file, resolve extensions, merge fragments, validate (Rhai compile, template check, intent registry). A compile error aborts the spawn with a miette diagnostic.

**Step 8 -- Write rendered layout.** The compiled scene produces a zellij KDL layout, written to `$XDG_RUNTIME_DIR/ark/layouts/{id}-scene.kdl`. If the scene has keybinds or zellij-event subscribers, ark injects an `ark-bus` plugin mount.

**Step 9 -- Resolve engine.** v0.1 resolution chain: scene-activated ACP extension (`use "ark:acp"`), `config.toml` `engines.<name>`, hardcoded `claude --acp`. Currently only `claude-code` is accepted.

**Step 10 -- Create zellij session.** Calls `mux.ensure_session(spec.session)`. See [Mux](/architecture/mux/) for the session-per-run model.

**Step 11 -- Engine preflight.** Calls `engine.preflight(spec)` which validates the engine binary exists, checks version requirements, and prepares hook infrastructure.

**Step 12 -- Spawn consumer tasks.** Three consumer tasks join the supervisor's `JoinSet`:
- `state_writer` -- writes every event to `events.jsonl`, updates `status.json` atomically.
- `status_pipe` -- forwards progress events to `mux.pipe("ark-status", json)` and `mux.pipe("ark-picker", json)`.
- `reaction_dispatcher` -- fires scene reactions on matching events.

All consumers are resilient to channel lag (drop-oldest + warn-log, never panic).

**Step 13 -- Register plugins.** Always-on plugins from the scene manifest are launched via `launch-or-focus-plugin`. Summon and event-mount plugins register as dormant subscribers.

**Step 14 -- Install observability.** Calls `engine.install_observability(cwd, tx.clone())` which injects hook scripts into the agent's configuration and starts transcript tailers. Returns an opaque `EngineHandle`.

**Step 15 -- Emit Started.** Sends `AgentEvent::Started { spec }` on the event bus.

**Step 16 -- Signal readiness.** The parent CLI process unblocks and prints the agent-id to stdout. From the user's perspective, launch is complete.

**Step 17 -- Run orchestrator.** Calls `orchestrator.run(spec, world).await`. This is the long-running phase.

**Step 18 -- Shutdown sequence.** On return from `run`:
1. Drain all consumer tasks (wait for in-flight reactions).
2. Tear down the engine (`engine.teardown(handle)`).
3. Shut down extension subprocesses.
4. Finalize state (`state.finalize(&outcome)` writes final `status.json`).
5. Unlink the control socket.
6. Release the file lock.
7. Exit with an outcome-derived exit code.

## Ready-signal protocol

The parent `ark` process needs to know when the supervisor is ready to accept commands. The protocol uses an internal pipe:

1. Before forking, the CLI creates an anonymous pipe.
2. The supervisor inherits the write end.
3. After step 16 (all preflight complete, agent-id known), the supervisor writes the agent-id as a UTF-8 line to the pipe and closes it.
4. The parent reads the pipe. On success, it prints the agent-id and exits 0. On pipe close without data (supervisor crashed), it exits non-zero.

This avoids polling. The parent blocks on a single `read()` call.

## State machine

The supervisor tracks agent phase as a simple state machine:

```d2
Starting: "Starting" {
  shape: oval
}
Running: "Running"
Idle: "Idle"
Prompting: "Prompting"
Reviewing: "Reviewing"
Done: "Done" {
  shape: oval
  style.double-border: true
}
Failed: "Failed" {
  shape: oval
  style.double-border: true
}
Crashed: "Crashed" {
  shape: oval
  style.double-border: true
}
Killed: "Killed" {
  shape: oval
  style.double-border: true
}

Starting -> Running: "preflight complete"
Running -> Idle: "agent waiting"
Idle -> Running: "agent active"
Running -> Prompting: "permission asked"
Prompting -> Running: "permission resolved"
Running -> Reviewing: "review phase"
Reviewing -> Running: "review complete"
Running -> Done: "success"
Running -> Failed: "error"
Running -> Killed: "SIGTERM / ark kill"
Starting -> Failed: "preflight error"
Starting -> Crashed: "panic / OOM"
Running -> Crashed: "panic / OOM"
```

Phase transitions emit a `PhaseTransition` event and update `status.json` atomically.

## Kill semantics

The supervisor registers a SIGTERM handler that:

1. Fires `world.cancel` (the `CancellationToken` shared with the orchestrator).
2. Waits up to 10 seconds for `orchestrator.run` to return.
3. If the orchestrator stalls, sends a `Kill` event, tears down the engine, closes all tabs via mux, and exits with `Outcome::Killed`.

External kill paths:

| Command | Behavior |
|---|---|
| `ark kill {id}` | Sends SIGTERM to the PID in `$STATE/agents/{id}/pid` |
| `ark kill {id} --force` | Sends SIGKILL; `ark doctor --fix` cleans orphans later |
| SIGKILL | Data loss minimized by `state_writer`'s per-event flush |

Kill cascades: if the orchestrator opened child tabs (review, subagents), the supervisor closes them all during `world.cancel` handling.

## Crash recovery

When a supervisor crashes (panic, OOM, laptop sleep), it leaves behind:
- Partial `events.jsonl`
- Stale `status.json`
- A `pid` file pointing to a non-existent process

Detection and recovery:

- `ark list` checks PID liveness via `kill(pid, 0)` (nix crate). Dead PIDs display as `Crashed`.
- `ark doctor --fix` archives crashed agents: moves the state dir to `$STATE/archive/{date}/{id}/`, removes the lock file.
- If the agent was in a live zellij session that still exists, `ark doctor` asks whether to close it.
- No automatic restart. Crashes are surfaced to the user.

## Auto-close behavior

When the orchestrator finishes, the supervisor decides whether to close tabs based on outcome and config:

| Outcome | Config key | Default | Behavior |
|---|---|---|---|
| `Success` | `defaults.auto_close_on_done` | `true` | Close orchestrator's tabs |
| `Failed` / `Crashed` | `defaults.auto_close_on_fail` | `false` | Leave tabs for review |
| `Killed` | `defaults.auto_close_on_kill` | `true` | Close tabs |

Closing is per-orchestrator-tab, not session-level. If the user manually opened other tabs in the session, those are left intact. The zellij session dies naturally when no tabs remain.

## Outcome types

The `Outcome` enum represents how a run terminates:

```rust
pub enum Outcome {
    Success { artifacts: Vec<PathBuf> },
    Failed { reason: String },
    Killed,
    Timeout,
    Crashed { reason: String },
}
```

The outcome drives:
- Exit code of the supervisor process
- Auto-close behavior
- Final phase in `status.json`
- The `Done` event payload on the event bus

## Error isolation

- Supervisor crashes do not affect other running supervisors -- each is its own process.
- A misbehaving hook command runs on a detached task with a 30-second timeout. It cannot block the event bus.
- Engine teardown failures are logged but do not prevent supervisor exit.
- Stale control sockets after a crash do not block recovery -- the picker GCs them on next scan.
