---
title: Supervisor
description: Ephemeral per-agent process lifecycle
---

The **supervisor** is the process that manages a single agent session. Every `ark` launch creates one supervisor — there is no shared daemon.

## Per-agent, ephemeral

Each supervisor:

- Owns exactly one agent session
- Forks from the `ark` CLI process via double-fork + setsid
- Runs a tokio async runtime
- Manages the zellij session, event bus, extensions, and scene reactions
- Exits when the agent finishes or `ark kill` is called

## Startup lifecycle

The supervisor follows an 18-step startup sequence:

1. Acquire lock file (prevents duplicate supervisors for the same agent)
2. Open control socket (for `ark kill`, `ark scene reload`, etc.)
3. Compile the scene from KDL to the internal AST
4. Render the layout to zellij-compatible KDL
5. Create the zellij session
6. Load extensions declared by `use` statements
7. Register reactions and keybinds
8. Start the agent process
9. Connect the ACP extension to the agent
10. Signal readiness to the parent CLI (via a pipe — the **ready-signal protocol**)
11. Enter the event loop

The parent CLI waits for the readiness signal. If the supervisor fails before step 10, the pipe closes without writing the ACK byte — the CLI surfaces a clean error.

## State directory

Each supervisor writes state to `$XDG_STATE_HOME/ark/<agent-id>/`:

- `status.json` — current agent status (running, done, failed)
- `events.jsonl` — append-only event log
- `supervisor.log` — tracing output (useful for debugging)

`ark list` reads these directories to show running agents. Dead supervisor state is garbage-collected by reachability — if the process is gone and the socket is stale, the entry is cleaned up.

## No daemon

ark deliberately avoids a shared daemon process. Benefits:

- No single point of failure
- No bootstrap dead zone (each supervisor is self-contained)
- No coordination overhead between agents
- Per-agent crash isolation — one supervisor dying never affects another
