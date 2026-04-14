---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Hook Sidecar + IPC

## Scope
Two inter-process surfaces:
1. **`ark-hook` sidecar binary** — invoked by Claude Code when a hook event fires, reads JSON from stdin, writes structured AgentEvent-shaped record to the agent's state dir and pipes to the status plugin.
2. **Host control socket** — unix socket at `$XDG_RUNTIME_DIR/ark/control.sock` listening for commands from the picker plugin (spawn, kill, rename, resurrect). One daemon-less listener per supervisor OR a central lightweight coordinator.

## Requirements — ark-hook sidecar

### R1: `ark-hook` binary
**Description:** Small binary invoked by Claude Code hooks.
**Acceptance Criteria:**
- [ ] Signature: `ark-hook --id <AgentId> --event <EVENT_NAME>`
- [ ] Reads a single JSON document from stdin (Claude Code's hook payload)
- [ ] Parses `{session_id, cwd, hook_event_name, tool_name?, tool_input?, ...}`
- [ ] Translates into one or more AgentEvent variants (e.g., `PostToolUse → ToolUse + FileEdited`)
- [ ] Writes the parsed events as JSON lines to `$STATE/agents/{id}/hooks/{event}.jsonl` (per-event jsonl file for simplicity and debuggability)
- [ ] Also forwards the events via `zellij pipe --name ark-status -- <json>` for the status plugin
- [ ] Also forwards to `ark-picker` pipe target
- [ ] For `PermissionRequest` with auto-approve policy: writes `{"hookSpecificOutput": {"decision": {"behavior": "allow"}}}` to stdout
- [ ] Exit code 0 on success (Claude allows), exit code 2 only for explicit deny decisions
- [ ] Running time budget < 200ms (Claude blocks its main loop on hook scripts)
**Dependencies:** cavekit-types-state-events, cavekit-engine-claude-code

### R2: State file writes
**Description:** Durable record of hook-derived events.
**Acceptance Criteria:**
- [ ] Writes under `$STATE/agents/{id}/hooks/`:
  - `PostToolUse.jsonl` — each line an AgentEvent::ToolUse derived record
  - `Stop.jsonl` — single Done trigger per line
  - `PermissionRequest.jsonl` — every permission ask + resolution
  - `Notification.jsonl` — claude's notification events
  - `SessionEnd.jsonl` — session termination markers
  - `TaskCompleted.jsonl` — claude task-tool completions
- [ ] Append-only; O_APPEND + O_CREAT semantics to allow concurrent writes from multiple hooks
- [ ] ClaudeCodeEngine's tailers also read these (same stream consumed by engine task)
**Dependencies:** cavekit-types-state-events

### R3: Errors and missing state dir
**Description:** Graceful degradation.
**Acceptance Criteria:**
- [ ] If `$STATE/agents/{id}/` does not exist, hook logs to stderr and exits 0 (never block claude)
- [ ] If pipe to zellij fails (no plugin, no session): log stderr, continue
- [ ] If stdin is empty or malformed JSON: log stderr, exit 0 with `{"decision": {"behavior": "allow"}}` on PermissionRequest (fail-open)
- [ ] On any unhandled error, never exit 2 (avoids blocking claude)
**Dependencies:** R1

## Requirements — Control socket

### R4: Control socket daemon
**Description:** Accept administrative commands from the picker plugin (or other clients like the CLI).
**Acceptance Criteria:**
- [ ] Path: `$XDG_RUNTIME_DIR/ark/control.sock` (unix stream socket, mode 0700)
- [ ] Lifecycle options (pick one at impl time; leaning toward option A):
  - **A) Shared lightweight listener** started on first `ark spawn`, exits when no agents active. Runs in a forked process separate from any supervisor.
  - B) Each supervisor listens on its own per-agent socket — more sockets, more complex dispatch.
- [ ] Protocol: newline-delimited JSON requests + responses
- [ ] Permissions: socket file is user-only (0700)
- [ ] Errors: malformed requests get `{"ok": false, "error": "..."}`; connection remains open
**Dependencies:** cavekit-plugin-picker, cavekit-supervisor

### R5: Control protocol
**Description:** Commands the socket accepts.
**Acceptance Criteria:**
- [ ] Request shape: `{"cmd": "<name>", "args": {...}}`, response: `{"ok": true, "data": ...}` or `{"ok": false, "error": "..."}`
- [ ] Commands:
  - `List { }` — returns array of AgentStatus for all known agents (active + recent done, reading state dir)
  - `Spawn { orchestrator, engine, cwd, name, layout, cmd, env? }` — forks supervisor (invokes `ark spawn` internally or reuses shared spawn logic)
  - `Kill { id, remove_worktree?: bool }` — SIGTERM supervisor, with optional worktree removal
  - `ForceKill { id }` — SIGKILL supervisor
  - `Rename { id, new_name }` — rewrites `spec.json.name` (not session name, which is frozen)
  - `Resurrect { id }` — reads crashed agent's spec, runs `Spawn` with same params
  - `Forget { id }` — sets `status.json.hide = true` so picker omits it
  - `Ping` — `{"ok": true, "data": "pong"}`
- [ ] Authorization: local-only (unix socket + user perms); no tokens
- [ ] Audit log: every command appended to `$STATE/control.log`
**Dependencies:** R4, cavekit-supervisor

## Example claude hook config (injected by engine)
```json
{
  "hooks": {
    "PostToolUse": [
      {"command": "ark-hook --id cavekit-myfeat-01JX... --event PostToolUse"}
    ],
    "Stop": [
      {"command": "ark-hook --id cavekit-myfeat-01JX... --event Stop"}
    ],
    "PermissionRequest": [
      {"command": "ark-hook --id cavekit-myfeat-01JX... --event PermissionRequest"}
    ]
  }
}
```

## Example control protocol exchange
```
C: {"cmd":"List","args":{}}
S: {"ok":true,"data":[{...AgentStatus}, ...]}

C: {"cmd":"Kill","args":{"id":"cavekit-myfeat-01JX...","remove_worktree":false}}
S: {"ok":true,"data":{"signaled":"SIGTERM"}}
```

## Out of Scope
- Remote / TCP control socket — local unix only
- Authentication / authorization — local user is trusted
- Event streaming over control socket — picker uses zellij pipe; control socket is request/response
- Hooks for non-claude engines — each engine declares its own hook surface

## Cross-References
- cavekit-engine-claude-code.md — hook injection reads this spec for the sidecar
- cavekit-plugin-picker.md — primary consumer of control socket
- cavekit-supervisor.md — kill/spawn/etc routed through control socket by picker
- cavekit-cli.md — cli can optionally use control socket (but defaults to direct SIGTERM for `ark kill`)
