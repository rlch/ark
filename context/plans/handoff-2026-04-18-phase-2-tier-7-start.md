---
created: "2026-04-18"
audience: next session / receiving agent
status: handoff
supersedes: handoff-2026-04-18-phase-2-impl-start.md (Tiers 0-6 now done)
---

# Handoff — Phase 2 Tier 7 start

## TL;DR

1. **Tiers 0-6 DONE.** 37 of 45 tasks complete (T-001..T-036 + T-045). 8 tasks remain (Tier 7: T-037..T-042, Tier 8: T-043..T-044). 42 commits since phase-2 start at `21d4c20`.
2. **Workspace green.** 1799 tests pass (`cargo test --workspace --lib`); pre-existing warnings only (2 in `hook/bridge.rs:311`).
3. **Execute Tier 7 next.** `context/plans/build-site-soul-phase-2.md` line 120 — stub harness + capability matrices + trybuild goldens + integration tests.
4. **DO NOT parallel-dispatch code-producing agents on main tree.** Confirmed 2× git-index collision this session (see memory `feedback_parallel_git_collision.md`). Serial wins.
5. **`/ck:make` protocol still active** — `--peer-review` flag enabled, Codex tier-gate review fires after each tier. Keep the cadence.

## What got built (Tiers 0-6 summary)

| Tier | Tasks | Domain | Key commits |
|---|---|---|---|
| 0 | T-001/T-002/T-003/T-045 | new crate skeleton + intent_register deletion + metadata fields + ExitReason | 20c21e6, b8e07ab, 8547655, 1544dab |
| 1 | T-004..T-007 | HandleKind, HandleId, View/CommandView/ZellijView, InvalidationCause | 6f31378, e913cb5, 541db89 |
| 2 | T-008..T-013 | Pane<V>/Stack<V>/TabHandle, PaneLike, marker-gated affordances, ParamsHash, SuppressionPolicy | 0ffc222, 5b33711, a904a98, cc5c02f |
| 3 | T-014..T-017 | HandleGone in ExtensionError, handle.invalidated wire golden, SessionHandles, cross-crate exports | bcd38e2, 09676d1, 753f91f, 5d157e3 |
| 4 | T-018..T-023 | 6 RPC req/resp pairs, lifecycle + feature-group hooks, ViewDecl.kind | ad001b7, 7a24239, d6a67bf |
| 5 | T-024..T-027 | Capability taxonomy (8 flags), `#[derive(View/CommandView/ZellijView)]`, capabilities auto-advertise | 117af17, 380700c, d691db7 |
| 6 | T-028..T-036 | Capability dispatcher, handshake host caps, load sequence, CLI list/doctor, figment loader, view-type table, reload gates, closed_by_user | c5dbf78, a949d40, 5d742b7, afa46d3, 890dd17, 59c1293 |

**Ledger:** `context/impl/impl-soul-phase-2.md` (per-task status table + wave log).

## Codex tier-gate findings — state of deferrals

| ID | Severity | Status | Detail |
|---|---|---|---|
| F-001 | P1 | DEFERRED → T-046 (new) | facet-kdl 0.42 sibling-Vec<T> ambiguity (pre-existing; impacts config_sections/reload_gates/views roundtrip with other Vec siblings) |
| F-002 | P1 | DEFERRED → T-043 | CURRENT_PROTOCOL_VERSION stays 1.0 until final Tier 8 bump |
| F-003 | P2 | FIXED | FlatEvent projects `exit` as scalar string |
| F-004 | P2 | FIXED | HandleId field is now private |
| F-005 | P2 | FIXED | HandleKind doc-comment truthful about wire compat |
| F-006 | P2 | FIXED | InvalidationCause doc-comment truthful |
| F-007 | P2 | FIXED | Stack::spawn_pane stub emits distinct placeholder handles |
| F-008 | P1 | FIXED | NDJSON encoder/decoder round-trips HandleGone structured payload |
| F-009 | P2 | FIXED | SessionHandles::pane_by_name_typed() variant with explicit view-type check |
| F-010 | P1 | PARTIAL | ViewDecl.kind "stack" now warn-logs via scene/ext/binding.rs (full consumption = T-034; consumed by validator now but binding still maps to pane render-mode) |
| F-011 | P2 | DEFERRED → tooling | gen-extension-spec emits HandleId as struct-with-field-0 not transparent string |
| F-012 | P1 | FIXED | module-level doc mentions `#[view]` → `#[ark_view]` rename |
| F-013 | P1 | ACCEPTED | #[derive(View)] requires ark-view dep in user crate; theoretical breakage only |
| F-014 | P2 | FIXED | Derives preserve generics via split_for_impl() |
| F-015 | P1 | FIXED | Dispatcher opts out on method_not_found (session-scoped set) |
| F-016 | P2 | DEFERRED | Doctor panic isolation granularity — fix alongside real dispatcher wiring post-Tier 7 |

## What's left — Tier 7 (stub harness + tests) + Tier 8 (proto bump + CI gate)

**Tier 7 — stub harness + tests (T-037..T-042, 6 tasks):**

| Task | Kit | Scope |
|---|---|---|
| T-037 | tests R1 | `crates/ark-ext-test-support` crate — `StubExtension` builder (dev-dep only, not reachable from production) |
| T-038 | tests R2 | NDJSON subprocess `[[bin]] ark-stub-ext` — parity test with in-proc |
| T-039 | tests R3 | Version-mismatch matrix (5 cells) across 1.0 ↔ 1.1 ↔ 2.0 |
| T-040 | tests R4 | Capability-gate matrix (cases a/b/c, d = xfail) |
| T-041 | tests R5 | `trybuild` view-type compile-fail + compile-pass goldens under `crates/scene/tests/ui/` |
| T-042 | tests R6+R7 | Integration tests: manifest-intent registry, suppression + invalidation suite (6 named tests) |

**Tier 8 — proto bump + CI gate (T-043..T-044, 2 tasks):**

| Task | Kit | Scope |
|---|---|---|
| T-043 | ext-surface R8 | Bump `CURRENT_PROTOCOL_VERSION::new(1, 0) → ::new(1, 1)` at `crates/ark-ext-proto/src/lib.rs:268` — AFTER every other Phase 2 method landed. Also clears F-002. |
| T-044 | tests R8 | Workspace green-gate — `cargo test --workspace --tests` (no features, no env, no net, no zellij); every per-crate `cargo test -p <crate>` also green |

## Dependency order (what unblocks what)

- T-037 blockedBy T-019/T-020/T-021/T-024 — all DONE. Start here.
- T-038 blockedBy T-037 + T-030 (T-030 DONE).
- T-039 blockedBy T-037.
- T-040 blockedBy T-028 (DONE) + T-037.
- T-041 blockedBy T-034 (DONE). Can run in parallel with T-037.
- T-042 blockedBy T-036 (DONE) + T-037 + T-041.
- T-043 blockedBy T-017 through T-042.
- T-044 blockedBy T-043.

**Suggested wave order (SERIAL — do not parallelize on main tree):**
1. Wave 8a: T-037 (stub harness — foundational, new crate).
2. Wave 8b: T-041 (trybuild goldens — scene-side, disjoint from harness).
3. Wave 8c: T-038 (NDJSON subprocess bin on stub crate).
4. Wave 8d: T-039 (version-mismatch matrix, consumes T-037).
5. Wave 8e: T-040 (capability-gate matrix, consumes T-037 + T-028).
6. Wave 8f: T-042 (integration — consumes T-037 + T-041).
7. Wave 8g: T-043 (proto bump).
8. Wave 8h: T-044 (CI gate).

## CRITICAL lessons carried forward

From this session:

### `feedback_parallel_git_collision` — SERIAL for code agents

Parallel `general-purpose` agents dispatched on main tree WILL collide at git-add time — confirmed twice in Tier 6 Wave 7a + Wave 7b. Agent A stages files; Agent B commits first and absorbs A's staged files. Even when agents touch different crates, the git index is process-global.

**Rule:** serialize code-producing agents. Parallel is ONLY safe for non-code tasks (explore, read-only research).

### `feedback_agent_type_alias_drift` (from Phase 1) — still valid

Verify via grep not just `cargo check`. Any task saying "delete X" → `rg -r X` should return 0 production hits. Saw this throughout Phase 2.

### `feedback_subagents` — verify commit SHAs

Agents report SHAs that may not exist. Parent always `git log --oneline -1` after agent returns to verify.

### Git-sweep-up as a feature, not bug

When 2 agents race and 1 commits 2 agents' work: that's fine — the code is all there. Don't try to split retroactively (risks lost changes). Note the dual attribution in ledger.

### Codex tier-gate flags P1s that are sometimes intentional

Not every P1 blocks advance. When the build-site explicitly sequences work across tiers (e.g. T-043 proto bump last), Codex flags the gap as P1. Use judgment: accept deferrals that match build-site sequencing, note them in ledger.

### ark-ext-proto CAN depend on ark-types

Old comment at `crates/ark-ext-proto/src/supervision.rs:16` says "this crate intentionally does NOT depend on ark-types" — that's stale. Current ark-types has zero ark-ext-proto deps (verified). T-020 adds the dep; comment should be updated when ergonomic.

## Memory touchpoints

New memory this session:
- `feedback_parallel_git_collision.md` — serial-only rule for code agents

Unchanged from Phase 1:
- `feedback_subagents.md` — use general-purpose + opus, verify SHAs
- `feedback_worktree_agents.md` — no worktrees (auto-clean issue)
- `feedback_use_subagents.md` — dispatch, don't hand-edit
- `feedback_workspace_test.md` — trust `cargo test --workspace --lib` after each wave
- `feedback_findings_file_preserved.md` — ledger files are append-only
- `feedback_agent_type_alias_drift.md` — grep-verify deletions

## Runtime / tooling state

- `cargo test --workspace --lib` — 1799 pass, 3 ignored (same 3 as Phase 1: pty tests gated on non-zellij env).
- `cargo check --workspace --tests` — green.
- 2 pre-existing warnings: unused imports in `hook/bridge.rs:311`.
- Codex CLI works via `codex exec review --base <sha>`. The plugin's `scripts/codex-review.sh` passes the deprecated `--approval-mode` flag and fails — just call `codex exec review` directly.

## Execute with /ck:make or manual dispatch

Either:

**Option A — continue `/ck:make` autonomous loop** (recommended):
```
/ck:make context/plans/build-site-soul-phase-2.md --peer-review
```
(The existing loop is still active on a 30-min cadence per earlier `/loop` call. Cron job ID `e83c50f8`. Cancel with `CronDelete e83c50f8` if switching to manual.)

**Option B — manual serial dispatch:**
Each agent gets one task from the "Suggested wave order" above. Start with T-037. After each, verify commit SHA lands, update ledger.

## DON'T re-open during Tier 7+

Same table as prior handoff — `phase-2-design-decisions.md` R-5..R-14 decisions are locked. Additionally pinned this session:

| Decision | State | Source |
|---|---|---|
| `HandleKind` = `{Tab, Pane, Stack}` | LOCKED in T-004 | ark-view R2 |
| `ExitReason` = `{Normal, Error(String), Cancelled}` + `#[non_exhaustive]` | LOCKED in T-045 | §R-5 |
| `HandleId` inner field private (not `pub String`) | LOCKED in F-004 fix | kit R5 opaque contract |
| `#[ark_view(...)]` (not `#[view(...)]`) attribute | LOCKED in T-025 | ext-surface R7 |
| Capability taxonomy = exactly 8 flags | LOCKED in T-024 | ext-surface R6 |
| `PHASE_2_CAPABILITY_FLAGS` + `HOST_PHASE_2_CAPABILITIES` duplicated with cross-check test | ACCEPTED — supervisor has no dep on ark-ext-proto | T-029 |
| Blake3 for both `ParamsHash` and manifest-set hash | LOCKED | §R-6 + T-012 + T-034 |
| Doctor per-extension panic isolation = F-016 deferred to real dispatcher wiring | ACCEPTED | F-016 |

## Post-Phase-2

When T-044 CI gate passes:
- v0.1 tag lands after 3 sibling build sites also complete: `build-site-scene-2026-04-18.md`, `build-site-claude-code-ext.md`, `build-site-cleanup.md`.
- Bare-ark PTY smoke test must pass from OUTSIDE zellij (user-owned, not agent's concern).

## End of handoff

Self-contained. New session can open `context/plans/build-site-soul-phase-2.md`, find the first incomplete Tier (7), fan out (serial) starting at T-037.
