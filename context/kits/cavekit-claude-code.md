---
created: "2026-04-18"
last_edited: "2026-04-18"
status: draft
depends_on:
  - cavekit-soul.md           # Phase 2 ext-hook surface; Phase 4 reframed (cc moves to ext, not delete)
  - cavekit-scene.md          # IntentRegistry, ExtEvent, typed handles, stack
  - cavekit-pi.md             # structural template (view-centric coordination, typed handles) — ported, narrowed
---

# Spec: claude-code — Integrating Claude Code as an ark Extension

## Scope

Specifies the single-crate extension that integrates [Claude Code](https://code.claude.com)
— Anthropic's official CLI coding agent — into ark. One crate in `extensions/`:

| Crate                     | Role                                                                              |
|---------------------------|-----------------------------------------------------------------------------------|
| `extensions/claude-code`  | Hook bridge + `claude-code` CommandView + `claude-code-subagent` stack view + transcript fs-watcher + doctor + list columns. |

No internal split. Claude Code's subagents are first-class in Claude Code; no
separate community package to sandbox out. No runtime tool injection surface
in v0.1 (MCP server deferred — see "Stretch" below); no `-control` sub-crate.

## Motivation

After the 2026-04-18 pivot (handoff: `handoff-2026-04-18-claude-code-first-pivot.md`),
**claude-code is the first engine integration to ship**. It validates the soul
Phase 2 ext-hook surface + the 2026-04-18 typed-handle + stack revision
against ark's actual target user workflow. Pi follows in v0.2 as the second
integration (`cavekit-pi.md` is DEFERRED, preserved for then).

Three properties the extension delivers, none of which require core edits:

1. **Hook-driven observability.** Claude Code's hook system
   (`SessionStart`, `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`,
   `SubagentStart`, `SubagentStop`, `PreCompact`, `Notification`) fires on
   every lifecycle transition. The `cc-hook` binary POSTs payloads to ark's
   per-session socket, which emits them as `claude-code.*` ExtEvents.
   Every hook event is visible to the user's scene via the `FlatEvent`
   shim — `on "claude-code.subagent.stop" { ... }` is a one-liner.

2. **Subagents fan out into a stack via typed handles.** The `claude-code`
   CommandView holds a `Stack<ClaudeCodeSubagent>` handle; on each
   `SubagentStart` hook it spawns a child view into that stack. Zellij
   stack navigation handles focus — collapsed tiles show status via pane
   title; focused tile renders the live transcript tail. No custom log
   pane; no `subagent.focus` intent; no user reactions to fan-out
   subagents.

3. **Passthrough UX.** Claude runs in a normal zellij pane with its own
   TUI handling permissions, input, completion. Ark does not mediate the
   user ↔ claude interaction; it observes and fans out. The scope is
   deliberately narrow — delegate to claude's TUI, watch hooks, show
   subagents.

## Dependencies

Depends on:

- **Soul Phase 1 complete** — `SessionSpec`/`SessionStatus`/`SessionId` +
  shrunk `CoreEvent::Ext(ExtEvent)` + bare `ark` launch green.
- **Soul Phase 2 complete** — the ext-hook surface:
  - `ArkExtension::on_session_start` / `on_session_end` — claude-code uses
    these to bind / tear down the hook IPC socket and reconcile the
    `~/.claude/settings.json` hooks block.
  - `ArkExtension::scene_compile_hook` — claude-code uses this for the
    fallback raw-`command cmd="claude"` path (R5b). Primary path is the
    view's own CommandView impl setting its env natively (R5).
  - `ArkExtension::register_intents` — claude-code contributes (minimal;
    no subagent-focus intent in v0.1).
  - `ArkExtension::doctor_checks` — preflight checks (R10).
  - `ArkExtension::list_columns` — contributes `cc model`, `cc tokens`,
    `cc cost` (R11).
  - **Lifecycle hooks (per `cavekit-plugin-protocol.md` R7, replacing the prior `control_verbs` design):**
    - `on-install(install-event::install | update | host-update | reload)` — idempotent setup. Writes the `~/.claude/settings.json` reconciler entry, copies the `cc-hook` binary into place, and verifies prerequisites. Replaces the prior standalone `ark ext claude-code install-hooks` and `... reinstall-hook-binary` verbs — both folded into this single idempotent hook fired by ark on install/update/host-upgrade/dev-reload.
    - `load()` — per-session subscriptions: open the per-session unix socket, attach the transcript fs-watcher, register intent handlers.
- **Soul Phase 3 complete** — ACP deleted.
- **Soul Phase 4 complete (revised)** — `crates/orchestrators/claude-code/` +
  `crates/hook/` + `crates/types/src/permission.rs` deleted as ark crates;
  salvaged content lives in `extensions/claude-code/` (see "Salvage" at
  bottom).
- **Scene v3 2026-04-18 revision** — typed view-parametric handles +
  `stack` primitive. `subagents: Stack<ClaudeCodeSubagent>` requires this.
- **`CoreEvent::Ext(ExtEvent)`** — carries `ext = "claude-code"` payloads.

## Non-goals

- **Replacing or wrapping Claude Code's TUI.** Claude's TUI stays native
  in its pane; ark observes via hooks and owns no input affordances that
  belong to claude.
- **Permission UI / policy engine.** Claude Code's TUI already renders
  tool-approval prompts. Ark does not intercept. Dropped from v0.1
  (previous draft included `CcPermissionView` + policy engine; cut).
- **Runtime tool injection.** Claude Code has no equivalent to pi's
  `pi.registerTool(...)`. The MCP-server approach (ark exposing
  `IntentRegistry` as MCP tools the user configures via `.mcp.json`) is
  Stretch, not v0.1.
- **Scene-write tool (`ark_scene_propose`-equivalent).** Ships with MCP
  stretch work, not v0.1.
- **Multi-claude coexistence** in one session (parallel models race). Scene
  primitives don't forbid it, but no kit-level guarantees in v0.1.
- **Packaging Claude Code itself.** User installs `claude` themselves
  (npm / brew / official installer). Doctor surfaces absence.
- **Windows support.** Unix-only, matching ark.

---

## Architecture summary

Example scene:

```kdl
scene "claude-dev" {
    use "claude-code"

    layout {
        tab "@main" cwd="{cwd}" focus="true" {
            row {
                pane "@builder" span="3" {
                    claude-code model="sonnet"
                                subagents=@subagents
                }
                stack "@subagents" span="1" { claude-code-subagent }
            }
        }
    }
}
```

Runtime:

```
┌──────────────────── zellij ─────────────────────┐
│ [@builder pane] claude (native TUI)             │
│   └── lifecycle hooks invoke cc-hook binary     │
│                                                  │
│ [@subagents stack] claude-code-subagent × N      │
│   (collapsed: pane title; focused: transcript)   │
└──────────────────────────────────────────────────┘
         │
 POST    │   (cc-hook binary, write-only; no reverse channel)
 NDJSON  ▼
┌─────────── supervisor ────────────────────────────┐
│ claude-code (single crate)                        │
│   ├── IPC socket receives cc-hook POSTs           │
│   ├── transcript fs-watcher (main + subagents/)   │
│   ├── ClaudeCodeView (typed subagents handle)     │
│   ├── ClaudeCodeSubagentView (per-subagent tail)  │
│   ├── settings.json installer + doctor            │
│   └── list columns + control verbs                │
└────────────────────────────────────────────────────┘
```

Direction of data: the view is a client of the layout. It accepts a typed
`&Stack<ClaudeCodeSubagent>` reference the scene declared, and it pushes
children into the stack on `SubagentStart` hook events. It never owns layout
state — the stack lives in the scene; the view reads config and pushes
effects.

**Write-only bridge.** cc-hook POSTs and exits. Ark never writes back.
Claude's TUI handles all interactive UX. This is the single biggest
divergence from pi-core (which has a bidirectional NDJSON socket).

Everything below is structured as `Rn` requirements with testable criteria.

---

## Part A — Hook bridge

### R1: cc-hook binary + `~/.claude/settings.json` installer

**Description:** `extensions/claude-code` ships a `cc-hook` subprocess
binary. On `on_session_start` for any session whose scene declares
`use "claude-code"`, the extension reconciles `~/.claude/settings.json`'s
`hooks` block to route all 9 Claude Code hook events to the installed
`cc-hook` binary, which POSTs the hook payload to the per-session ark
socket.

**Acceptance Criteria:**

- `cc-hook` binary is built by the `extensions/claude-code/bin/cc-hook/`
  target and installed via `cargo install` or ark's release pipeline;
  ark's installer places it under `$XDG_BIN_HOME/cc-hook` (or equivalent
  per platform).
- On session start, if the scene declares `use "claude-code"`, the extension
  reconciles the user's `~/.claude/settings.json` hook entries for the 9
  event kinds:
  `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`,
  `PostToolUse`, `SubagentStart`, `SubagentStop`, `Stop`, `PreCompact`,
  `Notification`.
- Each entry points at the installed `cc-hook` with a command template
  that passes the session id + socket path as args:
  `cc-hook --session <sid> --socket $STATE/sessions/<sid>/cc-hook.sock`.
- Reconciliation is idempotent + surgical: existing hook entries
  targeting other commands are preserved; only ark-managed entries
  (matched by a stable comment / key marker) are updated.
- Version drift (cc-hook binary version ≠ crate version) triggers a
  doctor warning + remediation hint (`ark ext reload claude-code` to re-fire `on-install`).
- On scene without `use "claude-code"`, the extension does NOT modify
  settings.json. Existing ark-managed entries from a prior session stay;
  cc-hook being invoked without a live socket is a no-op (see R2).
- `~/.claude/settings.json` unwritable → doctor error; session start
  does not fail (claude launches; ark sees no `claude-code.*` events —
  graceful degradation).

### R2: Hook IPC socket protocol

**Description:** `cc-hook` communicates with the ark supervisor over a
unix socket at `$STATE/sessions/<id>/cc-hook.sock`. Protocol is NDJSON,
write-only (cc-hook → ark), no reverse messages.

**Acceptance Criteria:**

- Socket path uses the ulid-bearing session id's path leaf.
- `cc-hook` POSTs a single NDJSON line per hook invocation then exits 0:
  ```json
  { "kind": "<HookEventName>",
    "session_id": "<sid>",
    "payload": { ... },   // verbatim Claude Code hook payload
    "emitted_at": "<rfc3339>" }
  ```
- Ark's per-session reader accepts each line as one event, translates
  to `ExtEvent { ext: "claude-code", kind: "<event>", payload }`, and
  forwards to the core event bus.
- Malformed NDJSON lines → logged + skipped with a tracing warning;
  never crash. cc-hook exit code stays 0 even on ark-side parse errors
  (Claude Code's hook spec requires fast exit).
- Socket absent or unreachable → cc-hook logs to stderr and exits 0.
  Claude Code must not be blocked by ark being down.
- No reverse channel in v0.1: ark does not reply, does not approve
  tools, does not send messages back. Claude's TUI handles all user-
  facing prompts.
- cc-hook does not hold the socket open across invocations; one process
  per hook fire.

### R3: Event forwarding

**Description:** All 9 Claude Code hook events are forwarded as
`claude-code.*` ExtEvents on the ark event bus.

**Acceptance Criteria:**

- Event kind mapping:
  - `SessionStart` → `claude-code.session.start`
  - `SessionEnd` → `claude-code.session.end`
  - `UserPromptSubmit` → `claude-code.user.prompt-submit`
  - `PreToolUse` → `claude-code.pre-tool-use`
  - `PostToolUse` → `claude-code.post-tool-use`
  - `SubagentStart` → `claude-code.subagent.start`
  - `SubagentStop` → `claude-code.subagent.stop`
  - `Stop` → `claude-code.stop`
  - `PreCompact` → `claude-code.pre-compact`
  - `Notification` → `claude-code.notification`
- Each ExtEvent carries the verbatim hook payload under `payload`
  (including `agent_id`, `agent_type`, `agent_transcript_path`,
  `tool_name`, `tool_input`, `tool_response`, `last_assistant_message`,
  etc. per hook type).
- Ark's Rhai scene script can match events via
  `on "claude-code.<kind>" { ... }` and inspect `event.payload.*`.
- Envelope test: a synthetic `cc-hook` POST with a representative
  `SubagentStop` payload arrives at an `on "claude-code.subagent.stop"`
  reaction with `event.payload.agent_id`, `event.payload.agent_type`,
  `event.payload.last_assistant_message`, `event.payload.agent_transcript_path`
  all readable.
- Fan-out inside `ClaudeCodeView` subscribes to `claude-code.subagent.start`
  and `claude-code.subagent.stop` (R7); user reactions see the same events.

### R4: Handshake on first hook invocation

**Description:** cc-hook identifies its version in the first POST; ark
validates and surfaces mismatch as a doctor warning.

**Acceptance Criteria:**

- cc-hook's first POST per session has an additional top-level field
  `bridge_version: "<semver>"` alongside `kind`, `session_id`, etc. The
  value is a compile-time constant matching the `claude-code` crate
  version.
- Ark checks the value on first receipt; if it differs from the crate's
  expected version, it records a one-shot doctor warning (surfaced on
  next `ark doctor`) and continues forwarding events.
- Subsequent POSTs in the same session do NOT re-emit `bridge_version`
  (kept lean; one version check per session is enough).
- Mismatch remediation (post plugin-protocol migration): the `on-install`
  lifecycle hook re-runs idempotently on the next ark restart or on
  explicit `ark ext reload claude-code` (which fires `install-event::reload`
  per plugin-protocol R7). The standalone `ark ext claude-code reinstall-hook-binary`
  verb is retired; the user simply triggers a reload.

---

## Part B — Views

### R5: `claude-code` CommandView

**Description:** The extension's primary scene surface is a `claude-code`
view, registered via `#[derive(Facet, View)]` + `impl CommandView for
ClaudeCodeView`. The view's config declares a typed `subagents` handle
attr that lets the scene author wire claude-code to a subagent stack. The
view impl constructs its own argv, env, and cwd.

**Acceptance Criteria:**

**Registration:**
- The `claude-code` view is registered via scene kit R17 derives.
- `impl CommandView for ClaudeCodeView` selects the subprocess render
  mode; the view runs as a zellij native command pane.

**Config schema (facet SHAPE):**
- `model: Option<String>` — claude's `--model` flag value; passed
  through verbatim. Default: unset (claude picks its own).
- `args: Vec<String>` — extra argv passed to claude after the model
  flag. Default empty.
- `cwd: Option<String>` — working dir (Rhai-interpolable, spawn scope).
  Default: the session's `cwd`.
- `subagents: Option<Stack<ClaudeCodeSubagent>>` — reference to a stack
  the view may push subagent children into. Optional; absent disables
  subagent fan-out (subagent events still flow as ExtEvents; they just
  don't get their own panes).

**View references, not owned layout state:**
- `subagents` is a reference to a scene-declared stack node. The view
  holds `&Stack<ClaudeCodeSubagent>` and pushes into it via
  `subagents.spawn_pane(attrs)` on `claude-code.subagent.start`.
- The view never creates panes outside of pushing into the handle it
  was given. If the scene author doesn't wire `subagents=@s`, subagent
  panes don't appear.

**argv + env construction (CommandView impl):**
- argv: `["claude"] + (--model <model> if set) + args`.
- env: starts from the pane env wrapper (`ARK_HANDLE=@<handle>`,
  inherited session env), then view impl adds
  `CLAUDE_HOOK_SOCKET=$STATE/sessions/<sid>/cc-hook.sock` so cc-hook
  (invoked by Claude Code per settings.json) can locate the ark
  socket.
- cwd: from the view's `cwd` attr (Rhai-rendered) or the session default.

**Handle-type validation (scene compile):**
- `claude-code subagents=@s`: compile error if `@s` is not declared as
  `Stack<ClaudeCodeSubagent>` (or superset).
- Errors surface as `error[scene/view-type-mismatch]` at `ark scene check`.

### R5b: Raw `command cmd="claude"` fallback

**Description:** Users who want to launch claude without the `claude-code`
view (e.g. through a wrapper script, during debugging, or with a
non-default binary) can use the generic `command` primitive; claude-code
provides an opt-in `scene_compile_hook` that injects `CLAUDE_HOOK_SOCKET`
into matching panes.

**Acceptance Criteria:**
- When `[claude-code] match_cmds = ["claude", ...]` is configured
  non-empty, the `scene_compile_hook` injects
  `CLAUDE_HOOK_SOCKET=<session-sock-path>` into any pane whose view is
  `command cmd=<member-of-list>`.
- Default config: `match_cmds = []` (fallback disabled; users choose
  the `claude-code` view).
- Raw-command claude panes do NOT receive typed subagent fan-out —
  they have no view struct and no typed attrs. They produce events via
  the shared socket but no view-level fan-out into a stack. Users who
  want subagent tiles use the `claude-code` view.

### R6: `claude-code-subagent` CommandView

**Description:** One view type handles both the stack-child role (one
per live subagent) and, in principle, standalone-pane use (not kit-
guaranteed but not forbidden). Rendering is identical in either
container: collapsed state is pane title only (zellij stack constraint);
expanded state is a live transcript tail.

**Acceptance Criteria:**

**Registration:**
- Alias: `claude-code-subagent`. Scene usage:
  `stack "@subagents" { claude-code-subagent }` declares a
  `Stack<ClaudeCodeSubagent>`.
- Registered via scene kit R17 derives.

**Config schema (facet SHAPE):**
- `id: String` — the subagent id (agent_id from Claude Code hook
  payload). Not authored by the scene writer; set by `ClaudeCodeView`
  at `spawn_pane` time (R7).
- `transcript_path: String` — absolute path to the subagent's
  transcript file (from `agent_transcript_path` in the `SubagentStart`
  payload). Also set by the spawner at spawn time.

**Collapsed rendering (pane title):**
- View emits zellij `RenamePane` on each status transition with a
  formatted title string.
- Format: `"{agent_type} · {status} · {last_tool}"`.
  - `agent_type` — from `SubagentStart` payload.
  - `status` — one of `running`, `done`, `failed` (inferred from
    `SubagentStop.status`).
  - `last_tool` — name of the most recent `PreToolUse` event scoped to
    this `agent_id` (truncate to 16 chars). Empty before first tool use.
- Truncation: total title truncated to 60 chars if longer. Zellij's
  tab/title rendering constrains width.
- No content rendering in collapsed state (zellij stack constraint — see
  R6-limits note below).

**Expanded rendering (focused content area):**
- View tails `transcript_path` (JSONL file) with a ratatui-backed
  renderer. Each transcript line is a JSON object per Claude Code's
  transcript format; view renders tool calls + assistant messages in a
  readable log style.
- Tail window bounded by `[claude-code] transcript_tail_lines`
  (default 200); older lines scroll off (user can zellij-scroll back up
  in the pane if needed).
- `tail -F`-style: survives transcript file truncation / rotation.

**Event subscription:**
- View subscribes to `claude-code.subagent.start`, `claude-code.subagent.stop`,
  and `claude-code.pre-tool-use` ExtEvents filtered by its `id`. Each
  event updates the cached status + last-tool, then triggers a
  `RenamePane` emission.

**R6-limits (kit note, not an acceptance criterion):**
- Zellij stack only renders title for non-focused tiles; content is
  drawn only for the focused tile. This is a zellij constraint, not an
  ark one. If zellij's stack rendering model changes (e.g. multi-line
  tile rendering), we can expand the collapsed renderer later without
  scene-author-visible changes.

### R7: `ClaudeCodeView` spawns subagent children on `SubagentStart`

**Description:** The `claude-code` view fans subagents into its
`Stack<ClaudeCodeSubagent>` handle on `claude-code.subagent.start`
events, keyed by `agent_id`.

**Acceptance Criteria:**
- On `claude-code.subagent.start`, if `subagents` handle is Some, the
  view calls
  `subagents.spawn_pane(ClaudeCodeSubagentAttrs { id, transcript_path })`
  with `id = payload.agent_id` and `transcript_path = payload.agent_transcript_path`.
- On `claude-code.subagent.stop`, the view does NOT remove the child —
  the tile stays so the user can read the final transcript. Status
  transitions via R6's title update (`done` / `failed`). Explicit
  removal is a user affordance (zellij close-pane keybind), not an ark-
  driven cleanup.
- Duplicate `SubagentStart` for the same `agent_id` (should not happen
  per Claude Code semantics, but defensive) → idempotent; no second
  child spawned.
- Subagents started before the `ClaudeCodeView` mounts (e.g. if the
  user reloads the scene mid-session) → missed. Not kit-guaranteed to
  recover; the transcript fs-watcher (R8) can post-hoc populate the
  stack via a scan-existing-subagents-on-mount path (future; not v0.1).
- If `subagents` handle is None (scene author didn't wire it), the view
  no-ops on these events — events still flow to user reactions, just
  no fan-out.

---

## Part C — Cross-cutting

### R8: Transcript fs-watcher

**Description:** The extension's transcript-tail infrastructure watches
`~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` for the main
session and `.../subagents/agent-<id>.jsonl` for each subagent. This is
the data source for R6's expanded rendering + R11's token/cost
accounting.

**Acceptance Criteria:**
- Uses `notify`-based recursive directory subscription on the active
  session's transcript directory.
- The encoded-cwd directory name follows Claude Code's convention
  (cwd with `/` → `-` and a `~/` prefix stripped; verify at
  implementation time against actual claude output).
- New file appearance under `.../subagents/` → no ExtEvent emitted
  (the authoritative source for subagent lifecycle is the `SubagentStart`
  hook via cc-hook; the fs-watcher is for read-only tail, not for
  lifecycle signalling).
- Transcript content emission: views (R6) subscribe to the watcher and
  pull tail segments on-demand (pull model, not push) via a shared
  `TranscriptTail` type exposed by the crate.
- Watcher survives transcript file truncation (Claude Code may rotate
  on compaction).
- Missing transcript directory at session start → watcher logs + waits;
  reacts when the directory appears (Claude Code creates it on first
  session event).

### R9: Config schema

**Description:** The extension contributes one config section.

**Acceptance Criteria:**
- Section `[claude-code]`:
  ```toml
  [claude-code]
  match_cmds = []                   # raw-command fallback allowlist (R5b)
  transcript_tail_lines = 200       # expanded subagent tile tail window
  auto_install_hook_entries = true  # if false, skip settings.json mutation
  ```
- Unknown keys warn but don't fail.
- Config hot-reloads on scene reload (changes visible on next session
  start; mid-session settings.json re-write deferred to hot-reload R12).
- No permission/policy keys in v0.1 (claude's TUI owns that surface).

### R10: Doctor checks

**Description:** The extension contributes `doctor` checks via
`ArkExtension::doctor_checks`.

**Acceptance Criteria:**
- Check: `claude` exists on `$PATH` (`which claude` non-empty). Missing
  → error with install hint (`npm install -g @anthropic-ai/claude-code`
  or similar, per current Claude Code install docs).
- Check: `cc-hook` binary exists at the expected install path and its
  version matches the crate version. Mismatch / absence → warning with
  remediation (`ark ext reload claude-code` per plugin-protocol R7 reload).
- Check: `~/.claude/settings.json` exists and its `hooks` block has
  ark-managed entries for all 9 event kinds pointing at the installed
  cc-hook. Drift → warning with remediation
  (`ark ext claude-code install-hooks`).
- Check: `$STATE/sessions` writable (for socket binding). Unwritable →
  error.
- Check: if any scene under the session state directory references the
  `claude-code` view, a non-raw pane (`claude-code subagents=@s ...`)
  exists — informational only, not a hard check.
- Each check returns `CheckResult { kind, level, message, fix? }` that
  `ark doctor` renders.

### R11: Contributed list columns

**Description:** The extension contributes three `ark list` columns.

**Acceptance Criteria:**
- Column `cc model`: populated from the most recent main-session
  transcript line's `model` field (or `claude-code.session.start`
  payload's model field if present). Empty string when no claude event
  has been seen this session.
- Column `cc tokens`: rolling sum of `input_tokens + output_tokens`
  from transcript `message.usage` entries. Uses Claude Code's own
  accounting.
- Column `cc cost`: rolling sum of transcript per-message `cost_usd`
  entries (when present). Displays as `$<n.nn>`; empty when no cost
  field is present.
- Columns only appear when the `claude-code` extension is loaded.
- Column state is per-session, persisted to `SessionStatus.ext_state["claude-code"]`.

### R12: Hot reload

**Description:** The extension respects the soul kit's ext-reload
semantics.

**Acceptance Criteria:**
- `ark ext reload claude-code` re-runs settings.json reconciliation
  (R1), re-binds the cc-hook socket, re-runs the transcript fs-watcher
  setup. Live `claude-code` view instances receive a re-initialised
  socket reader; typed `Stack<_>` ref held by the view survives the
  reload (points at layout node, not the socket).
- In-flight Claude Code sessions survive: the user's `claude` process
  is not touched; its next hook fire reconnects to the fresh socket
  via cc-hook's fresh-invocation model (R2).
- Scene hot-reload cascades: config changes (match_cmds,
  transcript_tail_lines) visible on next event dispatch. View-type
  constraints re-validated; mismatched wiring surfaces as
  `error[scene/view-type-mismatch]` and the reload is rejected (old
  scene stays live).

### R13: Test harness

**Description:** Integration tests without a real `claude` binary.

**Acceptance Criteria:**
- A `mock-claude` binary under `crates/test-fixtures/claude-code/`
  invokes `cc-hook` with scripted hook payloads per flags:
  - `emit-only` — replays a canned event timeline (including
    `SubagentStart` / `SubagentStop` pairs) via direct cc-hook
    invocations.
  - `subagent-burst` — fires N `SubagentStart` events in a row for fan-
    out testing.
  - `transcript-write` — writes synthetic transcript JSONL to a
    configurable path; used to exercise R6/R8/R11 without real claude.
- Each extension test uses `mock-claude` + a harness-installed `cc-hook`
  variant. Two integration strategies:
  1. Unit-level: scene-compiled-offline tests instantiate
     `ClaudeCodeView` + `ClaudeCodeSubagentView` with mock
     `Stack<ClaudeCodeSubagent>` handles (per scene kit R17 test stubs)
     and exercise view logic directly.
  2. PTY-level: `test-fixtures/claude-code-smoke/` runs `mock-claude`
     under real zellij via a scene that uses the `claude-code` view;
     asserts event forwarding, stack fan-out on SubagentStart, title
     updates on status transitions.
- `cc-hook` unit tests independently assert NDJSON serialisation +
  socket-unreachable behaviour.
- Transcript fs-watcher tests generate fake transcript directory trees
  to exercise tail emission without `claude`.

---

## Stretch (post-v0.1, captured for context)

Not required for first ship. Listed so later sessions don't re-invent:

- **MCP server for ark ops (`ark-mcp` binary).** Expose `IntentRegistry`
  as MCP tools. User adds it to their `.mcp.json`. Claude calls
  `ark_dispatch(kdl)` / `ark_ops()` via MCP handshake. Matches pi-control's
  R13–R16 via a different mechanism. Separate design session (likely
  paired with pi-control v0.2 ship).
- **Scene-propose tool via MCP + `claude-code-scene-propose` view.**
  Port of pi kit R17–R18. Requires the MCP surface above.
- **Pre-compact findings snapshot.** On `claude-code.pre-compact`,
  stash branch summary into a findings pane (scene-level, not ext).
- **Multi-claude race.** Launch N claude panes with different models on
  the same prompt; ark picks a winner. Falls out of scene primitives
  once MCP dispatch exists.
- **Voice → claude → ark dispatch.** Same story as pi's stretch.

---

## Open questions

- **cc-hook install path strategy.** Does ark install to
  `$XDG_BIN_HOME/cc-hook` (user-scope) or per-session state dir (avoid
  polluting global PATH)? User-scope is simpler for settings.json
  referencing; session-scope would need per-session settings.json
  rewrites. Lean user-scope; confirm at R1 implementation.
- **settings.json reconciliation ergonomics.** Users may have custom
  hook entries. Stable marker = comment like `// ark-managed` (JSON
  doesn't support comments) or a nested object key like
  `"ark_managed": true` on each entry. Decide at R1 implementation.
- **Mid-session hook-entry drift.** If the user edits settings.json
  while ark is running, the next cc-hook fire may go to an out-of-date
  binary path. Policy: accept. Doctor surfaces drift; user runs
  reload.
- **Transcript directory encoding.** Claude Code uses a specific cwd-
  encoding scheme for `~/.claude/projects/<encoded>/`. Verify the
  exact encoding at R8 implementation (reading claude source or
  running an experiment; do not assume).
- **Subagent observability when the user's scene pre-dates ark.** If a
  user starts claude outside an ark session then later runs ark, the
  hook entries already point at cc-hook, which has no live socket to
  write to. R2 already covers "socket absent → no-op"; confirm this
  path doesn't surface as an error to the user.
- **Auto-approve-safe + MCP policy.** Deferred to MCP stretch work.

---

## Salvage from pre-2026-04-18 code

The earlier scope cut deleted crates that contain directly-usable
content for this extension. Recover from `git show <pre-cut-sha>:<path>`:

- `crates/hook/src/lib.rs` — was `ark-hook`. Entire crate's functionality
  becomes `extensions/claude-code/bin/cc-hook/`.
- `crates/hook/src/event.rs` — `HookEvent` enum with the 9 Claude Code
  hook names. Restore into ext; drives R3's event-kind mapping.
- `crates/hook/src/payload.rs` — hook-payload deserialisation. Restore
  into ext; drives R2's NDJSON shape.
- `crates/orchestrators/claude-code/src/transcript.rs` (or whatever it
  was named) — transcript tail logic. Restore into ext; drives R6 + R8.
- `crates/types/src/permission.rs` — `READ_ONLY_TOOLS` +
  `PermissionPolicy` + `POLICY_FILE_NAME`. **Not restored** in v0.1
  (permission UI dropped). Preserve for MCP stretch work.

No kit file resurrection — this kit supersedes the deleted
`cavekit-engine-claude-code.md`, `cavekit-orchestrator-claude-code.md`,
and `cavekit-hook-ipc.md` as a consolidated single-crate spec.

---

## What this unblocks

- Claude Code as a daily-driver engine on ark with first-class scene
  integration, validated against real user workflow.
- The reference implementation for the Phase 2 ext-hook surface; pi
  (v0.2) follows with minimal churn once this pattern lands.
- User-authored claude-driven workflows: "when claude writes a file,
  show the diff; when claude spawns a subagent, fan it into the stack;
  when claude finishes a turn, emit a status line."
- The MCP-server stretch story for runtime tool injection, once the
  hook-driven observability surface is proven.

---

## Execution

After soul Phase 2 lands:

```
/ck:sketch cavekit-claude-code   # decompose R1-R13 into build tasks
/ck:map                          # DAG, rough estimate 25-35 tasks, 6-8 tiers
/ck:make                         # parallel dispatch, peer-review via Codex
/ck:scan                         # verify built code matches kit
```

Suggested tier ordering (independent of `/ck:map`'s output):

1. Socket protocol + mock-claude (R2, R13) — foundation.
2. cc-hook binary + settings.json installer + handshake (R1, R4).
3. Event forwarding (R3).
4. `claude-code` CommandView (R5) + raw-cmd fallback (R5b).
5. `claude-code-subagent` view (R6) + transcript fs-watcher (R8).
6. Fan-out wiring (R7).
7. Doctor + list columns (R10, R11).
8. Config + hot-reload (R9, R12).

Each tier lands green (`cargo check --workspace --tests` + `cargo test
--workspace`); the mock-claude smoke variant gates tiers 2 onward. Tier
5 is the first to exercise the 2026-04-18 typed-handle + stack scene
revision.
