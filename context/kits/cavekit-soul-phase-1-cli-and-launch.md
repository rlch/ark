---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
parent: cavekit-soul.md
phase: 1
status: ready
---

# Cavekit: Soul Phase 1 — CLI and Launch

## Scope

Covers the CLI + core consumer + bare-launch plumbing required to make
Phase 1 end-to-end green. Four sub-areas:

1. `crates/cli/src/commands/list.rs` strips agent/orchestrator columns.
2. `crates/cli/src/id_resolver.rs` reads `name` from `SessionSpec`.
3. `crates/core/src/consumers/state_writer.rs` rewritten against
   `CoreEvent`; phase-rollup logic stubbed out for Phase-2 ext fan-in.
4. `crates/core/src/consumers/reaction_dispatcher.rs` — `OpNode::Acp*`
   variants deleted outright (ACP leaves the tree per 2026-04-17
   interview #2). Dispatcher no longer matches them. `engine_compat`
   gone.
5. Bare `ark` launch constructs a `SessionSpec { name: "ark", scene: default, … }`
   and calls `run_supervisor_with(spec, None, None, world)`.
6. PTY smoke test goes green.

Locked by the parent kit's **Resolved Decisions** and the "Phase 1
sub-areas" list under Migration Path in `cavekit-soul.md`.

## Requirements

### R1: `ark list` drops `--orchestrator` flag + `PHASE_NAMES` + agent-specific columns

**Description:** `crates/cli/src/commands/list.rs` removes:
- the `--orchestrator` CLI flag,
- the `PHASE_NAMES` const + `is_known_phase` helper + `--status` filter,
- the `phase_name` helper,
- the `orchestrator` / `engine` / `phase` columns in both the table
  renderer and the detail-view renderer.

The minimal row becomes: `id`, `name`, `cwd`, `uptime`, and a `running?`
indicator (derived from socket-reachability).

**Acceptance Criteria:**
- [ ] `rg -n "orchestrator" crates/cli/src/commands/list.rs` prints zero hits.
- [ ] `rg -n "PHASE_NAMES" crates/cli/src/commands/list.rs` prints zero hits.
- [ ] `rg -n "phase_name\|is_known_phase" crates/cli/src/commands/list.rs` prints zero hits.
- [ ] `rg -n "--orchestrator|--status" crates/cli/src/commands/list.rs` prints zero hits in CLI flag definitions.
- [ ] `rg -n "fn phase_str" crates/cli/src/commands/list.rs` prints zero hits (or, if kept, resolves via `running?`-only discriminator).
- [ ] Running `ark list --help` in a built CLI shows no `--orchestrator` and no `--status` option. Verified by a `cargo test -p ark-cli` test that parses the `--help` output via clap's derive and asserts the ArgMatches has no such names.
- [ ] The rendered table has exactly these columns (in some order): `id`, `name`, `cwd`, `uptime`, `running?`. Verified by a `cargo test -p ark-cli` rendering test asserting header string + row string shapes.
- [ ] `ark list <id>` detail view prints `id`, `name`, `cwd`, `uptime`, `running?` — and does NOT print `orchestrator:`, `engine:`, `phase:`, `layout:`, `tab count:`, `last event:`, `findings:`, `source:`. Verified by a `cargo test -p ark-cli` detail-rendering test.
- [ ] `cargo test -p ark-cli` passes.

**Dependencies:** `cavekit-soul-phase-1-types.md` R1, R4, R6.

### R2: `id_resolver.rs` reads `name` from `SessionSpec`

**Description:** `crates/cli/src/id_resolver.rs` continues to resolve
by-name via `spec.json`. The projection struct (currently
`SpecNameProjection { name: String }`) keeps working: `name` is a
top-level field on `SessionSpec`, so the projection JSON decode is
unchanged. The function signatures change from `AgentId` → `SessionId`,
and `list_agent_ids` → `list_session_ids`.

**Acceptance Criteria:**
- [ ] `rg -n "fn list_agent_ids" crates/cli/src/id_resolver.rs` prints zero hits.
- [ ] `rg -n "fn list_session_ids" crates/cli/src/id_resolver.rs` prints exactly one hit.
- [ ] `rg -n "AgentId" crates/cli/src/id_resolver.rs` prints zero hits.
- [ ] `rg -n "SessionId" crates/cli/src/id_resolver.rs` prints at least one hit.
- [ ] `resolve_session_id("<query>", &state_layout)` walks `state_layout.sessions_root()` (not `agents_root`), verified by a `cargo test -p ark-cli` test that seeds `<state>/sessions/<id>/` directories and asserts resolution succeeds.
- [ ] The name-match tier (tier 4) successfully reads `name` out of a `SessionSpec`-shaped `spec.json` — verified by a `cargo test -p ark-cli` test seeding `<state>/sessions/foo-<ulid>/spec.json` with `{"id": {...}, "name": "foo", ...}` and asserting `resolve_session_id("foo", …)` returns the matching id.
- [ ] The existing resolve-tier semantics (exact → prefix → substring → name) are preserved, verified by the existing test suite (adapted for naming) continuing to pass.
- [ ] `cargo test -p ark-cli` passes.

**Dependencies:** `cavekit-soul-phase-1-types.md` R1, R3; `cavekit-soul-phase-1-state-layout.md` R1, R2.

### R3: `state_writer` rewritten against `CoreEvent` with phase-rollup stubbed

**Description:** `crates/core/src/consumers/state_writer.rs` no longer
pattern-matches on agent-specific `AgentEvent` variants. The consumer
subscribes to the `CoreEvent` bus, appends every event to
`events.jsonl`, and maintains a minimal `SessionStatus` atomically.

The per-variant phase-rollup logic that used to key on `AgentEvent::
ToolUse` / `Message` / `FileEdited` / `PermissionAsked` / etc. is
removed. Only the core-shaped fields (`id`, `started_at`,
`terminated_at`, `ext_state`) are written.

The `ext_state` bucket is left empty for Phase 1 (extensions populate
their own buckets starting Phase 2). The file is a scaffold for the
Phase-2 ext fan-in; the `update_status` function signature should make
the ext-dispatch seam obvious (a TODO marker naming the Phase 2 ticket
is acceptable).

**Acceptance Criteria:**
- [ ] `rg -n "AgentEvent" crates/core/src/consumers/state_writer.rs` prints zero hits.
- [ ] `rg -n "Phase::|Outcome::" crates/core/src/consumers/state_writer.rs` prints zero hits.
- [ ] `rg -n "fn update_status" crates/core/src/consumers/state_writer.rs` prints exactly one hit and its implementation updates `terminated_at` on `CoreEvent::SessionEnded` but does NOT branch on `ToolUse` / `Message` / `FileEdited` / `PermissionAsked` / `PermissionResolved` / `Stall` / `PhaseTransition` / `ReviewComment` / `Progress` / `TaskDone` / `Iteration`.
- [ ] A `cargo test -p ark-core` test sends `CoreEvent::SessionStarted { spec }` then `CoreEvent::SessionEnded { terminated_at }` through `state_writer` and asserts the final `status.json` on disk has `terminated_at = Some(_)` (value present, not `None`).
- [ ] A `cargo test -p ark-core` test sends a `CoreEvent::Ext(ExtEvent { ext: "some-ext", kind: "progress", payload: … })` through `state_writer` and asserts the event is appended to `events.jsonl` cleanly but the `ext_state` map stays empty (Phase 1 does not route ext events into `ext_state`).
- [ ] `rg -n "PhaseTransition" crates/core/src/consumers/state_writer.rs` prints zero hits.
- [ ] `cargo test -p ark-core` passes.

**Dependencies:** `cavekit-soul-phase-1-types.md` R4, R5, R6.

### R4: `reaction_dispatcher` drops ACP + engine_compat entirely

**Description:** `crates/core/src/consumers/reaction_dispatcher.rs`
deletes its `OpNode::AcpPrompt` / `AcpCancel` / `AcpPermit` / `AcpSetMode`
match arms. ACP leaves the tree entirely per 2026-04-17 interview #2
(was Phase 3, now folded into Phase 1). Scene's OpNode enum shrinks
accordingly. `engine_compat` module deletes too.

The dispatcher continues to route core ops (`OpNode::Pipe`, `Emit`,
`SetStatus`, `Exec`, `ReloadScene`, `Unknown`) normally, now against
`CoreEvent`.

**Acceptance Criteria:**
- [ ] `rg -n "engine_compat" crates/core/` prints zero hits.
- [ ] `rg -n "OpNode::Acp" crates/core/src/consumers/reaction_dispatcher.rs` prints zero hits (arms deleted).
- [ ] `rg -n "AcpPrompt|AcpCancel|AcpPermit|AcpSetMode" crates/scene/src/` prints zero hits (variants gone).
- [ ] `ls crates/acp-client` fails (crate deleted).
- [ ] `ls crates/scene/src/engine_compat.rs` fails.
- [ ] `ls crates/scene/src/ops/acp.rs` fails.
- [ ] `ls crates/scene/src/ext/acp.rs` fails; same for `permission.rs`, `inflight.rs`, `doctor.rs` under `crates/scene/src/ext/`.
- [ ] `ls crates/supervisor/src/permission.rs` fails; same for `turn_inflight.rs`, `engine_resolution.rs`.
- [ ] `rg "AcpHandle" crates/scene/src/intent.rs` prints zero hits.
- [ ] `cargo test -p ark-core` passes.
- [ ] `cargo check -p ark-scene` passes without any ACP module.

**Dependencies:** `cavekit-soul-phase-1-types.md` R6.

### R5: Bare `ark` launch constructs `SessionSpec` and calls `run_supervisor_with(spec, None, None, …)`

**Description:** `crates/cli/src/commands/launch/mod.rs` (the `run_with`
function) builds a `SessionSpec { id: SessionId::new(&session), name:
session.clone(), scene_path: scene_file, cwd, env: BTreeMap::new(),
created_at: Utc::now(), ext_config: BTreeMap::new() }` and passes
`None` for both orchestrator and engine when invoking the supervisor
spawner.

`crates/cli/src/commands/launch/real.rs`'s `ForkSupervisor` no longer
calls the orchestrator factory — its `supervisor_main` invocation passes
`None, None` for the orchestrator/engine parameters.

The launch-module trait surface (`Multiplexer`, `SupervisorSpawner`)
stays unchanged beyond the `AgentSpec` → `SessionSpec` type parameter
update.

**Acceptance Criteria:**
- [ ] `rg -n "AgentSpec" crates/cli/src/commands/launch/` prints zero hits.
- [ ] `rg -n "AgentId::new" crates/cli/src/commands/launch/` prints zero hits.
- [ ] `rg -n "SessionId::new" crates/cli/src/commands/launch/mod.rs` prints at least one hit.
- [ ] `rg -n "SessionSpec" crates/cli/src/commands/launch/mod.rs` prints at least one hit.
- [ ] `rg -n 'orchestrator.*"ark"|"cavekit"|"claude-code"' crates/cli/src/commands/launch/` prints zero hits (no hardcoded orchestrator/engine slugs in the bare-launch path).
- [ ] `rg -n "build_orchestrator|build_engine" crates/cli/src/commands/launch/` prints zero hits.
- [ ] The signature of `SupervisorSpawner::spawn_and_wait_for_ready` takes a `SessionSpec` (not `AgentSpec`). `rg -n "spawn_and_wait_for_ready" crates/cli/src/commands/launch/traits.rs` shows the signature.
- [ ] The production spawner passes `None, None` through to `supervisor_main`. `rg -n "supervisor_main" crates/cli/src/commands/launch/real.rs` shows the call with `None, None` in the orchestrator + engine positions.
- [ ] `cargo test -p ark-cli --test launch_integration` passes (existing mock tests keep passing after the type migration).

**Dependencies:** `cavekit-soul-phase-1-types.md` R1, R3; `cavekit-soul-phase-1-supervisor.md` R1.

### R6: PTY smoke test `real_zellij_accepts_compiled_default_layout` passes

**Description:** The existing PTY smoke test at
`crates/cli/tests/launch_pty.rs::real_zellij_accepts_compiled_default_layout`
must go green (skipped conditions — zellij not on PATH, or running
inside zellij — are acceptable CI-skip behaviour and do not count as
failure).

This is the end-to-end success criterion for Phase 1: bare `ark`
launches against a real zellij and a session of the expected name is
created.

**Acceptance Criteria:**
- [ ] `cargo test -p ark-cli --test launch_pty -- --ignored=no real_zellij_accepts_compiled_default_layout` exits 0 on a host where `zellij` is on PATH and `$ZELLIJ` is unset.
- [ ] The same test exits 0 (skipped path) on a host where `zellij` is NOT on PATH, printing `"SKIP: zellij not on PATH"` on stderr.
- [ ] Between test-run start and the `wait_for_session` deadline (10 s), the `zellij list-sessions` output contains the unique session name constructed from `format!("ark-pty-test-{}", std::process::id())`. Exact mechanism verified by the test asserting the bool return of `wait_for_session` is `true`.
- [ ] Test file is not modified in ways that weaken the assertion (the `assert!(appeared, …)` at the end stays; the `10 s` timeout stays; the inside-zellij skip stays).

**Dependencies:** R5, `cavekit-soul-phase-1-supervisor.md` R1, R2, R6, R7.

### R7: Existing `launch_integration.rs` mock tests keep passing

**Description:** `crates/cli/tests/launch_integration.rs` drives the
bare-launch path through the mock `Multiplexer` + `SupervisorSpawner`
trait impls. These tests must continue to pass after the type
migration, verifying that the mock surface lines up with the new
`SessionSpec`-based signatures.

**Acceptance Criteria:**
- [ ] `cargo test -p ark-cli --test launch_integration` exits 0.
- [ ] The mock `SupervisorSpawner` records a `SessionSpec` (not `AgentSpec`) on each invocation, verified by the test inspecting the recorded spec value and asserting its type compiles as `SessionSpec`.
- [ ] No test in this file calls the removed `--orchestrator` / `--status` filters, the `outcome_exit_code` helper, or any deleted `Outcome::*` / `Phase::*` / `AgentStatus::*` / `AgentSpec::*` API.

**Dependencies:** R5, `cavekit-soul-phase-1-types.md` R1, R6.

### R8: Workspace compiles and tests green

**Description:** Phase 1's landing criterion per the parent kit:
"workspace green (`cargo check --workspace --tests` + `cargo test
--workspace`)."

**Acceptance Criteria:**
- [ ] `cargo check --workspace --tests` exits 0.
- [ ] `cargo test --workspace` exits 0 (all tests passing, no ignored-but-required tests).
- [ ] `rg -n 'TODO\(cavekit-soul\)' crates/` — the permissible count is 0 if Phase 1 fully closes its own scope, OR the hits are all inside code-paths explicitly documented as "deferred to Phase N" where N >= 2. No TODOs may block "bare `ark` launches; PTY test green; state is sessions/".
- [ ] No test or module is `#[ignore]`d as a workaround. `rg -n "#\[ignore\]" crates/cli/tests/ crates/supervisor/src/ crates/core/src/` prints zero hits introduced by Phase 1 (pre-existing `#[ignore]`s in unrelated files are acceptable — the requirement is "no new ones to paper over Phase 1 breakage").

**Dependencies:** R1, R2, R3, R4, R5, R6, R7.

## Out of Scope

- Picker changes (`crates/plugins/picker/` / `ark spawn`). Phase 6.
- Full ACP op removal from the reaction dispatcher. Phase 3.
- ACP-extraction — moving `acp-client`, `permission.rs`, `turn_inflight.rs`,
  `engine_resolution.rs`, scene ACP ext subtrees into
  `extensions/acp-client/`. Phase 3.
- Config schema changes (`crates/config/src/schema.rs`,
  `HookEntry.on_orchestrator`). Phase 4.
- `ark doctor` refactor. Phase 4.
- `crates/hook/` → `extensions/claude-code/bin/cc-hook/`. Phase 4.
- Claude-code tool taxonomy in `crates/types/src/permission.rs`. Phase 4.
- Ext-contributed list columns + list-row formatter hooks. Phase 2.
- `ark list --watch` behavioural changes beyond column stripping (the
  `--watch` flag stays; it just now watches the reduced column set).

## Cross-References

- Parent spec: `cavekit-soul.md` (Resolved Decisions + Phase 1 sub-areas 4, 5, 6).
- Depends on: `cavekit-soul-phase-1-types.md` (R1 `SessionSpec`, R3 `SessionId`, R4 `SessionStatus`, R6 `CoreEvent`).
- Depends on: `cavekit-soul-phase-1-supervisor.md` (R1 `run_supervisor_with` signature, R3 non-`Outcome` return, R6 bare-session no-auto-close).
- Depends on: `cavekit-soul-phase-1-state-layout.md` (R1 `sessions_root`, R2 per-session accessors).

## Changelog

(empty)
