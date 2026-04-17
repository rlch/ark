---
title: "Hook IPC"
description: "ark-hook binary and control socket"
---

Ark has two inter-process communication surfaces: the `ark-hook` sidecar binary (invoked by agent CLIs on hook events) and the per-supervisor control socket (used by the picker plugin and CLI for management commands). This page covers both.

## ark-hook sidecar

`ark-hook` is a small binary shipped alongside `ark`. It serves as the bridge between agent CLIs (which fire hooks as subprocess calls) and ark's event system.

### Subcommands

`ark-hook` has four subcommands, each optimized for a different caller:

#### Default (hook-event, legacy)

```
ark-hook --id <AgentId> --event <EVENT_NAME>
```

Invoked by Claude Code when a hook event fires. Reads a single JSON document from stdin (Claude Code's hook payload), translates it into one or more `AgentEvent` variants, and writes the results to the state directory.

The translation from Claude Code hooks to ark events:

| Claude Hook | AgentEvent(s) |
|---|---|
| `PostToolUse` | `ToolUse` + `FileEdited` (if applicable) |
| `Stop` | `Done` trigger |
| `PermissionRequest` | `PermissionAsked` |
| `Notification` | `Log` |
| `SessionEnd` | `Done` |
| `TaskCompleted` | `TaskDone` |

For `PermissionRequest` with an auto-approve policy, `ark-hook` writes `{"hookSpecificOutput": {"decision": {"behavior": "allow"}}}` to stdout. Exit code is always 0 on success (never blocks Claude). Exit code 2 is used only for explicit deny decisions.

**Time budget: < 200ms.** Claude blocks its main loop on hook scripts.

#### intent

```
ark-hook intent --id <AgentId> --json '{"intent": "...", "args": {...}}'
```

Connects to the agent's control socket and sends an `Intent` command. Used by the `ark-bus` plugin to dispatch named intents (keybinds, zellij events forwarded through the scene intent registry).

**Time budget: < 50ms** (keybind UX).

#### emit

```
ark-hook emit --id <AgentId> --json '{"event": "...", "payload": {...}, "source": "..."}'
```

Connects to the control socket and sends an `Emit` command. Used by `ark-bus` for forwarding zellij events (`CommandPaneOpened`, `CommandPaneExited`, `PaneClosed`, `FileSystemUpdate`) onto ark's event bus.

#### permit

```
ark-hook permit --id <AgentId> --request-id <str> --outcome <allow|reject_once|reject_always> [--option-id <str>]
```

Connects to the control socket and sends a `Permit` command. Used by picker plugin modals to respond to outstanding permission requests.

### ID resolution

When `--id` is omitted, `ark-hook` reads `ARK_AGENT_ID` from the environment. The supervisor sets this env var in all spawned child processes, including zellij.

### State file writes

`ark-hook` writes to `$STATE/agents/{id}/hooks/`:

| File | Content |
|---|---|
| `PostToolUse.jsonl` | `AgentEvent::ToolUse` derived records |
| `Stop.jsonl` | Done trigger markers |
| `PermissionRequest.jsonl` | Permission ask + resolution pairs |
| `Notification.jsonl` | Claude notification events |
| `SessionEnd.jsonl` | Session termination markers |
| `TaskCompleted.jsonl` | Task-tool completions |

All files are append-only (`O_APPEND | O_CREAT`), allowing concurrent writes from multiple hook invocations. The engine's tailer tasks also read these files -- same stream, consumed from both sides.

### Error handling

`ark-hook` follows a strict fail-open policy:

- If `$STATE/agents/{id}/` does not exist: log to stderr, exit 0.
- If pipe to zellij fails (no plugin, no session): log to stderr, continue.
- If stdin is empty or malformed JSON: log to stderr, exit 0 with an allow decision on `PermissionRequest`.
- On any unhandled error: never exit 2 (avoids blocking Claude).

### Hook injection

The engine installs hook configuration into the agent's settings before launch. For Claude Code, this means writing to `.claude/settings.local.json`:

```json
{
  "hooks": {
    "PostToolUse": [
      { "command": "ark-hook --id cavekit-auth-01JX7Z... --event PostToolUse" }
    ],
    "Stop": [
      { "command": "ark-hook --id cavekit-auth-01JX7Z... --event Stop" }
    ],
    "PermissionRequest": [
      { "command": "ark-hook --id cavekit-auth-01JX7Z... --event PermissionRequest" }
    ]
  }
}
```

This injection is idempotent. The engine backs up the existing settings and deep-merges the hook configuration.

## Control socket

Each supervisor binds its own unix socket. There is no central daemon. This follows the kakoune model: one socket per session, enumeration by directory scan, dead sockets GC'd by reachability check.

### Socket path

The socket lives at `<runtime_root>/agents/{id}.sock`. Runtime root resolves in order:

1. `$ARK_RUNTIME_DIR` (verbatim)
2. `$XDG_RUNTIME_DIR/ark-$UID` (Linux with systemd)
3. `$TMPDIR/ark` (macOS -- `$TMPDIR` is already per-user, e.g., `/var/folders/.../T/`)
4. `/tmp/ark-$UID` (bare Linux last resort)

Parent directory mode is 0700. Socket file mode is 0700. The runtime directory is auto-created on first use by CLI entry points.

**Path length cap:** macOS unix sockets cap at ~104 bytes. Typical paths land at ~60-80 characters, well under the limit.

### Lifecycle

The socket binds in step 3 of the supervisor startup sequence -- immediately after StateDir + lock acquisition, before any slow engine work. This means the picker can reach the agent as soon as `ark` returns.

Socket cleanup:

| Exit path | Cleanup mechanism |
|---|---|
| Normal exit | `reclaim_name(true)` Drop guard on the `interprocess` listener |
| SIGTERM / SIGINT | `signal_hook` handler explicitly `unlink()`s the socket path |
| Panic with `panic = "abort"` | `signal_hook` SIGABRT handler covers this |
| SIGKILL / hard crash | Socket remains stale; GC'd by next picker/CLI scan |

Stale socket GC: any client that finds a `.sock` file for which `connect()` returns `ECONNREFUSED` (50ms timeout) unlinks it. This is the kakoune `kak -l` pattern.

### Implementation

The socket uses the `interprocess` crate (`local_socket::tokio::Listener`):

```rust
use interprocess::local_socket::{
    tokio::{prelude::*, Listener, Stream},
    ListenerOptions,
};

let listener = ListenerOptions::new()
    .name(socket_path)
    .mode(0o600)
    .try_overwrite(true)
    .reclaim_name(true)
    .create_tokio()?;
```

Connections serve on the supervisor's tokio runtime as `JoinSet` children (one task per connection). The protocol is newline-delimited JSON.

### Control protocol

**Request format:**
```json
{"cmd": "<name>", "args": {...}}
```

**Response format:**
```json
{"ok": true, "data": ...}
```
or
```json
{"ok": false, "error": "..."}
```

### Commands

Commands sent to a supervisor's own socket:

| Command | Args | Response |
|---|---|---|
| `Ping` | none | `{"ok": true, "data": "pong"}` |
| `Status` | none | Full `AgentStatus` snapshot for this agent |
| `Kill` | `remove_worktree?: bool` | SIGTERM self, optional worktree removal |
| `ForceKill` | none | SIGKILL to own process group |
| `Rename` | `new_name: str` | Rewrites `spec.json.name` |
| `Forget` | none | Sets `status.json.hide = true` |
| `ReloadScene` | none | Triggers scene reload (same turn-inflight gates apply) |
| `Intent` | `name: str, args: map` | Dispatches through scene intent registry |
| `Emit` | `event: str, payload: map, source: str` | Broadcasts `UserEvent` onto event bus |
| `Permit` | `request_id, outcome, option_id?` | Responds to outstanding permission request |

### Operations outside the socket

**List** -- The picker scans `$STATE/agents/*/status.json` and reachability-checks `<runtime_root>/agents/*.sock` directly. No central aggregator exists. Launching a new session is a CLI-only operation (`ark [--scene <name>]`); no agent-id exists until the supervisor is ready.

### Example session

Enumerate agents (no socket needed):

```
$ ls $XDG_RUNTIME_DIR/ark-501/agents/
cavekit-auth-01JX7Z8K6X.sock
cavekit-pay-01JY3R2M5N.sock
```

Connect to a specific agent:

```
$ socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ark-501/agents/cavekit-auth-01JX7Z8K6X.sock
> {"cmd":"Ping","args":{}}
< {"ok":true,"data":"pong"}

> {"cmd":"Status","args":{}}
< {"ok":true,"data":{"phase":"Running","progress":[3,8],...}}

> {"cmd":"Kill","args":{"remove_worktree":false}}
< {"ok":true,"data":{"signaled":"SIGTERM"}}
```

### Authorization

Local-only. Unix socket file permissions restrict access to the current user. No tokens, no TLS, no authentication beyond file mode.

Every command is appended to `$STATE/control.log` for audit.
