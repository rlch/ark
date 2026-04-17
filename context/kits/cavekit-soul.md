---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
supersedes: cavekit-architecture.md
status: draft
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
  Phase 5 must retire these (keep only `MUX_V1`).
- `crates/types/src/event.rs:41-167` — `AgentEvent` enum. Ten variants
  assume agent semantics: `TaskDone`, `Iteration`, `PhaseTransition`,
  `ToolUse`, `Message`, `FileEdited`, `ReviewComment`,
  `PermissionAsked`, `PermissionResolved`, `Stall`. Only `UserEvent`,
  `Log`, `Error` are generically reactive. Extensions should define
  their own event types and route into the bus as `UserEvent`s.
- `crates/types/src/status.rs:25-38` — `Phase` enum (`Prompting`,
  `Reviewing`, `Done`, `Failed`, `Killed`, `Timeout`, `Crashed`) +
  `AgentStatus::findings: Findings { p0..p3 }`. Review phases +
  severity-bucketed finding counts are Cavekit-methodology concepts
  living in core types.
- `crates/types/src/id.rs:32` — `AgentId::new(orchestrator, name)`.
  The ID constructor REQUIRES an orchestrator slug. Every
  `$STATE/agents/<orchestrator>-<name>-<ulid>` path has orchestrator
  baked in, which means `SessionSpec` can't just drop the orchestrator
  field without a parallel ID-namespace migration.
- `crates/types/src/spec.rs:79` — `pub type OrchestratorSpec = AgentSpec`
  alias. Second re-export of the same leaky shape.
- `crates/types/src/permission.rs:42-63` — `READ_ONLY_TOOLS = ["Read",
  "Glob", "Grep", "WebFetch", "WebSearch"]` + `PermissionPolicy`
  variants + `POLICY_FILE_NAME = "permission_policy"`. Claude Code tool
  taxonomy hardcoded in `ark-types`.

**Core consumers — P0**

- `crates/core/src/consumers/state_writer.rs:179-260` — `update_status`
  hardcodes phase-rollup rules keyed on each agent-specific
  `AgentEvent` variant (`TabOpened → Running`, `ToolUse → Running`,
  `Message → Running`, `PhaseTransition::to == "done" → Done`). Shrinks
  with `AgentEvent`.
- `crates/core/src/consumers/reaction_dispatcher.rs:372-390` — the
  reaction dispatcher has `OpNode::AcpPrompt / AcpCancel / AcpPermit /
  AcpSetMode` match arms *inside core*. ACP ops are dispatched from
  core, not from `acp-client`. Kit must include the reaction
  dispatcher in the ACP-extraction phase.

**Scene crate — P0 (entire subtrees that are pure ACP)**

- `crates/scene/src/ext/acp.rs`
- `crates/scene/src/ext/permission.rs`
- `crates/scene/src/ext/inflight.rs`
- `crates/scene/src/ext/doctor.rs`
- `crates/scene/src/ops/acp.rs`
- `crates/scene/src/engine_compat.rs`
- `crates/scene/src/intent.rs:172-300` — `AcpHandle` trait on
  `IntentContext`; `ctx.acp: Option<Arc<dyn AcpHandle>>`. ACP is
  hardwired into the scene intent-dispatch context.
- `crates/scene/src/context.rs` — `AgentSnapshot` used as the Rhai
  event-scope's `agent.*` binding. The scripting language's primary
  context object is agent-shaped, not session-shaped.

**Supervisor — P0 additions**

- `crates/supervisor/src/scene_runtime.rs:94-99,182-189` —
  `CompiledScene.engine_launch: Option<EngineLaunch>` + the
  `with_engine_launch` builder. The scene-compile output carries an
  engine launch spec. This is the structural leak in the scene
  runtime, not in the scene *crate* — move `engine_launch` out of
  `CompiledScene`.
- `crates/supervisor/src/auto_close.rs:60-87` — `AutoClosePolicy {
  on_done, on_fail, on_kill }` + `should_close(outcome)`. Pure
  outcome-lifecycle residue that the kit's initial audit missed.
- `crates/supervisor/src/commands.rs:366-395` — `Kill` command accepts
  `remove_worktree: bool`. Worktrees are methodology-level, not
  generic session control.
- `crates/supervisor/src/kill.rs:166-170` — always emits `AgentEvent::
  Done { Outcome::Killed }` at kill time, even for sessions that
  weren't agents. Under bare-ark this synthesizes a fake "done" event.

**Config, hooks, CLI — P1**

- `crates/config/src/schema.rs` — entire TOML schema is agent-aware:
  `[defaults].orchestrator / .engine / .auto_close_on_{done,fail,kill}
  / .stall_timeout_secs`; `[engine.claude_code]` with `transcript_tail
  / permission_policy / inject_hooks`; `[orchestrator.cavekit]` with
  `watch_ralph_loop / spawn_review_tab / review_on_phase`;
  `[orchestrator.claude_code]`; `[acp].permission_timeout_ms`;
  `[engines.<name>]` named-map section. Ext-owned config inverts the
  section-naming convention.
- `crates/config/src/hooks.rs:67-98` — `HookEntry.on_orchestrator:
  Vec<String>` + `HookContext.orchestrator: String`. Hook filtering
  keyed on orchestrator.
- `crates/cli/src/commands/list.rs:52-141,282-289` — `--orchestrator`
  filter flag, `PHASE_NAMES` hardcoded (`prompting`, `reviewing`,
  etc), detail view prints `orchestrator:` + `engine:` fields.
- `crates/cli/src/commands/doctor.rs:241-372,668-859` — `check_claude`,
  `check_acp`, `check_status_plugin_installed`,
  `check_picker_plugin_installed`, `check_dangling_worktrees`.
  Agent-methodology-specific preflight baked into the `ark doctor`
  verb. Doctor should ask each loaded extension for its own preflight
  checks rather than hardcoding these.
- `crates/cli/src/id_resolver.rs:6-11,126-187` — resolve-by-name reads
  `spec.json` as `{ name }`. Any `SessionSpec` migration must keep
  `name` readable from legacy `spec.json`.

**ark-hook crate — P0 (flagged Claude-specific in its entirety)**

- `crates/hook/src/event.rs:21-35` — `HookEvent` enum's six variants
  (`PostToolUse`, `Stop`, `PermissionRequest`, `Notification`,
  `SessionEnd`, `TaskCompleted`) are verbatim Claude Code hook names.
- `crates/hook/src/payload.rs:117-133` — `payload_to_events` hardcodes
  translation to `ToolUse`, `Done { Outcome::Success }`,
  `PermissionAsked`, `Message`, `TaskDone`.
- The `ark-hook` binary is Claude-Code glue, not generic
  event-bridging. It belongs inside `ext-claude-code`.

**Core-resident contract suites — P2**

- `crates/core/src/engine_contract.rs` + `crates/core/src/orchestrator_contract.rs`
  — reusable trait-conformance test suites exported from `ark-core`.
  Retire with the traits they test.

## Target Architecture

### Layer 1: Core runtime (stays in core)

1. **Scene** — KDL parsing, shape detection, compile, layout lowering,
   reactions, keybinds, plugin decls, hot-reload. (Already clean.)
2. **Mux** — zellij wrapper. Session / tab lifecycle. Layout artifacts.
   (Already clean.)
3. **Hook IPC** — event taxonomy + `ark-hook` CLI +
   per-supervisor control socket. (Already clean.)
4. **Supervisor reactive loop** — what remains after Engine/Orchestrator
   leave:
   - Lock, control-socket bind, scene compile, plugin lifecycle,
     reaction dispatcher, hot-reload watcher.
   - Main loop: `world.cancel.cancelled().await`. Nothing else.
   - No `orchestrator.run`. No `engine.teardown`. No factory slug
     matching beyond `mux`.
5. **Extension host** — the only way to add AI-side functionality.

### Layer 2: Sessions vs agents

Separate `SessionSpec` from `AgentSpec`.

```rust
// Replaces AgentSpec for the supervisor's perspective.
struct SessionSpec {
    id: SessionId,          // renamed from AgentId
    session: String,        // zellij session name (for 1:1 binding)
    scene_path: Option<PathBuf>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    created_at: DateTime<Utc>,
    // No orchestrator, no engine, no cmd, no runner_config.
    // Extensions that need these carry them in their own state.
}
```

`AgentSpec` survives inside the agent extension(s) that care about it.
The picker UI (when it comes back) operates on whatever "things you can
spawn" an extension exposes — which might be agents, or workflows, or
demos, or none at all.

### Layer 3: Extensions own AI

Extensions can register:
- **Scene-side:** reactions, keybinds, plugin decls (existing).
- **Supervisor-side (new):**
  - Long-running tasks spawned at scene compile time.
  - Control-socket command handlers (custom `ark ext foo intent` verbs).
  - Hook providers (custom hook names).
  - Session-lifecycle listeners (on-session-start / on-session-end).
  - Permission dispatchers (the ACP extension supplies ACP's).
- **Pane-side:** views (existing).

Example extensions the new architecture supports:

- `ext-acp-client` — ACP subprocess lifecycle, `session/prompt`
  tracking, permission dispatch. Replaces core's `acp-client` + the
  supervisor's `permission.rs` + `turn_inflight.rs`.
- `ext-claude-code` — Cavekit-style reactions, transcript watching, git
  diff artifact collection. Replaces
  `crates/orchestrators/claude-code/`.
- `ext-cavekit` — build-site progress tracking, findings feed.
  Replaces `crates/orchestrators/cavekit/`.
- `ext-pi` (future) — pi.dev native integration. Plugins that live in
  zellij panes speak to pi via the extension's supervisor-side task.
- `ext-subagents-on-pi` (future) — a meta-extension built on top of
  `ext-pi`, coordinating multiple subagents through scene-level
  "stacks" (new scene primitive TBD).

### What stays the same

- Scene KDL format (the grammar — ACP-specific scene extensions move
  to `ext-acp-client`).
- Every reaction / keybind / plugin mechanism.
- zellij as the substrate. The web client is zellij's, not ark's.
- `ark list` / `ark kill` / `ark scene *` / `ark pane *` /
  `ark config *` — bare-ark session management is unchanged
  user-facing (the internal shape of `AgentStatus` changes, but the
  user view stays).
- `ark doctor` keeps its verb. Its *checks* become extension-supplied:
  each loaded extension exposes a preflight list, doctor fans out.

### What explicitly moves out of core

Explicit call-outs beyond the obvious trait moves:

- `crates/hook/` — Claude Code glue. Moves to `ext-claude-code`.
- `crates/types/src/permission.rs` — Claude Code tool taxonomy. Moves
  to `ext-claude-code` (or a shared agent-ext base).
- `crates/scene/src/ext/{acp,permission,inflight,doctor}.rs` +
  `crates/scene/src/ops/acp.rs` + `crates/scene/src/engine_compat.rs`
  — move to `ext-acp-client`, which registers these scene-side
  primitives via the new supervisor-extension hook (Phase 3).
- `crates/scene/src/intent.rs:AcpHandle` — replaced by extensions
  registering their own intent handlers. The `IntentContext` no
  longer has an `acp` field.
- `crates/scene/src/context.rs:AgentSnapshot` — generalises to
  `SessionSnapshot` with an extensions sub-map for per-extension
  state.

## Migration Path

Phased. Each phase is independently shippable, and each leaves the
workspace green. No phase is a "rewrite everything" tier.

### Phase 0: Unblock bare ark (minimum viable patch, next session)

- Add an `"ark"` null-orchestrator + null-engine combo as a temporary
  stopgap, with a TODO pointing at this spec. Factory matches "ark"
  slugs, supervisor boot completes, bare `ark` launches. The
  PTY smoke test goes green.
- Explicitly mark as temporary scaffolding that Phase 1+ removes.

### Phase 1: Supervisor main loop without orchestrator.run

- Add `Option<Box<dyn Orchestrator>>` to `run_supervisor_with`; if
  None, step 13 is `world.cancel.cancelled().await`.
- Bare launch passes `None`. Cavekit + claude-code still pass Some(…).
- Remove step 15 dependency on a live engine (noop if engine is the
  stub).
- Integration tests for both paths.

### Phase 2: SessionSpec vs AgentSpec

- Introduce `SessionSpec` in `ark-types`. Derive from (or co-exist
  with) `AgentSpec` during migration.
- Supervisor takes `SessionSpec`. Orchestrators that need agent-level
  fields receive them via their extension-specific state.
- Update `ark list` / `ark kill` to operate on sessions, not "agents".

### Phase 3: Extension supervisor hooks

- Extend the extension manifest with supervisor-side registration
  points: `on_session_start`, `on_session_end`, `control_verbs`,
  `permission_dispatcher`.
- The `ark-ext-proto` crate grows; `ark-ext-derive` gets new
  derive arms.
- `ark-cavekit-orchestrator` becomes an extension, not a first-class
  crate in the workspace.

### Phase 4: ACP → extension

- `crates/acp-client/` and `crates/supervisor/src/permission.rs` and
  `crates/supervisor/src/turn_inflight.rs` move into
  `extensions/ext-acp-client/`.
- Supervisor no longer depends on `acp-client`.
- Engine resolution chain moves into `ext-acp-client`.
- `ark doctor` learns to ask extensions for their own preflight
  checks instead of hardcoding `claude` + `zellij`.

### Phase 5: Retire `Engine` / `Orchestrator` traits from core

- Move the traits into `ext-acp-client` (or wherever they're still
  useful). `ark-core` no longer exposes them.
- `crates/orchestrators/{cavekit,claude-code}/` become
  `extensions/ext-{cavekit,claude-code}/`.
- Factory in supervisor reduces to `build_multiplexer`.

### Phase 6: Picker new-agent replacement

- Picker gets its "spawn" capability back, but drives the *extension
  that provides the agent* rather than `ark spawn`. Each extension
  that provides spawnable things (agents, workflows) registers with
  a picker-compatible surface — the UI shape is shared, the semantics
  are extension-defined.

## Migration blind spots flagged in review

Per-phase risks the review surfaced:

### Phase 1

- `auto_close.rs` consumes `Outcome` off the bus. None-orchestrator
  sessions emit no terminal `Done` event → tabs leak. Needs a
  bare-session tab-tracking path OR Phase 1 ships with auto-close
  disabled for bare sessions.
- `kill.rs:166-170` always synthesises a fake `Outcome::Killed` event.
  Under bare-ark this is a lie. Either conditionalise on whether the
  session had an agent, or change the semantics.
- `state_writer.rs:168` bootstraps every status to `Phase::Running`.
  Bare sessions stay "running" forever until killed. `ark list` reads
  misleadingly.

### Phase 2

- `StateLayout::spec_path = agent_dir/spec.json`. Legacy specs carry
  orchestrator/engine/cmd/runner_config. Every removed field needs
  `#[serde(default, skip_serializing_if)]` — not just `scene_path`
  which is the only one that got the treatment.
- `AgentEvent::Started { spec: AgentSpec }` embeds the full spec into
  events.jsonl. Every v1 state dir's events.jsonl has the old spec
  shape. Readers (`events_log::EventLogReader`) break if `AgentSpec`
  changes. Either keep `AgentSpec` deserialisable forever OR version
  the jsonl stream.
- `id_resolver.rs` reads `spec.json` by name; `commands.rs::handle_rename`
  rewrites `name` via raw JSON mutation. Migration-era specs may drift
  field names — add a tolerant round-trip test before shipping.

### Phase 3

- `ark-ext-proto::ArkExtension` is a method-per-op trait under a
  version policy (R16 rule #3). Each new supervisor-side registration
  point = new method = minor version bump. Plan the hook surface as
  a batch, not one method at a time.
- `crates/ark-ext-proto/src/supervision.rs` already handles extension
  subprocess lifecycle (stdin-close → SIGTERM → SIGKILL). It does NOT
  yet expose the new session-lifecycle / control-verb /
  permission-dispatcher surfaces.

### Phase 4

- The scene's `AcpHandle` trait is load-bearing for `ops/acp.rs`.
  Either scene declares a generic `AgentHandle` trait (ext-acp-client
  impls it) or scene loses `ctx.acp` and ACP ops register through
  ext-acp-client's own intent registry. Picking the latter is cleaner
  and matches the spec's intent.
- `reaction_dispatcher::OpNode::Acp*` variants need to move with ACP.
  When they do, dispatcher loses exhaustiveness unless the enum becomes
  open (via trait-object or tagged-extension dispatch).
- `AgentEvent::PermissionAsked/PermissionResolved` variants are how
  scene selectors currently observe permission activity. Keep them as
  variants OR reroute permission events through `UserEvent` with a
  reserved namespace (e.g. `ark.acp.permission.asked`) — the latter is
  more principled but breaks every existing scene that uses the
  short-form selector.
- Scene's `PermissionRouter` and `TurnInflightTracker` are structurally
  generic — `RequestRouter<K,V>` and `InflightTracker<K>`. Before moving
  them to ext-acp-client, consider whether ext-pi / ext-subagents want
  them too.

### Phase 5

- `ark-core` re-exports `Engine`, `EngineHandle`, `ApprovalPolicy`,
  `Orchestrator`, `World`, contract suites at `lib.rs:41-45`. Every
  crate that imports them breaks simultaneously. A compat shim crate
  (`ark-legacy-engine`) keeps the workspace green mid-migration; delete
  once all extensions are updated.

### Phase 6

- Picker control-socket call sites (`crates/plugins/picker/`) parse
  `AgentStatus`. Phase 2's type changes hit here first. Audit before
  Phase 2 ships.

## Open Questions

- **Outcome.** Where do `Success / Failed / Killed / Timeout / Crashed`
  go? They're used by `pane::log` to format agent history. Probably
  move to `ext-acp-client` or to a shared `ark-ext-agent-outcome`
  crate that agent-providing extensions depend on.
- **`Phase` enum.** Keep a generic `Phase { Starting, Running, Idle,
  Terminated(kind) }` with opaque `TerminationKind`, OR delete from
  core entirely and let each extension serialize its own phase into
  `AgentStatus.ext_state: HashMap<String, Value>`.
- **`PermissionDecision` + `Severity` enums** (`types/event.rs:191-213`)
  — narrower than ACP/Cavekit, reusable by pure-UI scenes. Keep as
  domain-neutral enums in `ark-types` rather than move.
- **`StateLayout` path naming.** Currently every method uses `agents/`.
  Rename to `sessions/` with a one-shot symlink migration, or keep
  `agents/` and accept it as a legacy artefact.
- **Session discriminator.** Once `ENGINES_V1`/`ORCHESTRATORS_V1`
  lose their gatekeeper role, nothing distinguishes a bare-ark session
  from one with an agent. Need `SessionKind::Bare | Agent(ext_name)`
  on the persisted spec OR derive discriminator from which extensions
  registered for the session.
- **Bus event-type openness.** `AgentEvent` is a closed enum. Phase 4
  shrinks it; extensions emit via `UserEvent { name, payload }`.
  Confirm that scene selectors can target `UserEvent.name`
  expressively enough (e.g. `on "ark.acp.permission.asked" { … }`
  needs to be as ergonomic as today's `on PermissionAsked { … }`).
- **Hot-reload of supervisor-side extension code.** Phase 3 extensions
  run inside the supervisor process. Do we need a plugin-style
  reload, or is extension reload = scene reload + supervisor restart?
- **`ark-ext-proto` scope.** Today it's mostly scene-side metadata.
  Phase 3 adds supervisor-side registration. Does it absorb all of
  `ark-core::engine` + `ark-core::orchestrator`?
- **"Stacks"** the user mentioned for subagent coordination. Needs its
  own kit once Phase 3 is close. Probably a scene primitive that
  composes extensions.
- **Backwards compat for `spec.json`.** Millions (hundreds?) of existing
  state dirs carry `orchestrator` + `engine` fields. Migration reader
  has to tolerate both shapes.

## Non-goals for this spec

- No wire-format redesign for hooks or scenes.
- No zellij refactor. The web client story comes from zellij, not
  ark.
- No new CLI verbs. Bare `ark` + existing subcommands cover it.
- No cross-platform story for Windows — unix-only stays unix-only.

## Explicitly in-core infrastructure (audit confirmed)

The adversarial pass flagged these as correctly core; calling them out
so a reader doesn't over-extract them:

- `crates/types/src/event_bus.rs` — `EventSink` / `EventReceiver`
  infrastructure. The *channel* is core; the `AgentEvent` *payload*
  is what shrinks.
- `crates/scene/src/*` compile pipeline (parse / shape / compose /
  compile / layout lowering) — scene grammar minus the ACP-specific
  scene extensions.
- `crates/mux/zellij/*` — substrate.
- `crates/ark-ext-*` + `crates/ark-ext-derive/*` + `crates/ark-ext-proto/*`
  — the extension framework itself.
- `crates/config/src/*` TOML plumbing — the parser stays; agent-aware
  section schemas shrink.
- `crates/core/src/control_socket.rs` — NDJSON IPC envelope is pure
  plumbing.
- `crates/core/src/events_log.rs` + `status_writer.rs` + `socket_paths.rs`
  — state-on-disk primitives.
- `crates/types/src/state_dir.rs` — path layout. Consider renaming
  leaf `agents/` → `sessions/` (open question above) but keep the
  module.

## What this unblocks

- The "pi.dev extension with subagents via stacks" vision.
- Third-party AI CLI integrations without core edits.
- Bare `ark` as a legitimate first-class use (launching zellij with a
  scene, no agent attached — the "reactive IDE" experience).
- Extensions that are pure-UI (a scene-based diff viewer, a tab
  navigator) without pretending to be orchestrators.
