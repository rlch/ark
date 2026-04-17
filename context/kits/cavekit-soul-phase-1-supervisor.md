---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
parent: cavekit-soul.md
phase: 1
status: ready
---

# Cavekit: Soul Phase 1 — Supervisor

## Scope

Covers the supervisor-loop and lifecycle refactor required to make bare
`ark` launch succeed. Four sub-areas:

1. `run_supervisor_with` signature changes — `orchestrator` and `engine`
   become `Option<Box<dyn _>>`, skipping the R3 boot steps that presumed
   them.
2. `auto_close.rs` rewritten against `CoreEvent::SessionEnded`; bare
   sessions default to no auto-close.
3. `kill.rs` emits `CoreEvent::SessionEnded { terminated_at }` instead of
   a synthesised `AgentEvent::Done { Outcome::Killed }`.
4. `scene_runtime::CompiledScene.engine_launch` field deleted.

Locked by the parent kit's **Resolved Decisions** and the "Phase 1
sub-areas" list under Migration Path in `cavekit-soul.md`.

## Requirements

### R1: `run_supervisor_with` takes `Option<Box<dyn Orchestrator>>` + `Option<Box<dyn Engine>>`

**Description:** `crates/supervisor/src/orchestration.rs` changes the
`run_supervisor_with` signature so that `engine` and `orchestrator` are
both optional. When `None`, the supervisor skips R3 step 6's `build_*`
calls, step 13's `orchestrator.run`, and step 15's `engine.teardown`.

The existing orchestrator factory call-sites in `crates/orchestrators/
cavekit/` and `crates/orchestrators/claude-code/` keep working by passing
`Some(…)` — Phase 1 does not delete these orchestrators.

**Acceptance Criteria:**
- [ ] `rg -n "fn run_supervisor_with" crates/supervisor/src/orchestration.rs` prints exactly one hit and its declared signature shows `orchestrator: Option<Box<dyn Orchestrator>>` and `engine: Option<Box<dyn Engine>>`.
- [ ] A `cargo test --workspace` test drives `run_supervisor_with(spec, None, None, …)` against a `ZellijMux::for_test(...)` stub and asserts the call returns cleanly (no panic, no `orchestrator.run` invocation) and that the final `session_dir` contains a `spec.json` + `status.json`.
- [ ] The existing `cargo test -p ark-supervisor` suite still passes (the `Some(orchestrator), Some(engine)` path must keep working).
- [ ] When called with `engine = None` the supervisor never invokes any method on the (absent) engine — verified by the same test asserting that a preflight-style assertion the stub engine sets is NOT observed.
- [ ] When called with `orchestrator = None` the supervisor does not panic and does not block waiting for a `run(…)` future; verified by the test completing within a bounded tokio timeout (e.g. 5 s) without needing an external cancel fire.

**Dependencies:** `cavekit-soul-phase-1-types.md` R1 (signature takes `SessionSpec` not `AgentSpec`), R6 (no longer returns / synthesises `Outcome`).

### R2: Main loop becomes `world.cancel.cancelled().await` when orchestrator is None

**Description:** When `orchestrator = None`, the supervisor's long-lived
main loop reduces to awaiting the cancel token (per the parent kit's
Layer 1 Supervisor reactive loop block: "Main loop:
`world.cancel.cancelled().await`. Nothing else."). Consumers, reaction
dispatcher, scene compile, plugin lifecycle, and control socket bind
still run — they are independent of the orchestrator.

**Acceptance Criteria:**
- [ ] When `orchestrator = None`, the supervisor path between R3 step 11 (Started / SessionStarted emission) and R3 step 14 (drain consumers) is a single `cancel.cancelled().await` (or a `tokio::select!` whose only non-cancel arm is idle). Verified by reading the code and by a `cargo test --workspace` test that starts `run_supervisor_with(spec, None, None, …)`, waits 200 ms, fires the external cancel token, and asserts the function returns within 5 s.
- [ ] Consumers (state writer, reaction dispatcher, control-socket handler) still spawn and drain on the `None` path — verified by a `cargo test --workspace` test asserting `events.jsonl` contains at least a `SessionStarted` record after a `None / None` run.
- [ ] The `None` path does NOT attempt to compile a `World` that contains an orchestrator handle — `World::new` may still be called with references to mux + events + cancel, but no orchestrator field is populated.

**Dependencies:** R1.

### R3: Supervisor returns a non-`Outcome` result type

**Description:** Because `Outcome` is deleted from `ark-types`
(`cavekit-soul-phase-1-types.md` R5), `run_supervisor` /
`run_supervisor_with` must change their return type. The replacement is
implementation-agnostic: either `Result<(), anyhow::Error>`, or a new
`ExitReason` enum, or the `Option<…>` shape. Requirement:

**Acceptance Criteria:**
- [ ] `rg -n "-> Result<Outcome" crates/supervisor/src/` prints zero hits.
- [ ] `rg -n "Outcome::" crates/supervisor/src/` prints zero hits.
- [ ] `run_supervisor_with` and `run_supervisor` compile after the change — `cargo check --workspace --tests` passes.
- [ ] Every existing call-site of `run_supervisor_with` / `run_supervisor` (in `crates/cli/src/commands/launch/real.rs`, `crates/cli/src/commands/launch/mock.rs`, tests in `crates/supervisor/src/orchestration.rs`, and any `crates/orchestrators/*/` integration tests) compiles against the new return type.
- [ ] The outer daemon caller in `crates/cli/src/commands/launch/real.rs` derives a Unix exit code from the return value without calling the deleted `outcome_exit_code` helper. `rg -n "outcome_exit_code" crates/` prints zero hits.

**Dependencies:** R1, `cavekit-soul-phase-1-types.md` R5.

### R4: `auto_close.rs` rewritten against `CoreEvent::SessionEnded`

**Description:** `crates/supervisor/src/auto_close.rs` no longer
pattern-matches on `Outcome` variants. The `AutoClosePolicy` struct
(`on_done`, `on_fail`, `on_kill`) is deleted — per the parent kit, bare
sessions default to no auto-close; ext hooks register their own close
conditions in Phase 2. `apply_auto_close_policy` is reduced to the
minimum required for Phase 1: a no-op for bare sessions, called from a
`CoreEvent::SessionEnded` observation path.

**Acceptance Criteria:**
- [ ] `rg -n "pub struct AutoClosePolicy" crates/supervisor/src/auto_close.rs` prints zero hits.
- [ ] `rg -n "on_done|on_fail|on_kill" crates/supervisor/src/auto_close.rs` prints zero hits.
- [ ] `rg -n "Outcome::" crates/supervisor/src/auto_close.rs` prints zero hits.
- [ ] The module compiles under `cargo check --workspace --tests`.
- [ ] Bare sessions (sessions spawned without an orchestrator per R1) finish without closing any zellij tabs automatically. Verified by a `cargo test --workspace` test that runs `run_supervisor_with(spec, None, None, …)` against a scripted `ZellijMux::for_test` and asserts zero `close-tab-at-index` calls landed on the executor recording after the cancel fires and the supervisor drains.
- [ ] The module survives Phase 1 as a placeholder (OK to be nearly empty) or is deleted outright. If deleted, `rg -n "auto_close" crates/supervisor/src/lib.rs` prints zero hits.

**Dependencies:** `cavekit-soul-phase-1-types.md` R5, R6.

### R5: `kill.rs` emits `CoreEvent::SessionEnded { terminated_at }` at grace-expiry

**Description:** `crates/supervisor/src/kill.rs` no longer synthesises
an `AgentEvent::Done { outcome: Outcome::Killed }`. The grace-expiry
path instead broadcasts `CoreEvent::SessionEnded { terminated_at: now }`.
The tab-registry teardown logic stays (kill still closes any open
zellij tabs for that session).

**Acceptance Criteria:**
- [ ] `rg -n "Outcome::Killed" crates/supervisor/src/kill.rs` prints zero hits.
- [ ] `rg -n "AgentEvent::Done" crates/supervisor/src/kill.rs` prints zero hits.
- [ ] `rg -n "CoreEvent::SessionEnded" crates/supervisor/src/kill.rs` prints at least one hit.
- [ ] A `cargo test -p ark-supervisor` test drives `kill_handler` with a slow orchestrator, lets the grace window expire, and asserts a `CoreEvent::SessionEnded { terminated_at: _ }` record lands on the event bus — and asserts NO `AgentEvent::Done` record lands (pattern-match on the deleted variant fails compile, which is itself the strongest form of this check).
- [ ] `kill_handler`'s return type no longer mentions `Outcome` (it returns `()` or a new `KillReason` / `ExitReason` type — implementation-agnostic).
- [ ] Tab-teardown continues to function: the same test above records `mux.close_tab` calls for each tab in the registry, verified via `StubExecutor::recorded_calls()`.

**Dependencies:** R3, `cavekit-soul-phase-1-types.md` R6.

### R6: Bare sessions default to no auto-close on kill

**Description:** Explicit positive requirement for UX. Per the parent
kit: "Bare sessions default to no-auto-close." When a bare session (no
orchestrator) terminates — whether via natural cancel or kill grace —
zellij tabs opened by the user stay open unless the user explicitly
closed them.

**Acceptance Criteria:**
- [ ] A `cargo test -p ark-supervisor` test spawns a bare session (orchestrator=None, engine=None) against a scripted mux, fires the external cancel, and asserts the mux executor records zero `close-tab-at-index` invocations on the teardown path.
- [ ] The same test asserts the `spec.json` and `status.json` are written to disk under `<state>/sessions/<id>/`.

**Dependencies:** R1, R4, R5.

### R7: `CompiledScene.engine_launch` field deleted

**Description:** `crates/supervisor/src/scene_runtime.rs` removes the
`engine_launch: Option<EngineLaunch>` field from `CompiledScene` and the
`with_engine_launch` builder method. Extensions that want to launch
agent subprocesses on scene compile register a scene-compile hook in
Phase 2 and do it themselves.

**Acceptance Criteria:**
- [ ] `rg -n "engine_launch" crates/supervisor/src/scene_runtime.rs` prints zero hits.
- [ ] `rg -n "with_engine_launch" crates/supervisor/src/` prints zero hits.
- [ ] `rg -n "EngineLaunch" crates/supervisor/src/scene_runtime.rs` prints zero hits (the type may still live in `engine_resolution.rs` for Phase 3-extraction; it just doesn't adorn `CompiledScene`).
- [ ] `CompiledScene` still carries `source`, `ir`, `scene_id`, `registry`, `max_cascade_depth`. `rg -n "pub (source|ir|scene_id|registry|max_cascade_depth)" crates/supervisor/src/scene_runtime.rs` shows each field as a public member or accessor.
- [ ] `cargo test -p ark-supervisor` passes.

**Dependencies:** none (orthogonal to R1-R6 at the module level).

### R8: Existing orchestrator crates compile with `Some(…)` path unchanged

**Description:** Per the parent kit: "crates/orchestrators/cavekit/ +
claude-code/ keep compiling against the new types (`Option<Orchestrator>`
path = `Some(…)` for these; they survive Phase 1 unchanged
behaviour-wise)."

The orchestrator trait (`crates/core/src/orchestrator.rs`) stays. Its
`run(spec: AgentSpec, world: World) -> Outcome` signature must change to
accept `SessionSpec` and return a non-`Outcome` type. This kit does not
prescribe the replacement — but it must exist and compile, and the two
in-tree orchestrator impls must adopt it.

**Acceptance Criteria:**
- [ ] `cargo check --workspace --tests` passes (full workspace build).
- [ ] `cargo test -p ark-orchestrator-cavekit` passes (the cavekit orchestrator's own tests, if any exist).
- [ ] `cargo test -p ark-orchestrator-claude-code` passes.
- [ ] The orchestrator trait no longer mentions `Outcome` or `AgentSpec`: `rg -n "Outcome|AgentSpec" crates/core/src/orchestrator.rs` prints zero hits.
- [ ] `rg -n "fn run\s*\(" crates/core/src/orchestrator.rs` shows the `run` method taking `SessionSpec` (not `AgentSpec`).
- [ ] A scripted integration test in `crates/supervisor/src/orchestration.rs` (or its test module) drives `run_supervisor_with(spec, Some(orch), Some(engine), …)` with the `cavekit` orchestrator + a stub engine, and the call succeeds. (If this test already exists in some form, R8 just requires that it keeps passing; a cargo-test command is sufficient evidence.)

**Dependencies:** R1, R3, `cavekit-soul-phase-1-types.md` R1, R6.

## Out of Scope

- ACP extraction (`permission.rs`, `turn_inflight.rs`, `acp-client`).
  Phase 3.
- Scene ACP ext moves (`scene/src/ext/{acp,permission,inflight,doctor}.rs`,
  `scene/src/ops/acp.rs`, `scene/src/engine_compat.rs`, `intent.rs`'s
  `AcpHandle`). Phase 3.
- Factory slug deletion (`crates/supervisor/src/factory.rs` — the
  `build_engine` / `build_orchestrator` functions remain in Phase 1 and
  Phase 2 as-is; they accept the `None` path caller by simply not being
  called). Phase 5 deletes the factory.
- Engine resolution chain (`engine_resolution.rs`). Survives Phase 1
  unchanged; moves to `extensions/acp-client/` in Phase 3.
- Picker behaviour (`crates/plugins/picker/`). Phase 6.
- Plugin lifecycle manager changes beyond what compiles against the new
  `CoreEvent` / `SessionSpec` types — the manager stays.
- Config schema changes. Phase 4.
- Turning the orchestrator/engine traits themselves into extensions.
  Phase 5 deletes the traits entirely.

## Cross-References

- Parent spec: `cavekit-soul.md` (Resolved Decisions + Phase 1 sub-area 3).
- Depends on: `cavekit-soul-phase-1-types.md` (R1, R3, R4, R5, R6 — all type-shape prerequisites).
- Depends on: `cavekit-soul-phase-1-state-layout.md` R4 (supervisor boot calls the legacy-agents nuke).
- Siblings: `cavekit-soul-phase-1-cli-and-launch.md` (CLI launch path drives the new `run_supervisor_with(spec, None, None, …)` signature).

## Changelog

(empty)
