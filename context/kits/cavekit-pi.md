---
created: "2026-04-17"
last_edited: "2026-04-18"
status: deferred
defer_target: v0.2
depends_on:
  - cavekit-soul.md           # Phase 2 ext-hook surface + Phase 3/4 deletions
  - cavekit-scene.md          # IntentRegistry, ExtEvent, typed handles, stack
  - cavekit-claude-code.md    # v0.1 engine integration; pi ships second, reusing the validated pattern
---

# Spec: pi — Integrating pi.dev as an ark Extension Family

> **Status DEFERRED (v0.2) as of 2026-04-18 pivot.** Claude-code ships first
> as v0.1's engine integration (`cavekit-claude-code.md`); pi follows in
> v0.2 once the ext-hook surface + typed-handle + stack revisions are
> proven against claude-code. All R1-R22 content below is preserved
> verbatim for the v0.2 build — ark gains a second engine integration
> with minimal churn. See
> `context/plans/handoff-2026-04-18-claude-code-first-pivot.md` for the
> pivot rationale.

## Scope

Specifies the extension family that integrates [pi.dev](https://github.com/badlogic/pi-mono)
— a terminal coding agent — into ark. Three crates in `extensions/`:

| Crate                     | Role                                                          |
|---------------------------|---------------------------------------------------------------|
| `extensions/pi-core`      | Event bridge + pi process lifecycle + the `pi` CommandView.   |
| `extensions/pi-subagents` | fs watcher + subagent views (`pi-subagent-tile`, `pi-subagent-log`). |
| `extensions/pi-control`   | Exposes ark's `IntentRegistry` + scene edits as pi LLM tools + `pi-scene-propose` view. |

Each crate is independently loadable. `pi-subagents` and `pi-control` depend on
`pi-core`'s `ExtEvent` stream but not on each other.

## Motivation

After the 2026-04-18 pivot (same-day revision of the earlier scope cut),
**claude-code ships first in v0.1** (`cavekit-claude-code.md`) and **pi
ships second in v0.2**. Claude-code validates the Phase 2 ext-hook surface
+ the 2026-04-18 typed-handle + stack revision against the user's actual
daily-driver workflow; pi inherits the validated pattern and extends it
with runtime tool injection (pi-control) and propose-style views that
claude-code doesn't need.

Three ideas the pi family unlocks, none of which require core edits:

1. **Events in, reactions out.** Every `pi.*` event is visible to the user's
   scene via the `FlatEvent` shim — `on "pi.tool.result" where="payload.toolName == \"edit\""`
   is a one-liner.
2. **Views coordinate fan-out via typed handles.** pi-core ships a `pi`
   CommandView with typed handle attrs (`subagents: Stack<PiSubagentTile>`,
   `logs: Pane<PiSubagentLog>`, `propose: Pane<PiSceneProposeView>`). The
   user's scene wires the sinks once; the view manages children internally.
   No `@sub-{payload.id}` string interpolation, no user reactions for the
   subagent flow.
3. **Pi drives ark.** `extensions/pi-control` exposes ark's intent registry +
   scene edits as pi tools. Pi calling `ark_dispatch("spawn @diff { view …}")`
   drives zellij directly. Every ark op — core *and* ext-registered — is
   automatically addressable by pi the moment the op is registered.

## Dependencies

Depends on:

- **Soul Phase 1 complete** — `SessionSpec`/`SessionStatus`/`SessionId` + shrunk
  `CoreEvent` + bare `ark` launch green.
- **Soul Phase 2 complete** — the ext-hook surface:
  - `ArkExtension::on_session_start` / `on_session_end` — pi-core uses these
    to spawn / tear down the bridge socket and install the TS bridge asset.
  - `ArkExtension::scene_compile_hook` — pi-core uses this for the fallback
    `match_cmds` env-inject (R6b). Primary path is the `pi` view's own
    CommandView impl setting its env natively (R6).
  - `ArkExtension::register_intents` — pi-subagents and pi-control use this
    to contribute ops into the registry.
  - `ArkExtension::doctor_checks` — each crate contributes preflight checks.
  - `ArkExtension::list_columns` — pi-core contributes `pi model` and `pi tokens`.
  - `ArkExtension::control_verbs` — pi-core registers `ark ext pi new <name>`.
- **Soul Phase 3 complete** — ACP deleted. No `ark.acp.*` ops to worry about
  in `ark_ops()` discovery or allowlists.
- **Soul Phase 4 complete** — ark-side claude-code + cavekit crates deleted;
  claude-code rehomed into `extensions/claude-code/` (v0.1 engine integration
  per `cavekit-claude-code.md`). No legacy `[engine.*]` config sections or
  orchestrator infrastructure to navigate around.
- **claude-code extension shipped (v0.1)** — `cavekit-claude-code.md` landed;
  ext-hook surface proven against the user's daily workflow. pi inherits
  the validated pattern.
- **Scene v3 2026-04-18 revision** — typed view-parametric handles + `stack`
  primitive. pi-core's typed handle attrs (`logs: Pane<PiSubagentLog>`,
  `subagents: Stack<PiSubagentTile>`) are unavailable without this revision.
- **`CoreEvent::Ext(ExtEvent)`** — carries `ext = "pi"` / `"pi-subagents"` payloads.

## Non-goals

- **Embedding pi's UI inside a custom ark-rendered view.** Pi's TUI stays
  native; it runs in a regular zellij pane. Matches the engine-in-pane UI model.
- **Supporting pi's `--mode rpc`.** We use in-proc TS extension + unix socket.
  RPC mode is strictly worse for this use case (loses pi's TUI).
- **Packaging pi itself.** Users install pi themselves via npm / brew / etc.
  Ark's doctor check surfaces absence.
- **Windows support.** Unix-only, matching ark's posture.
- **A generic "engine bridge" abstraction.** Claude Code is its own ext; pi is
  its own ext. No `ark-ext-engine` shared crate. Copy-paste-and-specialise if a
  third engine arrives.

---

## Architecture summary

Example scene:

```kdl
scene "pi-dev" {
    use "pi-core"
    use "pi-subagents"
    use "pi-control"

    layout {
        tab "@main" cwd="{cwd}" focus="true" {
            row {
                pane "@builder" span="3" {
                    pi model="claude-sonnet-4-6"
                       subagents=@subagents
                       logs=@logs
                       propose=@propose
                }
                col span="1" {
                    stack "@subagents" { pi-subagent-tile }
                    pane  "@logs"      { pi-subagent-log }
                    pane  "@propose"   { pi-scene-propose }
                }
            }
        }
    }
}
```

Runtime:

```
┌──────────────────── zellij ─────────────────────┐
│  [@builder pane] pi (interactive TUI)           │
│    └── in-proc: ~/.pi/agent/extensions/         │
│                 ark-bridge.ts                   │
│                                                  │
│  [@subagents stack] pi-subagent-tile * N        │
│  [@logs pane] pi-subagent-log                   │
│  [@propose pane] pi-scene-propose               │
└──────────────────────────────────────────────────┘
         │         ▲
         │ NDJSON  │ reverse RPC
         ▼         │
┌─────────── supervisor ────────────────────────────┐
│  pi-core — bridge + `pi` CommandView              │
│    ├── PiView receives typed handle refs from the │
│    │   scene: &Stack<PiSubagentTile>,             │
│    │   &Pane<PiSubagentLog>,                      │
│    │   &Pane<PiSceneProposeView>.                 │
│    │   The stack + panes live in the layout; the  │
│    │   view only holds references and pushes into │
│    │   them (e.g. subagents.spawn_pane(attrs)).   │
│    ├── fs-watcher + bridge events drive fan-out   │
│    └── list columns + doctor checks               │
│                                                    │
│  pi-subagents — ships tile + log views            │
│  pi-control   — ships pi tools + propose view     │
│                 (ark_ops / ark_dispatch / …)      │
└────────────────────────────────────────────────────┘
```

Direction of data: the view is a *client* of the layout. It accepts typed
references to containers and panes the scene declared, and it pushes updates
into them (spawn a child in a stack, replace the content of a pane). It never
*owns* layout state — the layout is the scene's artefact; views read config
and push effects.

Everything below is structured as `Rn` requirements with testable criteria.

---

## Part A — pi-core (event bridge, lifecycle)

### R1: Bridge TS extension shipped as embedded asset

**Description:** `extensions/pi-core` ships a versioned `ark-bridge.ts` file
as a compiled-in asset. On `on_session_start` for any session whose scene
declares `use pi`, pi-core ensures a matching copy lives at
`~/.pi/agent/extensions/ark-bridge.ts`.

**Acceptance Criteria:**
- The bridge asset has a version string embedded in its first-line comment
  (`// ark-bridge.ts v<semver>`) matching the `pi-core` crate version.
- On session start, if the file is absent OR its embedded version differs
  from the crate's version, pi-core overwrites the file atomically (write to
  sibling `.tmp` + rename).
- If the file matches the expected version, no write occurs.
- Writing is skipped entirely when the scene does not `use pi`.
- Install errors (e.g. `~/.pi` unwritable) surface as a `doctor` warning but
  do not fail session start. Pi simply launches without the bridge and ark
  sees no `pi.*` events (graceful degradation).

### R2: Bridge socket protocol

**Description:** The TS bridge and pi-core communicate over a unix socket
at `$STATE/sessions/<id>/pi-bridge.sock`. Messages are NDJSON, LF-delimited.

**Acceptance Criteria:**
- Socket path uses the ulid-bearing session id's path leaf.
- Each line is a single JSON object. Parsing is strict; malformed lines are
  logged + skipped with a one-line tracing warning, never crash.
- Two message shapes are defined:
  - **Forward (TS → Rust):**
    ```json
    { "dir": "fwd", "kind": "<pi_event_type>", "payload": { … } }
    ```
  - **Reverse (Rust → TS):**
    ```json
    { "dir": "rev", "id": "<uuid>", "method": "sendMessage"|"registerTool"|"setModel"|…,
      "params": { … } }
    ```
    TS responds with `{ "dir": "rev-ack", "id": "<same-uuid>", "ok": true|false, "error": "…" }`.
- Reverse-channel responses carry the originating `id` for correlation.
- The TS bridge auto-reconnects if the socket drops; re-sends a
  `{kind: "bridge.resumed"}` line on reconnect.

### R3: Event forwarding

**Description:** Every event listed on `ExtensionAPI.on(...)` (per pi's
`extensions.md`) is subscribed by the TS bridge and forwarded verbatim to
pi-core, which wraps it as `ExtEvent { ext: "pi", kind: "<event>", payload }`.

**Acceptance Criteria:**
- All lifecycle events are forwarded: `session_start`, `session_shutdown`,
  `session_before_{switch,fork,compact,tree}`, `session_compact`, `session_tree`,
  `agent_start`, `agent_end`, `turn_start`, `turn_end`, `message_start`,
  `message_update`, `message_end`, `tool_call`, `tool_result`,
  `tool_execution_{start,update,end}`, `model_select`, `user_bash`, `input`,
  `before_provider_request`, `after_provider_response`.
- Payloads carry pi's verbatim event payload under `payload`, serialised via
  `JSON.stringify` on the TS side.
- Ark's Rhai scene script can match events via `on "pi.<kind>" { … }` and
  inspect `event.payload.*`.
- A generic envelope test asserts that `pi.tool.call` with
  `{toolCallId, toolName: "bash", input}` arrives at an `on "pi.tool.call"`
  reaction with those fields readable.

### R4: Event throttling

**Description:** `message_update` fires per streamed token and can reach
hundreds of events per second. The TS bridge coalesces these before forward.

**Acceptance Criteria:**
- `message_update` events for the same `message.id` are coalesced in the TS
  bridge with a ≤100 ms debounce window.
- `message_start` and `message_end` are never coalesced.
- The final emitted `message_update` carries the most recent `message` state
  (not a mid-stream snapshot).
- Other event types are forwarded without throttling.
- A load test: 1000 `message_update` in 1 s → ≤15 forwarded lines observed on
  the socket.

### R5: Reverse-channel API

**Description:** pi-core can call a narrow slice of pi's `ExtensionAPI`
through the reverse channel. Initial methods: `sendMessage`, `sendUserMessage`,
`registerTool`, `setModel`, `getActiveTools`, `setActiveTools`, `getSystemPrompt`,
`compact`, `shutdown`.

**Acceptance Criteria:**
- pi-core exposes a typed Rust API `PiHandle` with one method per supported
  reverse call, each returning `Result<Value, PiBridgeError>`.
- Each call serialises to one `{dir: "rev", id, method, params}` socket line
  and awaits the correlated `rev-ack`.
- Call timeout: 5 s default (override via arg). Timed-out calls return
  `PiBridgeError::Timeout`, bridge remains live.
- Unknown method names on the TS side respond with `ok: false, error: "unknown
  method"` rather than silence.
- An integration test round-trips a `registerTool` call: ark invokes it, TS
  bridge receives the registration, pi subsequently sees the tool in its tool
  catalogue.

### R6: pi-core ships the `pi` CommandView

**Description:** pi-core's primary scene surface is a `pi` view (alias
`pi`), registered via `#[derive(Facet, View)]` + `impl CommandView for PiView`.
The view's config declares typed handle attrs that let the scene author wire
pi to a subagent stack, a log pane, and a scene-propose pane. The view impl
constructs its own argv, env, and cwd — no `scene_compile_hook` env injection
is needed on the primary path.

**Acceptance Criteria:**

**Registration:**
- pi-core registers the `pi` view via scene kit R17 derives.
- `impl CommandView for PiView` selects the subprocess render mode; the view
  runs as a zellij native command pane.

**Config schema (facet SHAPE):**
- `model: Option<String>` — pi's `--model` flag value; passed through verbatim.
  Default: unset (pi picks its own).
- `args: Vec<String>` — extra argv passed to pi after the model flag. Default empty.
- `cwd: Option<String>` — working dir (Rhai-interpolable, spawn scope). Default:
  the session's `cwd`.
- `subagents: Option<Stack<PiSubagentTile>>` — reference to a stack the view
  may push subagent tiles into. Optional; absent disables subagent fan-out.
- `logs: Option<Pane<PiSubagentLog>>` — reference to a pane the view may set
  to tail a specific subagent's log. Optional.
- `propose: Option<Pane<PiSceneProposeView>>` — reference to a pane the view
  hands off to pi-control when `ark_scene_propose` is pending. Optional; if
  absent, `ark_scene_propose` tool returns `{accepted: false, reason: "no propose pane"}`.

**View references, not owned layout state:**
- Each handle attr is a *reference* to a scene-declared layout node. The
  stack and panes exist in the scene; the view holds `&Stack<V>` / `&Pane<V>`
  refs and pushes into them (e.g. `subagents.spawn_pane(attrs)`).
- The view never creates panes outside of pushing into the handles it was
  given. If the scene author doesn't wire `subagents=@s`, the view has no
  way to show subagents — that's the desired coupling.

**argv + env construction (CommandView impl):**
- argv: `["pi"] + model-flag (if set) + args`.
- env: starts from pane env wrapper (`ARK_HANDLE=@<handle>`, inherited
  session env), then pi-core's view impl adds `ARK_PI_SOCKET=$STATE/sessions/<id>/pi-bridge.sock`.
- cwd: from the view's `cwd` attr (Rhai-rendered) or the session default.

**Handle-type validation (scene compile):**
- `pi subagents=@s`: compile error if `@s` is not declared as
  `Stack<PiSubagentTile>` (or any superset that includes `PiSubagentTile`).
- `pi logs=@l`: compile error if `@l` is not `Pane<PiSubagentLog>`.
- `pi propose=@p`: compile error if `@p` is not `Pane<PiSceneProposeView>`.
- All errors surface as `error[scene/view-type-mismatch]` at `ark scene check`.

### R6b: Raw `command cmd="pi"` fallback

**Description:** Users who want to launch pi without the `pi` view (e.g.
through a wrapper script, during debugging, or with a non-default binary)
can use the generic `command` primitive; pi-core provides an opt-in
`scene_compile_hook` that injects `ARK_PI_SOCKET` into matching panes.

**Acceptance Criteria:**
- When `[pi.core] match_cmds = ["pi", "pi-dev", …]` is configured non-empty,
  pi-core's `scene_compile_hook` injects `ARK_PI_SOCKET=<session-sock-path>`
  into any pane whose view is `command cmd=<member-of-list>`.
- Default config: `match_cmds = []` (fallback disabled; users choose the
  `pi` view).
- Raw-command pi panes do NOT receive typed handle coordination — they have
  no view struct and no typed attrs. They get events + reverse-channel via
  the shared socket but no view-level fan-out.
- The TS bridge reads `ARK_PI_SOCKET` at module load; unset = bridge no-ops.

### R7: Doctor checks

**Description:** pi-core contributes `doctor` checks via
`ArkExtension::doctor_checks`.

**Acceptance Criteria:**
- Check: `pi` exists on `$PATH` (`which pi` non-empty). Missing → error with
  install hint.
- Check: `~/.pi/agent/extensions/ark-bridge.ts` exists and its first-line
  version comment matches crate version. Mismatch / absence → warning with
  remediation (`ark ext pi-core reinstall-bridge`).
- Check: `$STATE/sessions` is writable (for socket binding). Unwritable →
  error.
- Check: if any scene under the session state directory references the `pi`
  view, a non-raw pane (`pi model=… subagents=… …`) exists — informational
  only, not a hard check.
- Each check returns a structured `CheckResult { kind, level, message, fix? }`
  that `ark doctor` renders.

### R8: Contributed list columns

**Description:** pi-core contributes two `ark list` columns.

**Acceptance Criteria:**
- Column `pi model`: populated from the most recent `pi.model_select` event's
  `model.name`. Empty string when no pi event has been seen this session.
- Column `pi tokens`: rolling sum of `turn_end` token counts (input +
  output, using pi's own accounting from the event payload).
- Columns only appear when the `pi-core` extension is loaded.
- Column state is per-session, persisted to `SessionStatus.ext_state["pi"]`.

---

## Part B — pi-subagents

### R9: Auto-install pi-subagents npm package on first use

**Description:** `extensions/pi-subagents` auto-installs the community
`pi-subagents` npm package into pi's extension dir on first session where
the scene references any `pi.subagent.*` intent OR any `on "pi-subagents.*"`
reaction.

**Acceptance Criteria:**
- On `scene_compile_hook`, if any intent reference or event selector
  matches `pi.subagent.*` / `pi-subagents.*`, pi-subagents checks
  `~/.pi/agent/extensions/pi-subagents/` for the expected package.
- If absent, pi-subagents runs `npm install pi-subagents --prefix
  ~/.pi/agent` (or equivalent) and vendors the expected version into pi's
  extensions dir. Subsequent sessions skip the install.
- Install failure surfaces as `doctor` error + `ExtEvent { ext: "pi-subagents",
  kind: "install.failed", payload: { error } }`. Session continues; subagent
  features are inert until install succeeds.
- The installed version is pinned by pi-subagents crate version.
- `ark ext pi-subagents reinstall` forces reinstall.

### R10: Filesystem-watched subagent observability

**Description:** pi-subagents complements bridge events with a filesystem
watcher on `$TMPDIR/pi-subagents-<scope>/async-subagent-runs/`. Filesystem is
the source of truth for subagent lifecycle per the pi-subagents docs.

**Acceptance Criteria:**
- Watcher uses `notify`-based recursive directory subscription.
- New `<id>/status.json` appearing → emit `ExtEvent { ext: "pi-subagents",
  kind: "started", payload: { id, parent, task, spawned_at } }`.
- `events.jsonl` append → if the final line's event kind is `progress`, emit
  `kind: "progress"` with `{ id, message, tokens? }`.
- `status.json` transition to terminal state (`succeeded` / `failed` /
  `cancelled`) → emit `kind: "complete"` with `{ id, status, output_path,
  duration_ms }`.
- Events from the TS bridge's `subagent:started` / `subagent:complete`
  channel are treated as secondary confirmation; duplicates are deduplicated
  by `id`.
- Watcher survives TMPDIR rotation (detects recreated parent dir).

### R11: Subagent views + minimal intent surface

**Description:** pi-subagents ships two views registered via scene kit R17
derives. The views are how subagents appear in the scene; the pi view in
pi-core coordinates when and where via its typed handle attrs. Only one
cross-scene intent survives, for asking the pi view to focus a named
subagent from outside.

**Acceptance Criteria:**

**`pi-subagent-tile` view (CommandView):**
- Alias: `pi-subagent-tile`. Scene usage: `stack "@subagents" { pi-subagent-tile }`
  declares a `Stack<PiSubagentTile>`.
- Config attrs: `id: String` (the subagent id; set by the spawner at
  `spawn_pane` time, not by the scene author).
- Renders as a compact status row when collapsed (per zellij stack default):
  `<short-id> · <status> · <tokens> · <elapsed>`. Rendered as a live
  `events.jsonl` tail when expanded.
- Subscribes to `pi-subagents.*` ExtEvents filtered by its own `id`; updates
  its rendered state on each event.

**`pi-subagent-log` view (CommandView):**
- Alias: `pi-subagent-log`. Scene usage: `pane "@logs" { pi-subagent-log }`
  declares a `Pane<PiSubagentLog>`.
- Config attrs: `id: Option<String>` (subagent id to tail; if unset, pane is
  idle until something sets it via `pi.subagent.focus` or the pi view's
  internal wiring).
- Renders a `tail -F`-style stream of the subagent's `events.jsonl` formatted
  for readability.
- When its `id` attr is replaced at runtime (via `Pane::replace_view` from
  the pi view), it rewinds and tails the new id's file.

**Ext-registered intents (minimal surface):**
- `pi.subagent.focus @pi_view id="<id>"` — asks the `pi` view referenced by
  `@pi_view` to focus the subagent with that id: the view calls
  `subagents.focus_child(…)` on its `Stack<PiSubagentTile>` handle and
  `logs.replace_view("pi-subagent-log", {id})` on its `Pane<PiSubagentLog>`
  handle. No-op if the view has no `subagents`/`logs` wiring.
- That's the only cross-scene intent. Tailing, showing-log, and closing
  all happen inside the pi view's fan-out logic, not via user-dispatched
  ops.
- The intent is idempotent / fail-fast per scene kit T-055; unknown id →
  `SceneError::OpFailed`.
- Addressable via `ark_dispatch(…)` through pi-control; appears in
  `ark_ops()`.

### R12: pi-subagents ExtEvents are scene-observable

**Description:** ExtEvents emitted by pi-subagents are usable in Rhai
selectors.

**Acceptance Criteria:**
- An `on "pi-subagents.started" { … }` reaction fires with
  `event.payload.id` populated.
- `on "pi-subagents.complete" where="payload.status == \"failed\"" { … }`
  matches only failed subagents.
- A test scene that spawns one subagent and observes one `started`
  followed by one `complete` passes with zero duplicate deliveries under
  concurrent bridge + fs-watcher sources (dedup verified).

---

## Part C — pi-control (registry-as-tools)

### R13: `ark_ops()` discovery tool

**Description:** pi-control registers a pi tool `ark_ops` (via pi-core's
reverse channel, at session start) that returns the live intent registry.

**Acceptance Criteria:**
- The tool takes no arguments.
- It returns a JSON array of
  `{ name: string, description: string, args_shape: <typebox-compatible schema>, origin: string }`.
- `origin` is `"ark.core"` for core ops and `"ext:<name>"` for
  extension-contributed ops.
- The list reflects the current registry: after `ark.core.reload_scene` or a
  hot-reload that adds/removes ext ops, the next `ark_ops()` call returns the
  updated list.
- Ops whose fully-qualified name appears in `pi.control.deny_ops` are
  omitted from the list. Ops absent from `pi.control.allow_ops` (when that
  allowlist is non-empty) are also omitted.
- The tool description instructs the LLM to call `ark_ops` once per session
  before issuing `ark_dispatch`, and to re-call on `ark.scene.reloaded`
  events.

### R14: `ark_dispatch(kdl)` execution tool

**Description:** pi-control registers a pi tool `ark_dispatch` that accepts a
KDL op-list fragment, parses it with the scene parser, and dispatches each
op through `IntentRegistry` with an `ext:pi-control` origin.

**Acceptance Criteria:**
- Tool signature: `ark_dispatch(kdl: string) -> { results: [{op, ok, value|error}] }`.
- The KDL fragment is parsed as if it were the body of a reaction block
  (`on some_event { <here> }`). Any scene-legal op list is valid input.
- Parse errors return a structured failure with a miette-style snippet, not
  a stack trace.
- Each op is dispatched sequentially. `IntentContext.origin = "ext:pi-control"`.
  Handle-type hints and error semantics (T-055 idempotency, fail-fast at op
  level) are identical to user-authored reactions.
- The tool returns *after* all ops have completed (or the first error, per
  fail-fast). The results array records per-op outcome.
- `IntentValue` returns (non-None) are surfaced to pi under `value`. Pi can
  inline the returned values into subsequent tool calls.
- Nested `spawn @h { view { command … } }` bodies are supported because the
  scene parser is being reused directly.

### R15: Allowlist enforcement

**Description:** pi-control enforces an op-name allowlist / denylist *before*
dispatch, not after.

**Acceptance Criteria:**
- Config shape:
  ```toml
  [pi.control]
  allow_ops = ["ark.core.*", "pi.subagent.*"]   # glob patterns
  deny_ops = ["ark.core.exec", "ark.core.reload_scene"]
  ```
- An op is permitted iff (allow_ops is empty OR some pattern matches) AND no
  deny_ops pattern matches. Deny always wins.
- Unknown op names surface as `op_unknown`, not as `op_denied`.
- Denied ops in an `ark_dispatch` batch return a per-op result
  `{ok: false, error: "denied: <op>"}` *without* executing that op. Sibling
  ops still execute (controlled by policy: default is fail-fast, but
  `[pi.control] continue_on_deny = true` relaxes this — default `false`).
- Policy changes via scene reload are picked up on the next `ark_dispatch`
  call (no pi restart needed).

### R16: Origin attribution is end-to-end visible

**Description:** Any side effect originated by pi-control is traceable back
to it.

**Acceptance Criteria:**
- `IntentContext.origin = "ext:pi-control"` for every op dispatched via
  `ark_dispatch`.
- Tracing spans emitted from ops carry the `origin` field.
- `ExtEvent`s emitted by ops (via the `ark.core.emit` op) carry
  `source = "ext:pi-control"`.
- A user-authored reaction can distinguish pi-control-sourced emissions:
  `on "some.event" where="event.source == \"ext:pi-control\"" { … }`.

### R17: `ark_scene_propose(diff)` tool

**Description:** pi-control registers a third pi tool for persistent scene
edits. It is the only pi-control tool that writes disk. The UI lives in a
user-declared `pi-scene-propose` pane (R18); pi-control pushes state into
that pane via the `Pane<PiSceneProposeView>` handle the pi view received.
No overlay mechanism.

**Acceptance Criteria:**
- Tool signature:
  `ark_scene_propose(unified_diff: string, rationale?: string) -> { accepted: bool, reason?: string }`.
- The diff is unified-diff format targeting the currently-active scene file.
- The tool is gated by `[pi.control] allow_scene_writes` (default `false`).
  Gated calls return `{accepted: false, reason: "scene writes disabled"}`
  immediately.
- Resolution path:
  1. pi-control resolves the target scene path from the session.
  2. Applies the diff to a scratch buffer.
  3. Parses + shape-checks the result. Parse failure → return
     `{accepted: false, reason: "parse error: <miette>"}` without touching
     the propose pane.
  4. Obtains the `Pane<PiSceneProposeView>` handle from the active `PiView`
     (whichever pane the user wired `pi propose=@p` on). If no pi view has
     a propose handle wired, return `{accepted: false, reason: "no propose pane"}`.
  5. Pushes propose state into that pane via the view's typed API
     (`propose.replace_view("pi-scene-propose", { diff, rationale, pending_id })`).
     The view renders the pending state; user actions resolve the tool call.
  6. Blocks the pi tool call on a oneshot channel until the `pi-scene-propose`
     view reports accept / reject / timeout / stale.
  7. On accept, atomically writes the scratch buffer to disk via the scene
     hot-reload path.
- Default blocking timeout 300 s (user-configurable). Timeout →
  `{accepted: false, reason: "timeout"}`, propose pane reverts to idle.
- Concurrent scene-file edits during a pending propose → tool resolves with
  `{accepted: false, reason: "scene changed concurrently"}` (detect via
  file mtime at propose-time vs accept-time).
- Only one propose may be pending per session. Issuing a second
  `ark_scene_propose` while one is pending returns
  `{accepted: false, reason: "propose already pending"}`.

### R18: `pi-scene-propose` view

**Description:** pi-control ships the view that renders pending scene
proposals. Scene author declares where it lives; pi-control tool calls push
state into it.

**Acceptance Criteria:**

**Registration:**
- Alias: `pi-scene-propose`. View type: CommandView (runs a `ark pane
  scene-propose` ratatui helper, or equivalent pi-control-owned binary).
- Scene usage: `pane "@propose" { pi-scene-propose }` declares a
  `Pane<PiSceneProposeView>`.

**Render states:**
- **Idle.** No propose pending. Pane shows a one-line placeholder (`pi:
  scene-propose idle`). `set_status` clears pi-control's propose key.
- **Pending.** Rendered when pi-control pushes state. Shows a two-column
  unified-diff view (before / after) using the shared `delta`-style
  renderer; rationale (if provided) shown above the diff as wrapped prose.
  `set_status` pushes `pi: pending scene propose — y/n` with severity `info`.

**Pending-state keybinds (pane-scoped):**
- `y` / `Enter` → resolve with `accept`.
- `n` / `Esc` → resolve with `reject`.
- `e` → open the diff in `$EDITOR`; on save, re-validate; if valid, re-push
  the new diff as a replacement pending state (same `pending_id`); if
  invalid, display parse error inline and stay pending.

**Resolution wiring:**
- On any resolution, the view calls back into pi-control (via the common
  pane-view-to-ext channel — scene kit R17 view→ext emission surface) with
  `{ pending_id, outcome: "accept" | "reject" | "edited" | "timeout" | "stale" }`.
- pi-control matches `pending_id` to the in-flight `ark_scene_propose` tool
  call's oneshot and resolves it.
- After resolve, the view reverts to Idle.

**Concurrency:**
- Only one Pending state at a time. A new `replace_view` call while Pending
  replaces the rendering — used by the `e` (edit) path — but does not
  resolve the prior pending id.

---

## Part D — Cross-cutting

### R19: Config schema

**Description:** Each crate contributes its own config schema via the
Phase 2 ext-owned config registration surface.

**Acceptance Criteria:**
- `pi-core`:
  ```toml
  [pi.core]
  bridge_install = "auto"   # "auto" | "skip"
  throttle_message_update_ms = 100
  ```
- `pi-subagents`:
  ```toml
  [pi.subagents]
  auto_install = true
  tmpdir_override = ""      # empty = default $TMPDIR
  ```
- `pi-control`:
  ```toml
  [pi.control]
  allow_ops = ["ark.core.*", "pi.*"]
  deny_ops = ["ark.core.exec"]
  continue_on_deny = false
  allow_scene_writes = false
  propose_timeout_secs = 300
  ```
- Unknown keys in a registered section warn but don't fail.
- Config hot-reloads on scene reload (policy changes visible on next
  dispatch).

### R20: Bootstrap + version handshake

**Description:** The Rust side and TS side exchange versions before the
first forward event.

**Acceptance Criteria:**
- On socket connect, TS bridge sends
  `{ dir: "fwd", kind: "bridge.hello", payload: { bridge_version, pi_version, node_version } }`.
- pi-core replies with a reverse message
  `{ dir: "rev", id: …, method: "bridge.ready", params: { ark_version, ext_ready: true } }`.
- Version mismatch between `bridge_version` and the expected version surfaces
  as a `doctor` warning but does not tear down the session. Instead,
  pi-core triggers the R1 reinstall path and asks the bridge to exit (pi
  respawns the extension on next session).

### R21: Hot reload

**Description:** pi crates respect the soul kit's ext-reload semantics.

**Acceptance Criteria:**
- `ark ext reload pi-core` terminates the bridge socket (soul R16 shutdown
  ladder), re-runs R1 install, re-binds the socket. TS bridge auto-
  reconnects (R2). A new `bridge.hello` handshake occurs. Live `pi` view
  instances receive a re-initialised bridge handle; typed `Stack<_>`/`Pane<_>`
  refs held by the view survive the bridge restart (they point at layout
  nodes, not the bridge).
- `ark ext reload pi-control` re-registers pi tools via the reverse channel
  without restarting pi. `ark_ops()` and `ark_dispatch` are updated in-place.
  Any pending `ark_scene_propose` at reload time resolves with
  `{accepted: false, reason: "reload"}`.
- `ark ext reload pi-subagents` restarts the fs watcher; in-flight subagents
  survive (source of truth is on disk). `pi-subagent-tile` / `pi-subagent-log`
  view instances survive and re-bind to the fresh ExtEvent stream.
- Scene hot-reload cascades: allowlist changes visible next dispatch; new
  ext intents appear in `ark_ops()` on next call. View-type constraints
  are re-validated; mismatched wiring surfaces as `error[scene/view-type-mismatch]`
  and the reload is rejected (old scene stays live).

### R22: Test harness

**Description:** Integration tests without a real pi binary or network.

**Acceptance Criteria:**
- A `mock-pi` binary under `crates/test-fixtures/pi/` speaks pi-bridge
  NDJSON. Scripted scenarios via env vars or arg flags:
  - emit-only (replay a canned event timeline)
  - echo-bridge (receive reverse calls, send acks)
  - tool-call-loop (register tools from ark, call them)
- Each crate's tests use `mock-pi` in place of `pi`. Two integration
  strategies:
  1. Unit-level: scene-compiled-offline tests instantiate `PiView` /
     `pi-subagent-tile` / `pi-subagent-log` / `pi-scene-propose` with mock
     `Stack<_>` / `Pane<_>` handles (per scene kit R17 test stubs) and
     exercise view logic directly.
  2. PTY-level: `test-fixtures/pi-smoke/` runs `mock-pi` under real zellij
     via a scene that uses the `pi` view; asserts event forwarding, view-
     driven stack fan-out, and propose-view pane round-trips.
- pi-subagents fs-watcher tests generate fake `async-subagent-runs/` trees
  to exercise event emission without spawning pi.
- pi-control tests instantiate `IntentRegistry` with a stub mux + bus and
  drive `ark_dispatch(kdl)` inputs; assert op dispatch sequences match
  expected outcomes under allowlist / denylist matrices.

---

## Stretch (post-MVP, captured for context)

Not required for first ship. Listed so later sessions don't re-invent:

- **Cost / token dashboard plugin** consuming `pi.turn_end` + pi's own cost
  accounting. Reuses status plugin.
- **Pre-compact findings snapshot** — on `pi.session_before_compact`, stash
  the pre-compact branch summary into a findings pane.
- **Multi-pi race** — launch N pi panes with different models on the same
  prompt; ark picks the first-completing or user-chosen winner. Uses scene
  primitives, no new ops.
- **Voice → pi → ark dispatch** — speech-to-text module feeds pi; pi uses
  `ark_dispatch` to mutate the live layout. Falls out of the existing
  surface once voice input exists.

---

## Open questions

- **Bridge TS version pin strategy.** Crate version ↔ TS asset version is
  1:1, but if a user hand-edits the TS file we overwrite on next version
  bump silently. Alternative: refuse to overwrite a file whose content hash
  doesn't match our expected + warn loudly. Decide during R1 implementation.
- **Reverse-channel method surface growth.** R5 lists 9 methods. The pi
  `ExtensionAPI` surface is much larger; we'll grow this on demand. Risk:
  no versioning on the reverse channel yet. Revisit after pi-control ships
  and we see which methods it actually calls.
- **`ark_dispatch` return-value ergonomics.** `IntentValue` currently only
  covers `{None, String, Integer, Boolean}`. Ops that want to return
  richer data (e.g. a JSON object describing a spawned pane's layout) will
  need the scene kit's deferred `Value` widening. Pi-control punts on this;
  returned values in v1 are the narrow set.
- **Allowlist UX for subagent chains.** A subagent (spawned by pi-subagents)
  may itself be a pi session with pi-control active — recursion. The
  allowlist inherits or doesn't? Default: inherits. Revisit if it bites.
- **Multiple `pi` views in one session.** Scene may declare two pi panes
  (e.g. parallel models). Each has its own typed handle wiring. R17 says
  "only one propose pending per session" — should that become "one propose
  pending per pi view"? Revisit once multi-pi is a real use case.
- **`pi-subagent-tile` rendering across zellij stack states.** Compact vs
  expanded rendering mode — zellij tells the view which state it's in via
  the stack-rendering API. Exact API surface is zellij-version-sensitive;
  verify during R11 implementation and document the zellij version we pin
  against.

---

## What this unblocks

- pi as a daily-driver engine on ark with first-class scene integration.
- The reference implementation for the Phase 2 ext-hook surface.
- A pattern for integrating future engines (copy `extensions/pi-core/` →
  `extensions/<engine>/`, swap the bridge protocol).
- User-authored pi-driven workflows: "when pi writes a file, show the diff;
  when pi spawns a subagent, open a pane tailing its log; when pi finishes
  a turn, emit a status line."
- `ark_dispatch` as the single universal control surface for any registered
  op — including ops contributed by extensions we haven't written yet.

---

## Execution

After soul Phase 2 lands:

```
/ck:sketch cavekit-pi        # decompose R1-R22 into build tasks
/ck:map                      # DAG, rough estimate 40-55 tasks, 8-10 tiers
/ck:make                     # parallel dispatch, peer-review via Codex
/ck:scan                     # verify built code matches kit
```

Suggested tier ordering (independent of `/ck:map`'s output):

1. Socket protocol + mock-pi (R2, R22) — foundation.
2. Bridge install + handshake (R1, R20).
3. Event forwarding + throttle (R3, R4).
4. Reverse-channel API + `PiHandle` (R5).
5. `pi` CommandView (R6) + fallback hook (R6b) + doctor + list columns (R7–R8).
6. pi-subagents watcher + views + intent (R9–R12).
7. pi-control discovery + dispatch (R13–R16).
8. pi-control scene-propose tool + view (R17–R18).
9. Config schemas + hot-reload wiring (R19, R21).

Each tier lands green (`cargo check --workspace --tests` + `cargo test
--workspace`); the mock-pi smoke variant gates tiers 2 onward. Tier 5 is the
first to exercise the 2026-04-18 typed-handle + stack scene revision.
