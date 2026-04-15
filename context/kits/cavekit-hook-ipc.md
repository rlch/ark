---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Hook Sidecar + IPC

## Scope
Two inter-process surfaces:
1. **`ark-hook` sidecar binary** — invoked by Claude Code when a hook event fires, reads JSON from stdin, writes structured AgentEvent-shaped record to the agent's state dir and pipes to the status plugin.
2. **Per-supervisor control sockets** — each supervisor binds a unix socket at `<runtime_root>/agents/{agent-id}.sock` where `runtime_root` resolves per R4 (ARK_RUNTIME_DIR → XDG_RUNTIME_DIR/ark-$UID → $TMPDIR/ark → /tmp/ark-$UID). Picker enumerates the directory and connects to per-agent sockets. No central daemon. Modeled on kakoune's session-per-socket pattern (`kak -s`/`kak -l`/`kak -p`). New-agent spawn does NOT use the socket — picker `exec`s `ark spawn` as a subprocess (wezterm "connect-or-spawn" pattern coarsened).

## Requirements — ark-hook sidecar

### R1: `ark-hook` binary
**Description:** Small binary invoked by Claude Code hooks AND by the ark-bus plugin bridge (scene R5/R6 keybind + event dispatch).
**Acceptance Criteria:**
- [ ] **Hook-event subcommand (legacy, Claude-Code engine):** `ark-hook --id <AgentId> --event <EVENT_NAME>`
  - Reads a single JSON document from stdin (Claude Code's hook payload)
  - Parses `{session_id, cwd, hook_event_name, tool_name?, tool_input?, ...}`
  - Translates into one or more AgentEvent variants (e.g., `PostToolUse → ToolUse + FileEdited`)
  - Writes the parsed events as JSON lines to `$STATE/agents/{id}/hooks/{event}.jsonl`
  - Also forwards the events via `zellij pipe --name ark-status -- <json>` for the status plugin + `ark-picker` pipe target
  - For `PermissionRequest` with auto-approve policy: writes `{"hookSpecificOutput": {"decision": {"behavior": "allow"}}}` to stdout
  - Exit code 0 on success (Claude allows), exit code 2 only for explicit deny decisions
  - Running time budget < 200ms (Claude blocks its main loop on hook scripts)
- [ ] **Scene dispatch subcommand:** `ark-hook intent --id <AgentId> --json '<{intent, args}>'`
  - Connects to the agent's control socket (`${XDG_RUNTIME_DIR}/ark-$UID/agents/{id}.sock`).
  - Sends `Intent { name, args }` command per R5.
  - Reads response, exits 0 on `{ok: true}`, 1 otherwise; stderr carries error text for zellij hidden-pane-log surfacing.
  - Running time budget < 50ms (keybind UX).
- [ ] **Event-forward subcommand:** `ark-hook emit --id <AgentId> --json '<{event, payload, source}>'`
  - Connects to control socket, sends `Emit { event, payload, source }` per R5.
  - Used by ark-bus for forwarding `CommandPaneOpened`/`CommandPaneExited`/`PaneClosed`/`FileSystemUpdate` zellij events onto ark's event bus.
- [ ] **ACP permit subcommand:** `ark-hook permit --id <AgentId> --request-id <str> --outcome <"allow"|"reject_once"|"reject_always"> [--option-id <str>]`
  - Connects to control socket, sends `Permit { … }` per R5. Used by picker plugin modals.
- [ ] `--id` resolution: when omitted, `ark-hook` reads `ARK_AGENT_ID` from env (set by supervisor in all spawned child processes including zellij).
**Dependencies:** cavekit-types-state-events, cavekit-engine-claude-code, cavekit-scene R5/R6/R7/R17

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

### R4: Per-supervisor control socket (kakoune model)
**Description:** Each supervisor binds its own unix socket. Picker enumerates the agents directory and connects to per-agent sockets. No central daemon, no shared listener, no bind-race. Precedent: kakoune (one socket per `kak -s` session, `kak -l` enumerates by `read_dir`, dead sessions GC'd by reachability check).
**Acceptance Criteria:**
- [ ] Path scheme (option D2, revised 2026-04-15): runtime root resolves in this order — `$ARK_RUNTIME_DIR` (verbatim) → `$XDG_RUNTIME_DIR/ark-$UID` (Linux systemd) → `$TMPDIR/ark` (macOS idiom; `$TMPDIR` is already per-user so no uid disambiguator) → `/tmp/ark-$UID` (bare-Linux last resort). Sockets live at `<runtime_root>/agents/{agent-id}.sock` (flat directory, one socket per supervisor).
- [ ] Parent dir mode 0700; socket file mode 0700
- [ ] **Runtime-dir auto-create (option E):** CLI entry points (`Ctx::from_env`) best-effort create `<runtime_root>` and `<runtime_root>/agents` so `ark doctor --fix` is not required on a fresh install. Creation failures are swallowed; doctor remains the reporting surface for unwritable paths.
- [ ] **macOS:** `XDG_RUNTIME_DIR` is unset by default, but `$TMPDIR` is set to a sandboxed per-user path (`/var/folders/.../T/`). That path is pretty, already exists, and is the macOS equivalent of `XDG_RUNTIME_DIR` — hence no `ark-$UID` suffix in the TMPDIR branch. Precedent: tmux uses `$TMPDIR/tmux-$UID`; ark simplifies to `$TMPDIR/ark`. `dirs::runtime_dir()` returns `None` on macOS; ark MUST handle the TMPDIR fallback explicitly.
- [ ] **Path length cap:** macOS unix sockets cap at ~104 bytes (Linux 108). Typical macOS path `/var/folders/ab/cd/T/ark/agents/cavekit-myfeat-01JX....sock` is ~80 chars. `/tmp/ark-501/agents/…` is ~60 chars. Both well under. If agent-id schema ever grows, validate at bind time.
- [ ] Supervisor binds the socket immediately after `setsid` and StateDir creation, before signaling readiness to the parent CLI (see cavekit-supervisor.md R3 + R7). Listener lifetime = supervisor lifetime; no daemon process exists.
- [ ] Cleanup (graceful): supervisor unlinks its socket via `Drop` guard + `signal_hook` SIGTERM/SIGINT handler. On SIGKILL or hard crash the socket file remains stale until GC.
- [ ] **Stale socket GC:** any client (picker, CLI) that finds a `.sock` for which `connect()` returns `ECONNREFUSED` or `ENOENT`-during-handshake (50ms timeout) MUST `unlink()` it. This is the kakoune `kak -l` pattern.
- [ ] **No file locks needed.** Per-socket scheme avoids bind-race entirely.
- [ ] **`Spawn` is NOT a control-socket command.** Picker spawns new agents by `exec`ing `ark spawn <args>` as a detached subprocess (it then double-forks itself per cavekit-supervisor.md R1). This eliminates the bootstrap dead zone (zero supervisors → no socket → can still spawn the first agent via CLI). See R5 below for the command list.
- [ ] Protocol: newline-delimited JSON requests + responses
- [ ] Errors: malformed requests get `{"ok": false, "error": "..."}`; connection remains open
- [ ] Implementation: `interprocess` crate (`local_socket::Listener` with Tokio integration). Default name-reclamation-on-drop is fine here.
- [ ] Optional belt-and-suspenders: an `fd-lock` flock on `${XDG_RUNTIME_DIR}/ark-$UID/spawn.lock` only around `ark spawn` if deterministic agent-id assignment under concurrent spawns matters; otherwise skip.
**Dependencies:** cavekit-plugin-picker, cavekit-supervisor

### R5: Control protocol
**Description:** Commands the socket accepts.
**Acceptance Criteria:**
- [ ] Request shape: `{"cmd": "<name>", "args": {...}}`, response: `{"ok": true, "data": ...}` or `{"ok": false, "error": "..."}`
- [ ] Each command is sent to the **per-agent socket** for the target agent (picker resolves agent-id → socket path). `List` is the exception: it does not need a socket — picker reads `$STATE/agents/*/status.json` + reachability-checks `${XDG_RUNTIME_DIR}/ark-$UID/agents/*.sock` directly (see cavekit-plugin-picker.md R3).
- [ ] Commands accepted on a supervisor's own socket:
  - `Status { }` — returns this agent's full AgentStatus snapshot (single agent; not aggregate)
  - `Kill { remove_worktree?: bool }` — SIGTERM self, with optional worktree removal
  - `ForceKill { }` — SIGKILL self (supervisor sends SIGKILL to its own process group)
  - `Rename { new_name }` — rewrites `spec.json.name` (session name is frozen)
  - `Forget { }` — sets `status.json.hide = true` so picker omits this agent
  - `Ping` — `{"ok": true, "data": "pong"}`
  - `ReloadScene { }` — triggers `reload_scene` op on the supervisor (per scene R14); same gates (turn-inflight queue, re-entry guard) apply. Response carries `{queued: bool, reason?: str}`.
  - **Scene dispatch commands** (added for the ark-bus plugin bridge per scene R5/R6 + plan T-6.2/T-6.3):
    - `Intent { name: str, args: map }` — dispatches a named intent through the supervisor's scene intent registry. Used by `ark-hook intent` subcommand (ark-bus spawns a hidden command pane running this).
    - `Emit { event: str, payload: map, source: str }` — broadcasts a `UserEvent` onto the supervisor's event bus. `source` MUST be one of the canonical values per scene R4 attribution convention (`core` / `ext:<n>` / `plugin:<n>` / `hook:<n>` / `scene`). Used by `ark-hook emit` subcommand for event forwarding from ark-bus.
    - `Permit { request_id: str, outcome: "allow"|"reject_once"|"reject_always", option_id?: str }` — responds to an outstanding ACP `session/request_permission` (scene R17 permission dispatch). Used by picker plugin modals + scene reactions invoking `acp/permit`.
- [ ] **Out of socket protocol** (handled differently):
  - `Spawn` — picker `exec`s `ark spawn <args>` subprocess (no socket; agent-id doesn't exist yet)
  - `Resurrect` — picker reads crashed agent's `spec.json`, then `exec`s `ark spawn` with same params (semantically equivalent to Spawn)
  - `List` — picker scans state dir + socket dir directly (no central aggregator exists)
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

Picker enumerates agents (no socket needed):
```
$ ls $XDG_RUNTIME_DIR/ark-$UID/agents/
cavekit-myfeat-01JX....sock
cavekit-pay-01JY....sock
```

Picker connects to a specific agent's socket:
```
$ socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ark-$UID/agents/cavekit-myfeat-01JX....sock
C: {"cmd":"Status","args":{}}
S: {"ok":true,"data":{...AgentStatus}}

C: {"cmd":"Kill","args":{"remove_worktree":false}}
S: {"ok":true,"data":{"signaled":"SIGTERM"}}
```

Picker spawns a new agent (NOT via socket):
```
$ ark spawn --orchestrator cavekit --cwd /path -- claude --resume
spawned cavekit-newfeat-01JZ...
```

## Out of Scope
- Remote / TCP control socket — local unix only
- Authentication / authorization — local user is trusted
- Event streaming over control socket — picker uses zellij pipe; control socket is request/response
- Hooks for non-claude engines — each engine declares its own hook surface
- Central daemon / shared listener — explicitly rejected; per-supervisor sockets win on simplicity (no bind-race, no orphan listener, no bootstrap dead zone)
- Cross-agent aggregate commands over socket (e.g. "kill all") — picker iterates per-agent sockets locally

## Cross-References
- cavekit-engine-claude-code.md — hook injection reads this spec for the sidecar
- cavekit-plugin-picker.md — primary consumer of control socket
- cavekit-supervisor.md — kill/spawn/etc routed through control socket by picker
- cavekit-cli.md — cli can optionally use control socket (but defaults to direct SIGTERM for `ark kill`)
