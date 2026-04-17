---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
supersedes: cavekit-architecture.md
status: ready
---

# Spec: ark's Soul — Reactive IDE on zellij, Extensions Own AI

## Scope

Re-architects ark around its actual soul: **a reactive IDE layer on top of
zellij**. Extensions — not core — own integration with AI agents, with any
specific protocol (ACP, pi.dev, OpenAI, Anthropic), and with any
methodology (Cavekit, Ralph Loop, raw passthrough).

This spec supersedes `cavekit-architecture.md`. The prior architecture
baked `Engine` + `Orchestrator` traits + `Outcome`-shaped completion
semantics into core, which was the right call for the v1 Cavekit-targeted
ark but is wrong for the long-term product.

## Motivation

Ark was built around agent workflows. The v1 design assumed every ark
session had:
- an `Engine` (an AI agent CLI like `claude`),
- an `Orchestrator` (a methodology driving the engine),
- an `Outcome` (agent-completion semantics with `{Success, Failed,
  Killed, Timeout, Crashed}`).

In practice ark IS a generic reactive IDE on zellij. The v1 agent-centric
shape surfaces as:
- `ark` (bare, no subcommand) fails on a fresh machine because the
  supervisor can't build an orchestrator for a session that has no agent.
- Every new AI integration (pi.dev, Gemini's CLI, a browser-based codex
  client) requires editing core slugs (`ENGINES_V1`, `ORCHESTRATORS_V1`)
  + adding a factory branch.
- The `Outcome` type leaks into pane-log formatting; sessions that aren't
  agents have no sensible outcome to emit.
- Extensions are second-class. They can provide scene primitives but
  can't register their own supervisor-level lifecycle.

The fix is to make extensions the integration substrate, not a decoration
on top of a hardcoded agent model.

## Soul Statement

Ark is:
- a **reactive runtime** that watches scene files + dispatches events;
- a **multiplexer binding** that renders scenes into zellij layouts;
- an **extension host** where scenes, plugins, reactions, hooks,
  keybinds, and supervisor-level subsystems all come from extensions.

Ark is NOT:
- an agent orchestrator;
- an ACP client (ACP is one extension among many);
- opinionated about what finishes a "run" (there is no run, just a
  session).

## Resolved Decisions (2026-04-17 interview)

Locked calls from kit flesh-out. Drove the migration plan + target shape
below.

- **State compat:** nuke `$STATE` on first boot after Phase 1 lands. No
  migration code. User is sole dev; state dirs are ephemeral.
- **Path leaf:** `$STATE/agents/` renames to `$STATE/sessions/`. One-time
  rename alongside type migration; no symlink bridge.
- **No `SessionKind` discriminator.** Every session is just a session.
  Behaviour comes from which extensions have been loaded / registered
  for the session. Core never branches on kind.
- **`Phase` + `Outcome` delete from core entirely.** `AgentStatus`
  becomes `SessionStatus { id, started_at, terminated_at: Option, ext_state:
  BTreeMap<String, Value> }`. Extensions write into their own bucket.
  `ark list` default = core columns only; extensions contribute columns
  via a formatter hook. `pane::log` asks the ext that owned the session
  to format its terminal line; bare session gets a generic "session
  ended".
- **Engine / Orchestrator traits delete outright in Phase 5.** No shim
  crate. No shared `ark-ext-agent` base. Each extension that wants an
  internal abstraction defines its own.
- **`ark-hook` binary moves whole to `extensions/claude-code/`.** Not
  generalised. Claude Code hook JSON is a Claude Code concern.
- **Tool taxonomy (`READ_ONLY_TOOLS`, `PermissionPolicy`) moves to
  `extensions/claude-code/`.** No generic policy engine in core. Other
  agent exts invent their own.
- **`ark doctor` baseline:** zellij + scene-dir + state-dir only.
  `check_claude` / `check_acp` / `check_status_plugin_installed` etc
  move to their respective extensions. Doctor iterates loaded
  extensions and runs each preflight.
- **Bus payload shape:** 2-level `CoreEvent` (closed, small) +
  `CoreEvent::Ext(ExtEvent { ext, kind, payload })` (open, tagged).
  Scene-script convenience: a `Into<FlatEvent>` shim flattens both into
  a uniform `{ name: String, payload: Value }` so Rhai selectors can
  match on `"ark.core.session.started"` or `"claude-code.tool.use"` the
  same way.
- **Extension loading:** use the existing `ArkExtension` trait. Phase 2
  adds `on_session_start` / `on_session_end` / `control_verbs` /
  `permission_dispatcher` methods to the trait surface (one minor bump,
  one batch). In-proc (`#[derive(Extension)]`) + subprocess (NDJSON
  JSON-RPC) transports unchanged. WASM transport remains future.
- **`crates/extensions/<name>/` layout.** No `ext-` package prefix in
  the path; binaries keep short names (`cc-hook`, `acp-client`).
  Cargo package names may still use `ext-` prefix to avoid crate-namespace
  collisions.
- **`SessionId::new(name)`** bakes the ulid in. Path becomes
  `sessions/<name>-<ulid>`. `orchestrator` slug is gone from the ID
  constructor.
- **Phase 1 is bundled.** One phase covers: SessionSpec, path rename,
  AgentEvent → CoreEvent shrink, `Phase` / `Outcome` delete from core,
  supervisor `Option<Orchestrator>`, bare launch works, PTY test
  green, `$STATE` wiped on first run.
- **Phase 1 execution via Cavekit build-site.** `/ck:sketch` →
  `/ck:map` → `/ck:make` for Phase 1's ~30 tasks; parallel agents for
  independent subsystems.
- **Hot-reload: full story.** Both transports. Subprocess exts reload
  via R16 shutdown ladder + respawn. In-proc exts require supervisor
  restart (accept the gap). Scene reload unchanged.

## Current State — What's Hardcoded

Audit summary (2026-04-17). Severity: P0 breaks the soul immediately; P1
leaks agent concepts; P2 lives in an already-scoped subcrate.

### Core offenders — P0

- `crates/core/src/engine.rs` — the `Engine` trait. Surface:
  `install_observability`, `teardown`, `default_pane_cmd`,
  `transcript_path`, `auto_approve_permissions`. All AI-agent concepts.
- `crates/core/src/orchestrator.rs` — the `Orchestrator` trait + `World`
  capability bag. `run(spec, world) -> Outcome` is an agent run-loop.
- `crates/types/src/spec.rs` — `AgentSpec` conflates session with
  agent. Fields: `orchestrator: String`, `engine: String`, `cmd:
  Vec<String>`, `runner_config: Value`.
- `crates/supervisor/src/orchestration.rs` — R3 boot sequence calls
  `build_engine` + `build_orchestrator` at step 6, `orchestrator.run`
  at step 13, `engine.teardown` at step 15. Supervisor IS the agent
  run-loop.
- `crates/supervisor/src/factory.rs` — v1-locked slug matching. Every
  new agent integration = core edit.

### Agent-specific leakage in core — P1

- `crates/types/src/event.rs:181` — `Outcome` variants assume agent
  completion (`Success { artifacts: Vec<PathBuf> }`, etc).
- `crates/supervisor/src/permission.rs` — ACP permission dispatcher.
- `crates/supervisor/src/turn_inflight.rs` — tracks ACP
  `session/prompt` for reload-gate.
- `crates/supervisor/src/engine_resolution.rs` — 5-tier chain with
  hardcoded fallback to `claude --acp`.
- `crates/acp-client/` — live crate used by supervisor. ACP is the
  integration protocol; it doesn't belong in core.

### Already in optional subcrates — P2

- `crates/orchestrators/cavekit/` — Cavekit methodology.
- `crates/orchestrators/claude-code/` — passthrough orchestrator.

These are structurally correct (in an `orchestrators/` subdir) but are
still wired as first-class trait impls via the factory rather than as
extensions.

### Adversarial-review additions — 2026-04-17

After the initial draft, an adversarial pass surfaced more agent-concept
leakage than the first audit caught. These are additions, not
corrections.

**Core type surface — P0**

- `crates/types/src/scope.rs:6-12` — `ENGINES_V1`, `ORCHESTRATORS_V1`,
  `MUX_V1` const slugs + `is_v1_*` predicates. This is where the v1
  scope lock lives; every new AI integration edits this file. Kit's
  Phase 5 retires these (keep only `MUX_V1`).
- `crates/types/src/event.rs:41-167` — `AgentEvent` enum. Ten variants
  assume agent semantics: `TaskDone`, `Iteration`, `PhaseTransition`,
  `ToolUse`, `Message`, `FileEdited`, `ReviewComment`,
  `PermissionAsked`, `PermissionResolved`, `Stall`. Only `UserEvent`,
  `Log`, `Error` are generically reactive. Phase 1 shrinks this enum
  and renames to `CoreEvent`; extension-emitted events route through
  `CoreEvent::Ext(ExtEvent)`.
- `crates/types/src/status.rs:25-38` — `Phase` enum (`Prompting`,
  `Reviewing`, `Done`, `Failed`, `Killed`, `Timeout`, `Crashed`) +
  `AgentStatus::findings: Findings { p0..p3 }`. Review phases +
  severity-bucketed finding counts are Cavekit-methodology concepts
  living in core types. Phase 1 deletes both outright; per-ext state
  moves into `SessionStatus.ext_state`.
- `crates/types/src/id.rs:32` — `AgentId::new(orchestrator, name)`.
  Phase 1 renames to `SessionId::new(name)`; ulid baked in; path
  becomes `sessions/<name>-<ulid>`.
- `crates/types/src/spec.rs:79` — `pub type OrchestratorSpec = AgentSpec`
  alias. Second re-export of the same leaky shape. Delete.
- `crates/types/src/permission.rs:42-63` — `READ_ONLY_TOOLS = ["Read",
  "Glob", "Grep", "WebFetch", "WebSearch"]` + `PermissionPolicy`
  variants + `POLICY_FILE_NAME = "permission_policy"`. Claude Code tool
  taxonomy hardcoded in `ark-types`. Phase 4 moves to
  `extensions/claude-code/`.

**Core consumers — P0**

- `crates/core/src/consumers/state_writer.rs:179-260` — `update_status`
  hardcodes phase-rollup rules keyed on each agent-specific
  `AgentEvent` variant. Shrinks with `AgentEvent`. Phase 1 rewrites
  against `CoreEvent`; per-ext rollup logic moves with its
  extension.
- `crates/core/src/consumers/reaction_dispatcher.rs:372-390` — the
  reaction dispatcher has `OpNode::AcpPrompt / AcpCancel / AcpPermit /
  AcpSetMode` match arms *inside core*. ACP ops are dispatched from
  core, not from `acp-client`. Kit includes the reaction dispatcher
  in the ACP-extraction phase (Phase 3) via open-dispatch (trait-object
  op nodes or ext-registered op kinds).

**Scene crate — P0 (entire subtrees that are pure ACP)**

- `crates/scene/src/ext/acp.rs`
- `crates/scene/src/ext/permission.rs`
- `crates/scene/src/ext/inflight.rs`
- `crates/scene/src/ext/doctor.rs`
- `crates/scene/src/ops/acp.rs`
- `crates/scene/src/engine_compat.rs`
- `crates/scene/src/intent.rs:172-300` — `AcpHandle` trait on
  `IntentContext`; `ctx.acp: Option<Arc<dyn AcpHandle>>`. ACP is
  hardwired into the scene intent-dispatch context. Phase 3 replaces
  with ext-registered intent handlers; `IntentContext` loses `acp`
  field.
- `crates/scene/src/context.rs` — `AgentSnapshot` used as the Rhai
  event-scope's `agent.*` binding. Phase 1 generalises to
  `SessionSnapshot` with an `extensions: Map<String, Value>` sub-map
  for per-extension state. Scene scripts migrate `agent.*` →
  `session.*` / `session.extensions.<name>.*`.

**Supervisor — P0 additions**

- `crates/supervisor/src/scene_runtime.rs:94-99,182-189` —
  `CompiledScene.engine_launch: Option<EngineLaunch>` + the
  `with_engine_launch` builder. Phase 1 removes the field; extensions
  that want to launch agent subprocesses on scene compile register a
  scene-compile hook (Phase 2) and do it themselves.
- `crates/supervisor/src/auto_close.rs:60-87` — `AutoClosePolicy {
  on_done, on_fail, on_kill }` + `should_close(outcome)`. Consumes
  `Outcome`. Phase 1 rewrites against session-lifecycle events; ext
  hooks can register their own close conditions (Phase 2). Bare
  sessions default to no-auto-close.
- `crates/supervisor/src/commands.rs:366-395` — `Kill` command accepts
  `remove_worktree: bool`. Worktrees are methodology-level, not
  generic session control. Phase 4 moves worktree cleanup into
  `extensions/cavekit/` via a kill-time hook.
- `crates/supervisor/src/kill.rs:166-170` — always emits `AgentEvent::
  Done { Outcome::Killed }` at kill time. Phase 1 changes to emit
  `CoreEvent::SessionEnded { terminated_at }`; extensions that want
  "killed" semantics observe the event and emit their own
  `ExtEvent`.

**Config, hooks, CLI — P1**

- `crates/config/src/schema.rs` — entire TOML schema is agent-aware:
  `[defaults].orchestrator / .engine / .auto_close_on_{done,fail,kill}
  / .stall_timeout_secs`; `[engine.claude_code]` with `transcript_tail
  / permission_policy / inject_hooks`; `[orchestrator.cavekit]` with
  `watch_ralph_loop / spawn_review_tab / review_on_phase`;
  `[orchestrator.claude_code]`; `[acp].permission_timeout_ms`;
  `[engines.<name>]` named-map section. Phase 4 moves ext-owned
  config sections to their extensions (extensions register their own
  config schemas via the ext-proto surface).
- `crates/config/src/hooks.rs:67-98` — `HookEntry.on_orchestrator:
  Vec<String>` + `HookContext.orchestrator: String`. Phase 4 migrates
  to `on_extension: Vec<String>`.
- `crates/cli/src/commands/list.rs:52-141,282-289` — `--orchestrator`
  filter flag, `PHASE_NAMES` hardcoded, detail view prints
  `orchestrator:` + `engine:` fields. Phase 1 drops the hardcoded
  columns; exts contribute columns via formatter hooks (Phase 2).
- `crates/cli/src/commands/doctor.rs:241-372,668-859` — `check_claude`,
  `check_acp`, `check_status_plugin_installed`,
  `check_picker_plugin_installed`, `check_dangling_worktrees`. Phase 4
  moves each check to its owning extension; doctor iterates loaded
  extensions and runs each preflight.
- `crates/cli/src/id_resolver.rs:6-11,126-187` — resolve-by-name reads
  `spec.json` as `{ name }`. Phase 1 keeps `name` readable from
  `SessionSpec`.

**ark-hook crate — P0 (flagged Claude-specific in its entirety)**

- `crates/hook/src/event.rs:21-35` — `HookEvent` enum's six variants
  (`PostToolUse`, `Stop`, `PermissionRequest`, `Notification`,
  `SessionEnd`, `TaskCompleted`) are verbatim Claude Code hook names.
- `crates/hook/src/payload.rs:117-133` — `payload_to_events` hardcodes
  translation to `ToolUse`, `Done { Outcome::Success }`,
  `PermissionAsked`, `Message`, `TaskDone`.
- The `ark-hook` binary is Claude-Code glue, not generic
  event-bridging. Phase 4 moves inside `extensions/claude-code/` as
  `cc-hook` binary.

**Core-resident contract suites — P2**

- `crates/core/src/engine_contract.rs` + `crates/core/src/orchestrator_contract.rs`
  — reusable trait-conformance test suites exported from `ark-core`.
  Delete with the traits in Phase 5.

## Target Architecture

### Layer 1: Core runtime (stays in core)

1. **Scene** — KDL parsing, shape detection, compile, layout lowering,
   reactions, keybinds, plugin decls, hot-reload. (Already clean.)
2. **Mux** — zellij wrapper. Session / tab lifecycle. Layout artifacts.
   (Already clean.)
3. **Hook IPC** — event taxonomy + per-supervisor control socket.
   (Claude Code hook binary moves to extension.)
4. **Supervisor reactive loop** — what remains after Engine/Orchestrator
   leave:
   - Lock, control-socket bind, scene compile, plugin lifecycle,
     reaction dispatcher, hot-reload watcher.
   - Main loop: `world.cancel.cancelled().await`. Nothing else.
   - No `orchestrator.run`. No `engine.teardown`. No factory slug
     matching beyond `mux`.
5. **Extension host** — the only way to add AI-side functionality. Uses
   existing `ArkExtension` trait + in-proc / subprocess transports;
   Phase 2 adds new supervisor-side methods.

### Layer 2: SessionSpec + SessionStatus + SessionId

Replaces `AgentSpec` / `AgentStatus` / `AgentId`:

```rust
// crates/types/src/spec.rs
struct SessionSpec {
    id: SessionId,
    name: String,                // human-friendly, unique within state dir
    scene_path: Option<PathBuf>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    created_at: DateTime<Utc>,
    // Extensions serialize their per-session config into this map
    // under their ext name. Core never reads these fields.
    ext_config: BTreeMap<String, serde_json::Value>,
}

// crates/types/src/id.rs
struct SessionId { name: String, ulid: Ulid }
impl SessionId {
    pub fn new(name: &str) -> Self { /* ulid baked in */ }
    pub fn as_path_leaf(&self) -> String { format!("{}-{}", self.name, self.ulid) }
}

// crates/types/src/status.rs
struct SessionStatus {
    id: SessionId,
    started_at: DateTime<Utc>,
    terminated_at: Option<DateTime<Utc>>,
    // Per-extension state. Each ext writes into its own bucket.
    // `ark list` shows only core columns by default; exts contribute
    // columns via a formatter hook (Phase 2).
    ext_state: BTreeMap<String, serde_json::Value>,
}
```

`AgentSpec` survives inside extensions that care about it — e.g.,
`extensions/acp-client/src/agent_spec.rs`. The picker UI (when it comes
back) operates on whatever "spawnable things" an extension exposes.

### Layer 3: Bus payload — CoreEvent + ExtEvent (2-level)

```rust
// crates/types/src/event.rs
enum CoreEvent {
    Log { level, message, target },
    Error { error },
    SessionStarted { spec: SessionSpec },
    SessionEnded { terminated_at: DateTime<Utc> },
    Ext(ExtEvent),
}

struct ExtEvent {
    ext: String,             // e.g. "acp-client", "claude-code"
    kind: String,            // e.g. "permission.asked", "tool.use"
    payload: serde_json::Value,
}

// Convenience shim for scene scripts — both core and ext events
// render as { name, payload } so Rhai selectors match uniformly.
struct FlatEvent { name: String, payload: serde_json::Value }
impl From<&CoreEvent> for FlatEvent { /* core events → "ark.core.*" */ }
impl From<&ExtEvent>  for FlatEvent { /* "<ext>.<kind>" */ }
```

Scene script ergonomics:

```kdl
on "ark.core.session.started" { ... }
on "claude-code.tool.use" where="payload.tool == \"Read\"" { ... }
on "acp-client.permission.asked" { ... }
```

The `on <CamelCaseVariant>` form (closed-enum pattern-match) still
works for core variants; for ext events, the string form is the only
option.

### Layer 4: Extensions own AI

Extensions can register:
- **Scene-side (existing):** reactions, keybinds, plugin decls, intents,
  event subscriptions with glob selectors.
- **Supervisor-side (new, Phase 2):**
  - `on_session_start(&self, spec: &SessionSpec) -> Result<()>`
  - `on_session_end(&self, id: &SessionId, reason: ExitReason)`
  - `control_verbs() -> Vec<VerbSpec>` (custom `ark ext foo intent`
    surfaces)
  - `permission_dispatcher() -> Option<Arc<dyn PermissionDispatcher>>`
  - `scene_compile_hook(&self, compiled: &mut CompiledScene) -> Result<()>`
    (lets e.g. `extensions/acp-client/` attach its own engine-launch
    metadata)
  - `doctor_checks() -> Vec<CheckSpec>` (fanned into `ark doctor`)
  - `list_columns() -> Vec<ColumnSpec>` (contributed to `ark list`)
- **Pane-side (existing):** pane views.

Example extensions the new architecture supports:

- `extensions/acp-client/` — ACP subprocess lifecycle, `session/prompt`
  tracking, permission dispatch. Replaces `crates/acp-client/` + the
  supervisor's `permission.rs` + `turn_inflight.rs`.
- `extensions/claude-code/` — Claude Code hook glue (`cc-hook`
  binary), transcript watching, permission-policy taxonomy, git diff
  artifact collection. Replaces `crates/orchestrators/claude-code/` +
  absorbs `crates/hook/` + `crates/types/src/permission.rs`.
- `extensions/cavekit/` — build-site progress tracking, findings feed,
  worktree-aware kill. Replaces `crates/orchestrators/cavekit/`.
- `extensions/pi` (future) — pi.dev native integration.
- `extensions/subagents-on-pi` (future) — meta-extension built on
  `extensions/pi`, coordinating multiple subagents.

### Hot-reload

Scene reload: unchanged (already clean).

Extension reload:
- **Subprocess extensions** reload live via R16 shutdown ladder
  (stdin-close → SIGTERM → SIGKILL) + respawn. Supervisor exposes
  `ark ext reload <name>`; under the hood this issues R16 shutdown
  then `on_session_start` for every live session that registered with
  the ext.
- **In-proc extensions** (`#[derive(Extension)]` + `inventory::submit!`)
  require supervisor restart. Accept the gap. Document in the ext
  authoring guide.
- **Ext manifest hot-swap** (bumping `extensions/<name>/manifest.kdl`):
  same rules. Subprocess → restart the subprocess; in-proc → supervisor
  restart.

### What stays the same

- Scene KDL format. ACP-specific scene extensions move to
  `extensions/acp-client/` but the *grammar* is unchanged.
- Every reaction / keybind / plugin mechanism.
- zellij as the substrate. The web client is zellij's, not ark's.
- `ark list` / `ark kill` / `ark scene *` / `ark pane *` /
  `ark config *` — bare-ark session management is unchanged
  user-facing. Internal shape of `AgentStatus` → `SessionStatus`
  changes but the user view stays plus ext-contributed columns.
- `ark doctor` keeps its verb. Its *checks* become extension-supplied.
- The `ArkExtension` RPC surface (34 methods today). Phase 2 adds
  methods; doesn't rip any.

### What explicitly moves out of core

Explicit call-outs beyond the obvious trait moves:

- `crates/hook/` — Claude Code glue. Moves to
  `extensions/claude-code/bin/cc-hook/`.
- `crates/types/src/permission.rs` — Claude Code tool taxonomy. Moves
  to `extensions/claude-code/`.
- `crates/scene/src/ext/{acp,permission,inflight,doctor}.rs` +
  `crates/scene/src/ops/acp.rs` + `crates/scene/src/engine_compat.rs`
  — move to `extensions/acp-client/`, which registers these scene-side
  primitives via the new supervisor-extension hooks (Phase 3).
- `crates/scene/src/intent.rs:AcpHandle` — replaced by extensions
  registering their own intent handlers. `IntentContext` no longer has
  an `acp` field.
- `crates/scene/src/context.rs:AgentSnapshot` — generalises to
  `SessionSnapshot` with an `extensions: Map<String, Value>`
  sub-map.
- `crates/orchestrators/cavekit/` → `extensions/cavekit/`.
- `crates/orchestrators/claude-code/` merges into
  `extensions/claude-code/`.
- `crates/acp-client/` → `extensions/acp-client/`.

## Migration Path

Six phases. Each independently shippable; each leaves the workspace green.

### Phase 1: Types + supervisor + launch unblock (bundled)

Single phase covering all the type surgery plus supervisor refactor
required to make bare `ark` launch succeed.

**Sub-areas** (drive the Cavekit build-site decomposition):

1. **Types migration** (`crates/types/`):
   - `AgentSpec` → `SessionSpec`. `AgentStatus` → `SessionStatus`.
     `AgentId::new(orch, name)` → `SessionId::new(name)` (ulid baked in).
   - `AgentEvent` → `CoreEvent` with shrunk variant list + new
     `Ext(ExtEvent)` variant.
   - Delete `Phase`, `Outcome`, `Findings` from `ark-types`.
   - Delete `ENGINES_V1` / `ORCHESTRATORS_V1` slug consts (keep
     `MUX_V1`).
2. **State layout:**
   - Rename `StateLayout::agents*()` → `sessions*()`. Path leaf
     `agents/` → `sessions/`.
   - On supervisor boot, if `$STATE/agents/` exists, delete it
     (nuke). Log a single INFO line.
3. **Supervisor refactor** (`crates/supervisor/`):
   - `run_supervisor_with` takes `orchestrator: Option<Box<dyn
     Orchestrator>>`. `None` path skips steps 13 + 15; main loop is
     `world.cancel.cancelled().await`.
   - `engine: Option<Box<dyn Engine>>` similarly.
   - `auto_close.rs` rewritten against `CoreEvent::SessionEnded`;
     bare sessions default no-auto-close.
   - `kill.rs` emits `CoreEvent::SessionEnded`, not a synthesised
     `Outcome::Killed`.
   - `scene_runtime.rs` — `CompiledScene.engine_launch` deleted.
     Replaced in Phase 2 by scene-compile hook on extensions.
4. **CLI/consumer updates:**
   - `crates/cli/src/commands/list.rs` — drop `--orchestrator` flag +
     `PHASE_NAMES` const + orchestrator/engine/phase columns. Minimal
     row: `id name cwd uptime running?`.
   - `crates/cli/src/id_resolver.rs` — reads `name` from `SessionSpec`.
   - `crates/core/src/consumers/state_writer.rs` — rewrites against
     `CoreEvent`. Per-ext rollup logic not yet present; scaffolded
     stub for Phase 2.
   - `crates/core/src/consumers/reaction_dispatcher.rs` — `OpNode::Acp*`
     kept temporarily (deleted in Phase 3) but no longer trigger
     `engine_compat`.
5. **Bare launch path:**
   - `crates/cli/src/commands/launch.rs` constructs a
     `SessionSpec { name: "ark", scene: default, … }` and calls
     `run_supervisor_with(spec, None, None, world)`.
   - The launch-module trait surface (`Multiplexer`,
     `SupervisorSpawner`) stays; `real.rs` drops the orchestrator
     factory call.
6. **Test posture:**
   - PTY smoke test `real_zellij_accepts_compiled_default_layout`
     goes green.
   - Existing `launch_integration.rs` mock tests keep passing.
   - `crates/orchestrators/cavekit/` + `claude-code/` keep compiling
     against the new types (`Option<Orchestrator>` path = `Some(…)`
     for these; they survive Phase 1 unchanged behaviour-wise).

**Out of scope for Phase 1:** extension-registered session-lifecycle
hooks, ACP extraction, claude-code extraction, doctor refactor,
picker. All deferred.

**Execution:** Cavekit build-site. `/ck:sketch` the Phase 1 kit from
this spec's Phase 1 sub-areas, `/ck:map` to generate a build graph
(rough estimate: 25-35 tasks across 6-8 tiers), `/ck:make` to dispatch.
Peer-review via Codex.

### Phase 2: Extension supervisor hooks

- Extend the `ArkExtension` trait with the new methods
  (`on_session_start`, `on_session_end`, `control_verbs`,
  `permission_dispatcher`, `scene_compile_hook`, `doctor_checks`,
  `list_columns`). Single minor version bump.
- `ark-ext-proto::ArkExtensionMetadata` surface grows; derive macro
  (`ark-ext-derive`) picks up the new arms.
- `ark list` columns + `ark doctor` checks become ext-fan-in.
- Integration tests with a stub in-proc extension.

### Phase 3: ACP → `extensions/acp-client/`

- Move `crates/acp-client/` + `crates/supervisor/src/permission.rs` +
  `crates/supervisor/src/turn_inflight.rs` + scene `ext/{acp,permission,inflight,doctor}.rs`
  + `ops/acp.rs` + `engine_compat.rs` + `intent.rs:AcpHandle` into
  `extensions/acp-client/`.
- Supervisor no longer depends on `acp-client`.
- `reaction_dispatcher::OpNode::Acp*` variants removed; dispatcher
  gets open-op-kind dispatch (ext-registered op handler trait).
- `engine_resolution.rs` chain moves to `extensions/acp-client/`.
- `ark doctor` uses ext-fan-in from Phase 2 for `check_acp`,
  `check_claude`, etc.

### Phase 4: Claude Code + Cavekit → extensions

- `crates/orchestrators/claude-code/` + `crates/hook/` (as `cc-hook`
  binary) + `crates/types/src/permission.rs` (tool taxonomy) +
  config's `[engine.claude_code]` / `[orchestrator.claude_code]`
  schema sections merge into `extensions/claude-code/`.
- `crates/orchestrators/cavekit/` → `extensions/cavekit/`. Worktree
  cleanup (`Kill { remove_worktree }`) migrates to a cavekit-ext
  kill-time hook.
- `HookEntry.on_orchestrator` → `on_extension`.
- Config sections per-extension — extensions register their own
  schemas via the ext-proto surface.

### Phase 5: Delete `Engine` / `Orchestrator` from core

- Delete `crates/core/src/engine.rs`, `orchestrator.rs`,
  `engine_contract.rs`, `orchestrator_contract.rs`, re-exports at
  `lib.rs:41-45`.
- Delete `crates/supervisor/src/factory.rs` (only `build_multiplexer`
  was still legitimate — fold into `crates/mux/`).
- Delete `crates/supervisor/src/engine_resolution.rs` leftovers.
- Delete `crates/types/src/spec.rs:OrchestratorSpec` alias.
- Since Phase 1 made `Option<Orchestrator>` the supervisor's shape and
  Phases 3-4 moved all Some(…) call-sites into extensions, Phase 5 is
  mostly deletions + a final compile check.

### Phase 6: Picker spawn via extensions

- Picker gets its spawn capability back, but it asks loaded extensions
  for "spawnable things". Each extension that provides spawnable
  things (agents, workflows, demos) registers a picker surface via
  the ext-proto + returns spawn specs. Picker UI is generic; semantics
  per-extension.
- `ark spawn` CLI verb does not return. Use `ark ext <name> spawn …`
  or equivalent ext-specific commands.

## Migration blind spots (still-live risks)

Most Phase 1 blind spots from the initial draft are resolved by the
bundle + nuke-state decisions. These remain:

### Phase 2
- `ark-ext-proto::ArkExtension` is method-per-op under a version policy
  (R16 rule #3). Batch all new methods into one minor bump. Don't drip
  them.
- Existing in-proc extensions (`#[derive(Extension)]` users) auto-get
  default impls for new methods. Subprocess extensions need to handle
  `method not found` gracefully for older ext versions.

### Phase 3
- `reaction_dispatcher::OpNode::Acp*` needs exhaustiveness break.
  Introduce an open-dispatch pattern (trait-object op nodes OR
  `OpNode::Ext { ext, kind, args: Value }`) before moving the ACP
  variants.
- `CoreEvent::Ext` payload matching in Rhai — need to confirm Rhai's
  dynamic map inspection is ergonomic enough to replace the typed
  `PermissionAsked { … }` short form. May need a helper binding
  (`event.matches("permission.asked")`) in the scripting context.

### Phase 4
- `HookEntry.on_orchestrator` → `on_extension` is a config schema
  break. User config files need tolerant parsing + a one-time rewrite
  on read (or, since state gets nuked, bless-and-forget for the first
  migration boot).

### Phase 6
- Picker control-socket call sites (`crates/plugins/picker/`) parse
  `AgentStatus` via the NDJSON envelope. Phase 1 hits them;
  Phase 6 re-uses the surface. Audit the parsing at Phase 1 time.

## Open Questions

Narrowed considerably; remaining:

- **Rhai ergonomics for open-namespace event selectors.** Concrete
  goal: `on "claude-code.permission.asked" where="payload.tool ==
  \"Read\"" { … }` must be as ergonomic as today's `on
  PermissionAsked { … }`. Verify during Phase 1 sub-task "event bus
  rework" — if Rhai's indexing is painful, add a `event.matches(glob)`
  + `event.payload` binding.
- **`ark-ext-proto` scope.** Today it's mostly scene-side metadata.
  Phase 2 adds supervisor-side registration. No plans to absorb the
  old Engine/Orchestrator traits — they're deleted, not rehomed. But
  any shared "session object" type that extensions need to reference
  (e.g., `SessionSnapshot`) likely moves from `ark-types` into
  `ark-ext-proto`. Decide during Phase 2.
- **Auto-close defaults for bare sessions.** Phase 1 defaults to
  no-auto-close for bare. Confirm this matches the intended UX once
  the `ark list` + `ark kill` flow is re-tested by hand.

## Non-goals for this spec

- No wire-format redesign for hooks or scenes.
- No zellij refactor. The web client story comes from zellij, not
  ark. Zellij stacks are a separate concern tracked elsewhere.
- No new CLI verbs. Bare `ark` + existing subcommands cover it.
- No cross-platform story for Windows — unix-only stays unix-only.
- No WASM extension transport work — protocol allows it, impl
  deferred.

## Explicitly in-core infrastructure (audit confirmed)

The adversarial pass flagged these as correctly core; calling them out
so a reader doesn't over-extract them:

- `crates/types/src/event_bus.rs` — `EventSink` / `EventReceiver`
  infrastructure. The *channel* is core; the `CoreEvent` *payload* is
  what shrinks (+ gains `Ext(ExtEvent)`).
- `crates/scene/src/*` compile pipeline (parse / shape / compose /
  compile / layout lowering) — scene grammar minus the ACP-specific
  scene extensions.
- `crates/mux/zellij/*` — substrate.
- `crates/ark-ext-*` + `crates/ark-ext-derive/*` + `crates/ark-ext-proto/*`
  — the extension framework itself. Phase 2 grows the trait surface
  but the crates stay.
- `crates/config/src/*` TOML plumbing — the parser stays; agent-aware
  section schemas shrink.
- `crates/core/src/control_socket.rs` — NDJSON IPC envelope is pure
  plumbing.
- `crates/core/src/events_log.rs` + `status_writer.rs` + `socket_paths.rs`
  — state-on-disk primitives.
- `crates/types/src/state_dir.rs` — path layout. Phase 1 renames
  `agents/` → `sessions/`; the module stays.

## What this unblocks

- The "pi.dev extension with subagents" vision (on top of zellij
  stacks, handled separately).
- Third-party AI CLI integrations without core edits.
- Bare `ark` as a legitimate first-class use (launching zellij with a
  scene, no agent attached — the "reactive IDE" experience).
- Extensions that are pure-UI (a scene-based diff viewer, a tab
  navigator) without pretending to be orchestrators.

## Execution

Phase 1 runs via Cavekit:

```
/ck:sketch cavekit-soul-phase-1   # decompose Phase 1 sub-areas into R-numbered kits
/ck:map                           # build the task DAG (~25-35 tasks, ~6-8 tiers)
/ck:make                          # dispatch parallel agents, peer-review via Codex
/ck:scan                          # verify built code matches kits
```

Phases 2-6 get their own sketch+map+make cycles. Each phase lands with:

- Workspace green (`cargo check --workspace --tests` + `cargo test
  --workspace`).
- The PTY smoke test passing (from Phase 1 onward).
- No residual `#[deprecated]` / `TODO(cavekit-soul)` markers past the
  phase that was meant to fix them.

Handoff commits follow session discipline:
1. Types/layout rename commit (no behaviour change).
2. Supervisor-loop refactor commit (types already migrated).
3. Bare-launch wiring + PTY test commit (the green).
