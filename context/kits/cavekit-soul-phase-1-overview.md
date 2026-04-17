---
created: "2026-04-17T00:00:00Z"
last_edited: "2026-04-17T00:00:00Z"
parent: cavekit-soul.md
phase: 1
status: ready
---

# Cavekit: Soul Phase 1 — Overview

## Scope

Index for the four Phase-1 domain kits. Phase 1 is the bundled
types + supervisor + launch unblock: every change required to make bare
`ark` launch succeed against a real zellij, with `$STATE` renamed to
`sessions/` and the PTY smoke test green.

All design decisions are locked in the parent kit
(`cavekit-soul.md`) under **Resolved Decisions** and the
**Migration Path / Phase 1** section. The sibling kits in this
subdirectory decompose that single phase into four parallelisable
domains.

## Domain Table

| Kit | Scope | R count |
| --- | --- | ---: |
| `cavekit-soul-phase-1-types.md` | Pure type surgery in `crates/types/` + scene `SessionSnapshot`. AgentSpec/Id/Status/Event → Session*/CoreEvent + ExtEvent + FlatEvent. Delete Phase/Outcome/Findings + V1 scope consts + OrchestratorSpec alias. | 9 |
| `cavekit-soul-phase-1-state-layout.md` | `StateLayout` path rename (`agents/` → `sessions/`), accessor renames, supervisor-boot nuke of legacy `$STATE/agents/`. | 5 |
| `cavekit-soul-phase-1-supervisor.md` | `run_supervisor_with` takes `Option<Orchestrator>` + `Option<Engine>`, bare path skips R3 steps 6/13/15, `auto_close` + `kill` rewritten against `CoreEvent::SessionEnded`, `CompiledScene.engine_launch` deleted. | 8 |
| `cavekit-soul-phase-1-cli-and-launch.md` | `ark list` column strip, `id_resolver` name tier, `state_writer` + `reaction_dispatcher` rewritten against `CoreEvent`, bare launch builds `SessionSpec` + None/None, PTY smoke test green, workspace green. | 8 |

**Total Phase-1 requirements:** 30.

## Dependency Order

```
                   ┌──────────────┐
                   │ Phase 1 kits │
                   └──────┬───────┘
                          │
                ┌─────────▼─────────┐
                │ phase-1-types     │  (root)
                │  R1..R9           │
                └────┬────┬────┬────┘
                     │    │    │
         ┌───────────┘    │    └───────────────┐
         │                │                    │
         ▼                ▼                    ▼
  ┌────────────────┐  ┌─────────────┐  ┌──────────────────────┐
  │ phase-1-       │  │ phase-1-    │  │ phase-1-cli-and-     │
  │ state-layout   │  │ supervisor  │  │ launch               │
  │  R1..R5        │  │  R1..R8     │  │  R1..R8              │
  └────────┬───────┘  └──────┬──────┘  └────────┬─────────────┘
           │                 │                  │
           └──────┐   ┌──────┘                  │
                  ▼   ▼                         │
         (supervisor boot calls nuke)           │
                                                │
                      (cli-and-launch depends on both
                       state-layout and supervisor)
```

**Ordering rules:**
- `phase-1-types` is the foundation. Nothing else compiles without its
  type-shape changes landing first.
- `phase-1-state-layout` and `phase-1-supervisor` are independent of each
  other; both depend only on `phase-1-types`. They may be developed in
  parallel.
- `phase-1-cli-and-launch` depends on both `phase-1-state-layout` (for
  `sessions_root` + per-session accessors) and `phase-1-supervisor` (for
  the new `run_supervisor_with` signature + bare session semantics).
- The PTY smoke test (`phase-1-cli-and-launch` R6) is the Phase 1 end-to-
  end gate — it cannot green until every other requirement lands.

## Cross-Reference Map

| Requirement | Consumed by |
| --- | --- |
| `phase-1-types` R1 (`SessionSpec`) | `phase-1-supervisor` R1, R3, R8; `phase-1-cli-and-launch` R1, R5, R7 |
| `phase-1-types` R3 (`SessionId::new(name)`) | `phase-1-state-layout` R1, R2, R3; `phase-1-cli-and-launch` R2, R5 |
| `phase-1-types` R4 (`SessionStatus`) | `phase-1-cli-and-launch` R1, R3 |
| `phase-1-types` R5 (delete `Phase`/`Outcome`/`Findings`) | `phase-1-supervisor` R3, R4; `phase-1-cli-and-launch` R1, R3 |
| `phase-1-types` R6 (`CoreEvent`) | `phase-1-supervisor` R1, R5; `phase-1-cli-and-launch` R3, R4 |
| `phase-1-types` R7 (`FlatEvent`) | (consumed by scene Rhai scope; no downstream Phase-1 kit mandates its use) |
| `phase-1-types` R9 (`SessionSnapshot`) | `phase-1-supervisor` (via `scene_runtime` compile + reaction dispatcher wiring — follow-on) |
| `phase-1-state-layout` R1 (`sessions_root`) | `phase-1-cli-and-launch` R2 |
| `phase-1-state-layout` R2 (`session_*` accessors) | `phase-1-cli-and-launch` R2, R5 |
| `phase-1-state-layout` R4 (nuke legacy agents/) | `phase-1-supervisor` (boot calls it) |
| `phase-1-supervisor` R1 (Option signatures) | `phase-1-cli-and-launch` R5 |
| `phase-1-supervisor` R3 (non-Outcome return) | `phase-1-cli-and-launch` R5, R7 |

## Parent Kit Decision Coverage

Every Resolved Decision from `cavekit-soul.md` that affects Phase 1 lands
in exactly one sibling kit:

| Parent kit decision | Owning sibling kit |
| --- | --- |
| State compat — nuke `$STATE` | `phase-1-state-layout` R4 |
| Path leaf — `agents/` → `sessions/` | `phase-1-state-layout` R1, R2 |
| No `SessionKind` discriminator | (negative — no kit introduces one) |
| `Phase` + `Outcome` delete from core | `phase-1-types` R5 |
| `SessionId::new(name)` with ulid baked in | `phase-1-types` R3 |
| Bus payload: 2-level `CoreEvent` + `Ext(ExtEvent)` | `phase-1-types` R6, R7 |
| Phase 1 is bundled | (this overview) |
| Bare `ark` launch succeeds / PTY test green | `phase-1-cli-and-launch` R5, R6 |
| Engine/Orchestrator Option in supervisor | `phase-1-supervisor` R1, R2, R8 |
| `CompiledScene.engine_launch` removed | `phase-1-supervisor` R7 |
| `auto_close` / `kill` rewritten against SessionEnded | `phase-1-supervisor` R4, R5 |
| Bare sessions default no-auto-close | `phase-1-supervisor` R6 |
| `ark list` column strip | `phase-1-cli-and-launch` R1 |
| `id_resolver` name field unchanged | `phase-1-cli-and-launch` R2 |
| `state_writer` rewritten, phase-rollup stubbed | `phase-1-cli-and-launch` R3 |
| `reaction_dispatcher` Acp* placeholders + engine_compat off | `phase-1-cli-and-launch` R4 |

## Explicitly Out of Scope for Phase 1

Tracked in the parent kit; repeated here so reviewers don't mis-scope
individual kits:

- **Phase 2:** `ArkExtension` trait expansion (`on_session_start`,
  `on_session_end`, `control_verbs`, `permission_dispatcher`,
  `scene_compile_hook`, `doctor_checks`, `list_columns`); ext-fan-in for
  `ark list` columns + `ark doctor` checks.
- **Phase 3:** ACP extraction — `acp-client`, `permission.rs`,
  `turn_inflight.rs`, scene ACP ext subtrees, `engine_resolution.rs`,
  `reaction_dispatcher::OpNode::Acp*` removal via open-dispatch pattern.
- **Phase 4:** Claude Code + Cavekit orchestrators → extensions;
  `crates/hook/` → `cc-hook` binary; `permission.rs` tool taxonomy;
  config schema per-extension.
- **Phase 5:** Delete `Engine` / `Orchestrator` traits + factory +
  `engine_contract.rs` / `orchestrator_contract.rs`.
- **Phase 6:** Picker spawn via extensions.

## Cross-References

- Parent spec: `cavekit-soul.md` (**the** source of truth for every
  locked decision — read first before touching any sibling kit).
- Sibling kits: listed in the Domain Table above.
- Project overview index: `cavekit-overview.md` (the workspace-level
  kit overview; `cavekit-soul.md` supersedes `cavekit-architecture.md`).

## Changelog

(empty)
