---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-16"
---

# Spec: Engine — Claude Code

> **STATUS (2026-04-16): LEGACY, retires at v0.3.** Per `cavekit-scene.md` R17, ark becomes a first-class Agent Client Protocol (ACP) client; engines become ACP agents with a trivial launch spec (`command` + `args` + `env`). Claude Code speaks ACP natively via `claude --acp`, so the hook-injection + transcript-tailing apparatus documented here (R1–R7) is obsolete once ACP is live. **v0.1 and v0.2 still run this code path** while the scene system is being stood up; v0.3 retires it via `build-site-scene.md` T-ACP.7. The post-R17 replacement is a four-line `engines.claude` entry in `config.toml` + the ACP client in `crates/acp-client/`.
>
> R1–R7 below are preserved for the v0.1/v0.2 engine runtime and as the behavioral contract the ACP replacement must match (permission auto-approve, done-detection, phase transitions). Do NOT add new requirements to this spec; all new coding-agent integration work belongs in `cavekit-scene.md` R17.

## Scope
The `ClaudeCodeEngine` implementation. Installs Claude Code hooks into a worktree's `.claude/settings.local.json` pointing at the `ark-hook` sidecar, tails the session transcript JSONL for richer message-level signal, and enforces a permission auto-approve policy via the `PermissionRequest` hook. Emits AgentEvents to the shared event bus.

## Background (Claude Code surfaces)
Claude Code exposes 20+ hook events via `settings.json` (`SessionStart`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Notification`, `PermissionRequest`, `TaskCreated`, `TaskCompleted`, `Stop`, `SessionEnd`, etc). Each hook passes JSON on stdin; exit code 2 blocks, exit code 0 allows. JSON output can include a `{"decision": {"behavior": "allow"}}` payload on `PermissionRequest` for programmatic auto-approve. Session transcripts are written real-time to `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` in Messages-API-shaped blocks.

## Requirements

### R1: Hook injection
**Description:** Inject hooks into the worktree's `.claude/settings.local.json` pointing to the `ark-hook` sidecar binary.
**Acceptance Criteria:**
- [ ] On `install_observability(cwd, sink)`:
  - Locate or create `{cwd}/.claude/settings.local.json`
  - Preserve any pre-existing settings (deep merge, do not clobber)
  - Inject hook entries for the events listed in `config.engine.claude_code.inject_hooks`
  - Each hook command invokes `ark-hook --id {AgentId} --event {EVENT}` with stdin piped from Claude
- [ ] Backup the original settings.local.json to `{cwd}/.claude/settings.local.json.ark-backup` once; `teardown` restores from backup
- [ ] Idempotent: re-running `install_observability` on the same cwd detects existing ark hooks and does nothing (by checksum)
- [ ] If `.claude/` does not exist, create it (0700)
- [ ] Hook entries are marked with a `# ark` comment so users know they were injected
**Dependencies:** cavekit-hook-ipc (for ark-hook binary), cavekit-config

### R2: Transcript tailing
**Description:** Tail the live session JSONL transcript for richer message/tool_use/tool_result signal than hooks alone provide.
**Acceptance Criteria:**
- [ ] On first hook event received after install (`SessionStart` typically), extract `session_id` from the payload
- [ ] Tail `~/.claude/projects/{encoded-cwd}/{session-id}.jsonl` using `tokio::fs::File` with inotify watch (via `notify` crate)
- [ ] Each line is a JSON object; parse into typed variants (user_message, assistant_message, tool_use, tool_result, thinking, etc.)
- [ ] Emit corresponding AgentEvents:
  - `ToolUse` from tool_use blocks (summarize input to first 80 chars)
  - `Message` from user/assistant messages (summary = first 80 chars)
  - `FileEdited` from tool_use matching Edit/Write/NotebookEdit (with path extracted)
- [ ] Handle mid-stream JSONL rotation (Claude creates new session on `--resume` after SessionEnd) by re-reading `session_id` on next `SessionStart`
- [ ] Tail runs as a JoinSet child inside the engine handle; cancels on `teardown`
- [ ] Only active when `config.engine.claude_code.transcript_tail = true`
**Dependencies:** cavekit-types-state-events

### R3: Permission auto-approve
**Description:** Apply a user-configured policy to Claude's permission prompts via the `PermissionRequest` hook.
**Acceptance Criteria:**
- [ ] Policy is set by `config.engine.claude_code.permission_policy`: one of `ask | auto_approve_read | auto_approve_all`
- [ ] `ask`: hook emits `PermissionAsked` event, does not auto-resolve (Claude prompts user in its TUI)
- [ ] `auto_approve_read`: hook approves if tool is in read-only set (`Read`, `Glob`, `Grep`, `WebFetch`, `WebSearch`); else `PermissionAsked`
- [ ] `auto_approve_all`: hook always approves
- [ ] Approval payload: JSON on stdout `{"hookSpecificOutput": {"decision": {"behavior": "allow"}}}`
- [ ] Every decision emits `PermissionAsked` + `PermissionResolved` events regardless of policy
- [ ] Configurable per-agent via `runner_config.permission_policy` override (future, not v1)
**Dependencies:** cavekit-hook-ipc

### R4: Done detection
**Description:** Use the `Stop` or `SessionEnd` hooks as authoritative signals the agent finished work.
**Acceptance Criteria:**
- [ ] On `Stop` hook: emit `Done { outcome: Success { artifacts: Vec::new() } }` (orchestrator may upgrade with artifacts)
- [ ] On `SessionEnd` hook: emit `Done { outcome: Success { artifacts: Vec::new() } }` if not already emitted
- [ ] Orchestrator can short-circuit a `Done` (e.g., cavekit may delay until review completes) by listening to `Stop`, suppressing propagation, and emitting its own `Done` later — supported via engine emitting to bus only, not direct supervisor signal
- [ ] `Killed` and `Crashed` outcomes are set by supervisor, not engine (engine never sees kill)
**Dependencies:** cavekit-types-state-events

### R5: Stall detection
**Description:** Emit `Stall` when no tool use has occurred for `config.defaults.stall_timeout_secs` (default 120).
**Acceptance Criteria:**
- [ ] Engine task tracks last ToolUse/Message event time
- [ ] Timer task polls every 10s; if last event age > threshold and no Stop emitted: emit `Stall { since }`
- [ ] Stall is a one-shot event per stall interval (de-dup via last emitted Stall timestamp)
- [ ] On resumed activity (new ToolUse/Message), emit a Log at level Info (`resumed after {secs}s stall`)
**Dependencies:** R2, cavekit-config

### R6: EngineHandle
**Description:** Opaque token returned by install_observability and consumed by teardown.
**Acceptance Criteria:**
- [ ] `EngineHandle` is a struct encapsulating:
  - JoinSet of subtasks (transcript tailer, stall watcher, hook dispatcher)
  - Reference to the worktree path for settings.json restoration
  - The ID for routing
- [ ] `teardown(handle)`:
  - cancels all subtasks
  - restores `.claude/settings.local.json` from backup
  - removes `{cwd}/.claude/settings.local.json.ark-backup`
- [ ] Supervisor holds the handle for the duration of the run
**Dependencies:** cavekit-architecture R1

### R7: Preflight
**Description:** Validate the environment before declaring readiness.
**Acceptance Criteria:**
- [ ] `preflight` called by supervisor before install:
  - `claude` binary on PATH
  - `~/.claude/` directory exists
  - write access to `{cwd}/.claude/`
  - `ark-hook` binary discoverable (same dir as `ark` or on PATH)
- [ ] Returns detailed error on failure with remediation hint
**Dependencies:** cavekit-hook-ipc

## Reference: injected settings snippet
```json
{
  "hooks": {
    "PostToolUse":    [{"command": "ark-hook --id cavekit-auth-01JX... --event PostToolUse"}],
    "Stop":           [{"command": "ark-hook --id cavekit-auth-01JX... --event Stop"}],
    "PermissionRequest": [{"command": "ark-hook --id cavekit-auth-01JX... --event PermissionRequest"}],
    "Notification":   [{"command": "ark-hook --id cavekit-auth-01JX... --event Notification"}],
    "TaskCompleted":  [{"command": "ark-hook --id cavekit-auth-01JX... --event TaskCompleted"}],
    "SessionEnd":     [{"command": "ark-hook --id cavekit-auth-01JX... --event SessionEnd"}]
  }
}
```

## Out of Scope
- MCP server observability (MCP servers are tool providers, not passive observers)
- Agent SDK (headless mode) — deferred to v2
- Claude Code's new session format changes — adapted when/if they ship
- Non-Claude engines (AiderEngine, CodexEngine) — deferred

## Cross-References
- cavekit-architecture.md R1 — Engine trait surface
- cavekit-hook-ipc.md — ark-hook binary spec, event dispatch contract
- cavekit-orchestrator-cavekit.md — consumes engine events alongside its own FS watchers
- cavekit-config.md — `[engine.claude_code]` section
