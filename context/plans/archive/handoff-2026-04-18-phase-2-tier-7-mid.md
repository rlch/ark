---
created: "2026-04-18"
audience: next session / receiving agent
status: handoff
supersedes: handoff-2026-04-18-phase-2-tier-7-start.md
follows: handoff-2026-04-18-phase-2-tier-7-start.md (T-037 landed; 7 tasks remain)
---

# Handoff — Phase 2 Tier 7 continuation (T-037 landed; T-038..T-044 remain)

## TL;DR

1. **38 of 45 tasks DONE.** T-037 (stub harness) landed at `3863b1a`. 7 tasks remain: T-038..T-042 (Tier 7), T-043..T-044 (Tier 8).
2. **Workspace green.** ~2046 tests pass (`cargo test --workspace --lib`). 45 commits since phase-2 start.
3. **Execute serially.** Parallel dispatches caused 2× git-index collisions in Tier 6 (memory: `feedback_parallel_git_collision.md`). One agent at a time on main tree.
4. **Use opus subagents.** `Agent(subagent_type="general-purpose", model="opus", ...)`. The `ck:task-builder` subagent type is broken (memory).
5. **Codex tier-gate review fires after each tier.** `codex exec review --base <sha> --title "..."`. Don't use `scripts/codex-review.sh` — its `--approval-mode` flag is deprecated.

## Repo state at handoff

- Head commit: `3863b1a feat(ark-ext-test-support): T-037 stub harness crate`
- Base for Tier 7: `8b9d567` (pre-Tier-7 head, used for eventual tier-gate review via `--base 8b9d567`)
- Tree: clean except untracked `.claude/` + `context/impl/loop-log-soul-phase-1.md`
- A prior `/loop` call scheduled a 30-min cron (`CronCreate` id `e83c50f8`) that re-invokes this task. If the loop fires when you're working, integrate or cancel with `CronDelete e83c50f8`.

## What T-037 delivered

- NEW crate `crates/ark-ext-test-support/` (workspace member, not in any production [dependencies])
- `StubExtension` + `StubBuilder` with all 5 spec'd config axes:
  - `.with_method(name, handler)` — per-method JSON-erased closures
  - `.advertise_capabilities(iter)`
  - `.with_manifest(ExtensionMetadata)`
  - `.with_protocol_version(ProtocolVersion)`
  - `.method_advertised_but_unimplemented(name)`
- Accessors: `advertised_capabilities()`, `manifest()`, `protocol_version()`, `call_log()`, `clear_call_log()`
- Helper: `stub_advertising_everything_implementing_nothing()`
- `impl ArkExtension for StubExtension` covering every Phase 2 method (12 surface methods: 6 pane/stack + 2 lifecycle + 4 feature-group)
- 8 unit tests + 1 doctest — all green

Key gotcha learned: `PaneEmitRequest.payload` is `OpaqueJson` (= `String`), NOT `serde_json::Value`. T-038+ fixtures need `"{}".to_string()` not `json!({})`.

## Remaining tasks — Tier 7 (T-038..T-042) + Tier 8 (T-043..T-044)

### T-038 — NDJSON subprocess variant of stub harness

**Kit:** tests R2
**BlockedBy:** T-037 ✓, T-030 ✓
**Scope:**
- Add `[[bin]] ark-stub-ext` target in `crates/ark-ext-test-support/`
- Binary accepts config via `--config <path>` or `ARK_STUB_CONFIG` env (JSON format)
- Runs the existing `crates/ark-ext-proto/src/transport/ndjson.rs` server against the stub
- Round-trip test `stub_subprocess_matches_in_proc` proves parity with in-proc
- `supervisor_spawns_stub_and_dispatches` exercises the real ext-launch path

**Implementation hint:** Look at existing conformance tests in `crates/ark-ext-proto/tests/conformance/` for the `both_transports!` pattern. Subprocess config serialization: pick JSON (tests need to round-trip through filesystem or env).

### T-039 — Version-mismatch matrix tests

**Kit:** tests R3
**BlockedBy:** T-037 ✓
**Scope:**
- Place tests under `crates/ark-ext-proto/tests/` (or `crates/ark-ext-test-support/tests/`)
- 5 cells per kit table:
  - 1.1 ↔ 1.1: OK, no warnings
  - 1.1 ↔ 1.0: OK no warn (older ext tolerated)
  - 1.0 ↔ 1.1: OK with WARN log naming unknown caps
  - 2.0 ↔ 1.1: UnsupportedVersion, no further RPCs
  - 1.1 ↔ 2.0: symmetric UnsupportedVersion
- Use `stub.with_protocol_version(ProtocolVersion::new(1, 0))` etc. to drive the matrix

### T-040 — Capability-gate matrix tests

**Kit:** tests R4
**BlockedBy:** T-028 ✓, T-037 ✓
**Scope:**
- Cases: (a) advertised + implemented → call reaches stub; (b) not-advertised → zero calls + zero wire bytes; (c) advertised-but-unimplemented → ONE WARN + session survives; (d) removed-in-MAJOR placeholder (xfail)
- Exercise via `supervisor::ext_dispatch::should_dispatch` + the stub's `call_log()`
- For case (b), assert `stub.call_log().is_empty()` after dispatcher skips
- For case (c), assert the one-warn dedup: subsequent `should_dispatch` returns false (F-015 opt-out)

### T-041 — trybuild view-type goldens

**Kit:** tests R5
**BlockedBy:** T-034 ✓
**Scope:**
- Fixtures under `crates/scene/tests/ui/`:
  - compile-fail: `undeclared_view_type.rs`, `view_type_mismatch_on_handle_attr.rs`, `stack_child_under_non_stack_parent.rs`, `handle_typed_attr_takes_non_handle.rs`
  - compile-pass: `valid_pane_and_stack_decls.rs`, `cross_ext_view_reference.rs`
- Harness `crates/scene/tests/view_types_trybuild.rs` with `trybuild::TestCases::new()` + `.compile_fail` / `.pass`
- `.stderr` goldens generated via `TRYBUILD=overwrite cargo test -p ark-scene --test view_types_trybuild`

**IMPORTANT gotcha:** The kit spec calls for `.kdl:line:col` error pointers in stderr. That requires scene's compile-time *macro* validator, which doesn't exist yet — only runtime `validate_view_reference` (T-034) does. Options:
- (a) Build minimal fixtures exercising the Rust-level ViewDecl / ExtensionMetadata type-checker (E0308, E0063 errors). Document the KDL-level deferral.
- (b) Build a tiny proc-macro wrapper `scene_compile_check!(...)` that invokes T-034 APIs and emits compile errors. More work; satisfies kit fully.
- Recommend (a) for T-041 scope; note (b) as a future task in the ledger.

User rejected an earlier T-041 dispatch (the interrupted attempt) — likely didn't want the "deferral" shortcut. Consider option (b) if user asks to make the KDL-level validator real.

### T-042 — Integration tests (manifest-intent + suppression + invalidation)

**Kit:** tests R6 + R7
**BlockedBy:** T-036 ✓, T-037 ✓, T-041
**Scope:**
- Manifest-intent integration: `manifest_intent_appears_in_registry`, `scene_op_dispatches_to_manifest_intent`, `intent_register_rpc_method_is_gone` (grep + compile-fail), `undeclared_intent_scene_op_rejected_at_compile` (trybuild)
- Suppression + invalidation suite (6 named tests): `user_close_records_suppression_and_emits_invalidated`, `reconcile_same_params_skips_spawn_after_user_close`, `reconcile_new_params_respawns_after_user_close`, `pane_op_after_invalidation_returns_handle_gone`, `supervisor_restart_clears_suppression`, `stack_child_user_close_does_not_suppress_respawn`
- Harness via T-037 StubExtension + T-036 ClosedByUserMap + T-028 ext_dispatch

### T-043 — Bump CURRENT_PROTOCOL_VERSION to 1.1

**Kit:** ext-surface R8
**BlockedBy:** T-017..T-042 (everything except T-044)
**Scope:**
- `crates/ark-ext-proto/src/lib.rs:268` — change `ProtocolVersion::new(1, 0)` → `ProtocolVersion::new(1, 1)`
- `rg 'ProtocolVersion::new\(1, 0\)' crates/` must show 0 production hits post-bump (tests at 1.0 for version-mismatch matrix are OK)
- Existing `is_compatible` test still green
- Clears Codex finding F-002 (protocol advertises intent_register gone but version stayed at 1.0)

### T-044 — CI / workspace green gate

**Kit:** tests R8
**BlockedBy:** T-043
**Scope:**
- `cargo test --workspace --tests` green with no features, no env, no network, no zellij
- `cargo check -p ark-view` + `-p ark-ext-proto` + `-p ark-ext-derive` + `-p ark-ext-metadata-types` + `-p ark-scene` + `-p ark-supervisor` + `-p ark-cli` + `-p ark-config` + `-p ark-ext-test-support` all green
- `TRYBUILD=overwrite cargo test --workspace` yields no diff against committed goldens
- Stub crate appears in root `Cargo.toml` members ✓ (already done by T-037)

## Suggested wave order (SERIAL)

Serial dispatches per memory rule. Roughly 4-8 minutes per agent.

| Wave | Task | Why this order |
|------|------|---|
| 8b | T-041 | Scene-side, disjoint from ext-proto work. Ships trybuild harness early so T-042 can consume. |
| 8c | T-038 | Consumes T-037 stub; adds subprocess transport variant. |
| 8d | T-039 | Consumes T-037 for multi-version handshakes. |
| 8e | T-040 | Consumes T-037 + T-028 dispatcher. |
| 8f | T-042 | Consumes T-037 + T-041 + T-036 + T-028. Biggest integration; do last in Tier 7. |
| 8g | T-043 | Proto bump — landable once T-042 green. |
| 8h | T-044 | CI gate — final. |

## Critical lessons (saved to memory, reiterated here)

### `feedback_parallel_git_collision` — SERIAL for code agents

Confirmed 2× this session (Tier 6 Wave 7a, Wave 7b). Two agents on main tree, each `git add <explicit-paths>` + commit — Agent B's commit sweeps up Agent A's staged files. Git-index is process-global; explicit paths only prevent over-staging, not cross-agent absorption.

**Rule:** serialize code-producing agents. Parallel is safe only for exploration (read-only Glob/Grep/Read).

### `feedback_subagents` — verify commit SHAs

Agents can report non-existent SHAs. After each agent returns, run `git log --oneline -1` to verify. If absorbed into a prior commit (the collision case), note the dual attribution in the ledger rather than trying to rewrite history.

### `feedback_agent_type_alias_drift` — grep-verify deletions

When a task says "delete X": grep after, not just `cargo check`. Type aliases silently paper over deletions.

### `feedback_findings_file_preserved` — ledger is append-only

`context/impl/impl-soul-phase-2.md` prepends newest waves at the top. Never `Write(whole file)` — always `Edit` or append.

## Codex tier-gate findings — triage state

Full table in the prior handoff; additions since T-037 landed: **none** (T-037 didn't trigger a review yet — the next tier-gate fires after T-042 when Tier 7 completes).

## Dispatch template for next agent

```
Agent(
  subagent_type: "general-purpose",
  model: "opus",
  prompt: "TASK: T-NNN — <title>

BUILD SITE: /Users/rjm/Coding/Personal/ark/context/plans/build-site-soul-phase-2.md
CAVEKIT: /Users/rjm/Coding/Personal/ark/context/kits/cavekit-soul-phase-2-tests.md (R<n>)

FILE OWNERSHIP (only these):
- <exact paths>

DO NOT touch <other paths>.

SCOPE: <paste relevant kit/decision text>

ACCEPTANCE CRITERIA:
- <bullets from kit>

CAVEMAN MODE: ON in reports. Code/commits normal.

COMMIT:
- git add <explicit paths>
- Message: <conventional commit style>
- Do NOT push.

DEAD ENDS:
- <anti-patterns>
- Do NOT git add .
- Do NOT use worktrees (memory: feedback_worktree_agents)

REPORT:
TASK RESULT:
- Task: T-NNN
- Status: COMPLETE | PARTIAL | BLOCKED
- Commit SHA: <full>
- Files: <list>
- Tests added: <N>
- Issues: <any>"
)
```

After agent returns:
1. `git log --oneline -3` — verify commit SHA reported by agent matches `HEAD`
2. `cargo test --workspace --lib` — verify no regressions
3. Update `context/impl/impl-soul-phase-2.md` — add task row
4. Append brief entry to `context/impl/loop-log.md`
5. If tier complete: run Codex review `codex exec review --base <tier-start-sha> --title "..."`
6. Commit ledger separately

## Codex tier-gate review at end of Tier 7

Fire this when T-042 lands:
```bash
codex exec review --base 8b9d567 --title "Phase 2 Tier 7: stub harness + matrices + trybuild + integration (T-037..T-042)"
```

Triage P0/P1 inline. Defer P2 to ledger deferrals. Gate is advisory — if the build site intentionally sequences a fix for later (like T-043 proto bump fixes F-002), mark ACCEPTED in ledger and proceed.

## Runtime notes

- Codex CLI: `codex exec review --base <sha>` works. `codex exec review --base <sha> --title "..."` is the full form. The plugin's `scripts/codex-review.sh` passes deprecated `--approval-mode` flag — skip it, use `codex exec review` directly.
- Cavekit preset: quality (opus/opus/sonnet, caveman=on). Execution model = opus.
- Workspace has 2 pre-existing warnings in `hook/bridge.rs:311`. Ignore.
- NDJSON transport already handles `-32006 HandleGone` roundtrip (F-008 fix landed at `1094a0a`).

## File inventory since last handoff

Phase 2 adds:
- `crates/ark-view/` (Tier 0-3)
- `crates/ark-ext-test-support/` (Tier 7, just landed)
- New modules in supervisor: `ext_dispatch.rs`, `host_capabilities.rs`, `ext_loader.rs`, `scene_runtime.rs::reload_gates`, `user_close_suppression.rs`
- New module in scene: `compile/view_types.rs`
- New module in config: `ext_sections.rs`
- Manifest extensions: `ExtensionMetadata.{views, config_sections, reload_gates}`, `ViewDecl.kind`
- Derives: `#[derive(View)]`, `#[derive(CommandView)]`, `#[derive(ZellijView)]`, `#[extension(capabilities="...")]`
- RPC surface: 6 pane/stack methods + 2 lifecycle + 4 feature-group = 12 new trait methods
- Capability taxonomy: `PHASE_2_CAPABILITY_FLAGS` (ark-ext-proto) + `HOST_PHASE_2_CAPABILITIES` (supervisor)
- Protocol: 1.0 still (bump happens in T-043)

## DO NOT re-litigate

Decisions locked in `phase-2-design-decisions.md` R-5..R-14 remain locked. Additionally:
- HandleKind = {Tab, Pane, Stack} (T-004)
- HandleId inner field is private (F-004 fix)
- `#[ark_view(...)]` attribute name, not `#[view]` (T-025)
- PaneEmitRequest.payload = `OpaqueJson` (String), not `serde_json::Value` (T-037 discovery)
- Capability taxonomy = 8 flags exactly (T-024)
- Blake3 for ParamsHash + manifest-set hash (T-012 + T-034)
- PHASE_2_CAPABILITY_FLAGS (in ark-ext-proto) + HOST_PHASE_2_CAPABILITIES (in supervisor) duplicated with cross-check test — supervisor has no dep on ark-ext-proto for that direction

## End of handoff

Self-contained. Next session opens `context/plans/build-site-soul-phase-2.md`, finds Tier 7 at line 120, starts at T-041 (scene trybuild goldens) per suggested order. Or T-038 if Option A (simpler ext-proto subprocess) is preferred. Dispatch serially with opus.
