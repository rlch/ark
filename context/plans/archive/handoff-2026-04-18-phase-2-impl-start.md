---
created: "2026-04-18"
audience: next session / receiving agent
status: handoff
supersedes_prep_for: build-site-soul-phase-2.md impl start
follows: handoff-2026-04-18-claude-code-first-pivot.md (that was design pivot; this is impl kickoff)
---

# Handoff — Phase 2 impl start

## TL;DR

1. **Execute Phase 2 impl next.** `context/plans/build-site-soul-phase-2.md` — 45 T-numbered tasks, 9 tiers, starts at Tier 0. Use `/ck:make` (or `/ck:make build-site-soul-phase-2`), or fan out manually with `Agent` calls.
2. **Tree is green at commit `a50e484`.** `cargo check --workspace --tests` passes (2 pre-existing unused-import warnings in `hook/bridge.rs:311`). `cargo test --workspace --lib` 1645 pass / 0 fail / 3 ignored. Phase 1 complete + one follow-up fix landed (`ZELLIJ_SOCKET_DIR=/tmp/ark-<uid>`).
3. **All 10 open items are pinned.** `context/plans/phase-2-design-decisions.md` R-5..R-14 resolve everything the mapping agents flagged. No open interviews needed before starting impl.
4. **Build site scope is tight.** Cross-site deps enforced via blockedBy: Phase 2 must finish before scene-2026-04-18 + claude-code-ext begin. Cleanup runs last.

## Context you need to load

Read these before starting (top-to-bottom order):

1. **`context/plans/build-site-soul-phase-2.md`** — the executable DAG. Tier 0 tasks have no deps; fan those out first.
2. **`context/plans/phase-2-design-decisions.md`** — authoritative design contract. Every cross-cutting decision is locked (R-1..R-14). DO NOT re-open any of these during impl; if something feels wrong, surface as an explicit `AskUserQuestion` rather than silently deciding.
3. **`context/kits/cavekit-soul-phase-2-overview.md`** + the 4 sub-kits it indexes — what the tasks implement.
4. **`context/impl/impl-soul-phase-1.md`** — Phase 1 retrospective. Check the "Dead Ends" section before attempting Tier 0: T-020 type-alias drift lesson is the biggest gotcha.

## Phase 1 lessons you must carry forward

From `impl-soul-phase-1.md` + memory:

### `feedback_agent_type_alias_drift` — verified lesson

Downstream compile-check gates tempt agents to paper over type deletions via:

- `pub type OldName = NewName` aliases
- Commenting out / cordoning entire modules
- Stubbing out function bodies with `todo!()` just to make cargo-check pass

All three happened during Phase 1 T-020 and had to be reverted in commit `9437edd`. **Verify Phase 2 task output via structural greps, not just cargo-check.** If a task says "delete X", `grep -r X` should return 0 hits in production code. If a task says "migrate X to Y", `grep -r X` should only find X in tests / archived dirs.

### `feedback_use_subagents` — fan out aggressively

The `ck:task-builder` agent is broken; use `Agent(subagent_type="general-purpose", model="opus", ...)` instead. Main session orchestrates; subagents grind. Tier 0 has several tasks that can run in parallel — dispatch them in a single message.

### `feedback_workspace_test` — trust tests, not reports

Subagent self-reports can be optimistic. After every wave:

```bash
cargo test --workspace --lib        # fast signal
cargo check --workspace --tests     # compile gate
```

Use the `bp:surveyor` pattern (`Agent(subagent_type="Explore")`) to verify removals via grep rather than taking an agent's word.

### `feedback_findings_file_preserved` — ledger files are prepend-only

`context/impl/impl-review-findings.md`, `dead-ends.md`, `loop-log.md` accumulate history. Never `Write(whole file)` — always `Edit` or append. Same for `impl-soul-phase-2.md` once you create it.

### `feedback_worktree_agents` — use main tree for agents

`isolation: "worktree"` auto-cleans on zero changes and has cost agent commits before. Run builder agents on main with normal workflow.

## Runtime setup before starting

One-shot environment checks:

```bash
# Sanity — should be clean-ish
git status --short

# Workspace green
cargo check --workspace --tests
cargo test --workspace --lib

# Phase 1 follow-ups the user owes (not impl agent's concern, but note)
#   - Validate bare `ark` from outside zellij via
#     `cargo test -p ark-cli --test launch_pty -- --nocapture`
#     (cannot be run from inside zellij; user does this on their own)
```

Expected state at handoff: 2 warnings (unused imports in `hook/bridge.rs:311`), 3 `#[ignore]` tests with documented rationale, 1645 passing tests, no errors.

## How to execute the build site

### Option A — autonomous `/ck:make`

```
/ck:make build-site-soul-phase-2
```

Runs the build site end-to-end, parallelizing within tiers. Watch for: surveyor reports between tiers; cross-tier merge conflicts; any agent that type-alias-drifts.

### Option B — manual Tier 0 kickoff, then evaluate

Recommended if you want tighter review loops:

1. Read Tier 0 of `build-site-soul-phase-2.md` (~4 tasks, no deps).
2. Fan out 4 parallel Agent calls (one per task).
3. Surveyor pass after all return.
4. Evaluate tier-completion gate, then proceed to Tier 1.

Tier 0 composition (per map report):
- T-001 workspace skeleton for `crates/ark-view/`
- T-002 `HandleKind` narrowed to `{Tab, Pane, Stack}` in `crates/scene/src/intent.rs`
- T-003 (verify exact IDs against build site when you open it)
- T-045 extend `CoreEvent::SessionEnded` with `exit: ExitReason` in `ark-types::event`

Tier 0 is intentionally light so you can sanity-check the agent flow before committing to the deeper tiers.

## Decisions doc — DON'T re-litigate during impl

The 10 resolved items at `phase-2-design-decisions.md` R-5..R-14 are:

| R | Decision | Avoid re-opening |
|---|---|---|
| R-5 | `SessionOutcome` = `CoreEvent::SessionEnded` + `exit: ExitReason { Normal, Error(String), Cancelled }` | ExitReason variants are fine — don't add more |
| R-6 | Manifest hash = `blake3` | Don't substitute xxhash |
| R-7 | Stack-child naming = `<stack-handle>-<ulid>` (ark auto) | Don't accept ext-supplied names |
| R-8 | Union syntax = **DEFERRED**, v0.2 | Don't sneak union-parser tasks in |
| R-9 | Stack sizing attrs on container only, children = compile-error | Don't silently drop child attrs |
| R-10 | `view_table` = private, `IntentContext::view_of(&HandleId)` only | Don't add pub accessor |
| R-11 | Delete `factory.rs` whole; inline `build_multiplexer` | Don't keep factory.rs as skeleton |
| R-12 | Delete `run_preflight: bool` param | Don't keep as config field |
| R-13 | `cc-hook` at `$XDG_BIN_HOME/cc-hook`, embedded via build.rs | Not claude-code-ext site's concern yet; prep only |
| R-14 | Claude Code project-dir encoding = **non-issue** (transcript_path supplied) | Don't add probe code |

Items that remain genuinely open (impl-time micro-decisions, not re-design):

- `ParamsHash` hash input composition (for R-9's suppression-lift-on-params-change) — pin exact input fields during `ark-view` R8 impl.
- Capability-gate WARN log format — pin during `host-dispatch` R6 impl.
- `reload.deferred` event wire shape — pin during `host-dispatch` R5 impl.
- `Contributions` shape for `scene_compile_hook` — pin during `host-dispatch` impl when the first caller materializes.
- `#[derive(Extension)]` behavior when it can't see overridden methods in a separate `impl` block — hard-fail or warn; decide during ext-surface R7 impl.
- Stub test-support crate name (`crates/ark-ext-test-support` proposed) — confirm or rename during `tests` R1 impl.

These don't block tier starts; they're one-line choices inside specific tasks.

## Known-ugly corners you'll hit

### Cascading workspace deps

Phase 2 adds `crates/ark-view/`. The new crate depends on `crates/types/`. Downstream: `crates/scene`, `crates/ark-ext-proto`, `crates/ark-ext-derive`, `crates/supervisor` all need a path dep added to their `Cargo.toml`. The build site should task each, but verify via `grep -l 'ark-view' crates/*/Cargo.toml` before declaring a tier done.

### `CURRENT_PROTOCOL_VERSION` bump is the LAST task

Do not bump `ark-ext-proto::CURRENT_PROTOCOL_VERSION` from `1.0` to `1.1` until every Phase 2 method has landed. The bump signals "1.1 fully present" to conformance tests. Premature bump = red tests.

### `commands.rs` 1500-line test suite is still gated

From Phase 1: `crates/supervisor/src/commands.rs` has a test suite gated behind `any()` (always false) because the old tests assumed the deleted Orchestrator trait. Phase 2 R6 in `host-dispatch` sub-kit (capability-aware dispatcher) is the moment to rewrite them — not before. If a Phase 2 task feels compelled to un-gate them early, pause and ask.

### Macro-derive test isolation

`ark-ext-derive` codegen changes (`ext-surface` sub-kit) can silently break `inventory::submit!` at link time. After every derive change, run the full ext-proto conformance suite:

```bash
cargo test -p ark-ext-proto --tests
```

A passing single-crate unit test is not enough; the inventory collection happens at binary link.

## Memory touchpoints to keep fresh

After a completed tier, update:

- `context/impl/impl-soul-phase-2.md` — create on first completion; prepend-only ledger style.
- Memory — if you hit a new class of problem (not already in feedback_* entries), save it as a new memory.

Do NOT touch:

- `context/impl/impl-soul-phase-1.md` — Phase 1 archive; read-only.
- Any `cavekit-soul-phase-1-*.md` — frozen.
- `phase-2-design-decisions.md` R-1..R-14 — resolved; if new decisions emerge, append as R-15+ with new interview.

## Post-Phase-2 — what unlocks

When `build-site-soul-phase-2.md` completion gate is green:

1. Unlocks **scene-2026-04-18 landing** (`build-site-scene-2026-04-18.md`, 26 tasks). Consumer of `crates/ark-view`.
2. Partially unlocks **claude-code-ext** (`build-site-claude-code-ext.md`, 48 tasks). Tier 0-1 salvage can start; Tier 2+ blocks on scene-2026-04-18.
3. Cleanup (`build-site-cleanup.md`, 12 tasks) runs last, after claude-code-ext salvage done.

v0.1 tag lands after all four sites complete + bare-ark PTY smoke test green from outside zellij.

## End of handoff

Self-contained. New session can open at `build-site-soul-phase-2.md` and begin. Preserve this doc for reference — update only if a lesson learned during Phase 2 impl warrants an R-15+ entry in the decisions doc (otherwise leave immutable).
