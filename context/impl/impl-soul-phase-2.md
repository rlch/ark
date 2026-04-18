---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: Soul Phase 2

Build site: context/plans/build-site-soul-phase-2.md

Ledger is prepend-only. Newest entries at top. Append completion rows below as tasks land.

## Task Status

| Task | Tier | Kit R | Status | SHA | Notes |
|------|------|-------|--------|-----|-------|
| T-001 | 0 | ark-view R1 | DONE | `20c21e6` | new crate `crates/ark-view/` + root Cargo.toml; dep budget bounded (facet+serde+serde_json+thiserror) |
| T-002 | 0 | ext-surface R3 | DONE | `b8e07ab` | deleted `intent_register` RPC; `intent_dispatch` retained; gen-extension-spec EXPECTED_METHODS updated |
| T-003 | 0 | ext-surface R4 | DONE | `8547655` | added `config_sections`+`reload_gates` Vec fields + decl structs; downstream construction sites patched |
| T-045 | 0 | ext-surface R1 | DONE | `1544dab` | `CoreEvent::SessionEnded` gains `exit: ExitReason`; new enum `{Normal, Error(String), Cancelled}` `#[non_exhaustive]`; also patched `kill.rs`/`orchestration.rs` prod sites |
| F-003 fix | 0 | — | DONE | `3133529` | Codex tier-gate: flatten `exit` to scalar string on FlatEvent; scene selectors can now match `exit="cancelled"` |
| T-004 | 1 | ark-view R2 | DONE | `6f31378` | HandleKind {Tab, Pane, Stack} + Facet + snake_case serde |
| T-005 | 1 | ark-view R5 | DONE | `6f31378` | HandleId opaque newtype (`#[serde(transparent)]`); inner String now private (F-004 fix) |
| T-006 | 1 | ark-view R3 | DONE | `e913cb5` | View + CommandView + ZellijView marker traits (Send+Sync+'static) |
| T-007 | 1 | ark-view R7 | DONE | `541db89` | InvalidationCause {UserClosed, SceneReloadDropped, SessionEnded} |
| F-004/F-005/F-006 fix | 1 | — | DONE | `dc90de0` | HandleId pub-field → private; wire-compat doc clarification on both enums |
| T-008 | 2 | ark-view R4+R5 | DONE | `0ffc222` | Pane<V>/Stack<V>/TabHandle typed wrappers; PhantomData<fn()->V> for Send+Sync |
| T-009 | 2 | ark-view R4 PaneLike | DONE | `a904a98` | PaneLike trait + impls for Pane/Stack |
| T-010 | 2 | ark-view R4 marker-gated | DONE | `a904a98` | impl<V: CommandView> Pane<V> (env/write_stdin/pid); impl<V: ZellijView> Pane<V> (pipe); 4 trybuild negative tests with .stderr goldens |
| T-011 | 2 | ark-view R4 Stack methods | DONE | `a904a98` | spawn_pane/close_child/children/clear; PaneAttrs struct |
| T-012 | 2 | ark-view R8 ParamsHash | DONE | `5b33711` | blake3 of canonical-JSON; ParamsHash newtype `[u8;32]`; hash_params<T: Serialize>() |
| T-013 | 2 | ark-view R8+R9 | DONE | `cc5c02f` | SceneHandleName + SuppressionPolicy contract type; 6 invariants documented; debug_assert on stack-child |
| F-007 fix | 2 | — | DONE | pending | distinct synthetic handles from Stack::spawn_pane stub (atomic counter) |
| T-014 | 3 | ark-view R7 | DONE | `bcd38e2` | HandleGone { handle, cause } on ExtensionError + wire code `-32006 ext-proto/handle-gone` + NDJSON arm |
| T-015 | 3 | ark-view R7 wire | DONE | `09676d1` | ark.handle.invalidated golden test (6 tests pinning `{handle, cause}` shape) |
| T-016 | 3 | ark-view R10 | DONE | `753f91f` | SessionHandles + pane_by_name/stack_by_name/tab_by_name; zero-RPC pure reads |
| T-017 | 3 | ark-view R11 + R1 deps | DONE | `5d157e3` | cross-crate re-exports (10 names) from ark-ext-proto; integration test pins compile-time resolution |
| F-008/F-009 fix | 3 | — | DONE | `1094a0a` | HandleGone NDJSON roundtrip (encoder stuffs structured data, decoder parses it); SessionHandles::pane_by_name_typed enforces view-type today; untyped variant doc-updated |
| T-018..T-044 | 4-8 | various | PENDING | — | see build site |

## Wave Log

### Waves 4a+4b — 2026-04-18 — Tier 3
- 4a (parallel 3): T-014 (bcd38e2) + T-015 (09676d1) + T-016 (753f91f). Disjoint crates/files. All landed.
- 4b (solo): T-017 (5d157e3). Cross-crate re-exports from ark-ext-proto, integration test compiles.
- Codex tier-gate: 2 findings. F-008 [P1] HandleGone NDJSON decoder regression — fixed inline (1094a0a) with structured data encoding + roundtrip test. F-009 [P2] SessionHandles ignored view-type — added pane_by_name_typed() variant + honest doc on untyped path. Gate PROCEED.
- 1714 tests pass workspace-wide (+14 since Tier 2).
- Next: Tier 4 — T-018..T-023 (six ext→host RPC request/response pairs + lifecycle + feature-group hooks + ViewDecl extension).

### Waves 3a+3b — 2026-04-18 — Tier 2
- 3a (parallel 2): T-008 (0ffc222) + T-012 (5b33711). Both landed; PhantomData<fn()->V> to preserve Send+Sync; blake3 canonical JSON hash with hex serde.
- 3b (parallel 2): T-009/T-010/T-011 bundle (a904a98, 12 tests + 4 trybuild compile-fail fixtures with .stderr goldens) + T-013 (cc5c02f, 5 tests + 6 invariants in SuppressionPolicy doc-comment).
- Codex tier-gate: 1×P2 (F-007 spawn_pane aliasing). Fixed inline — atomic counter per stack-handle. Gate PROCEED.
- 1700 tests pass workspace-wide (+34 since Tier 1 start; +53 total ark-view).
- Next: Tier 3 — T-014..T-017 (HandleGone in ExtensionError, ark.handle.invalidated event wire, SessionHandles lookup, public exports + cross-crate deps).

### Wave 2 — 2026-04-18 — Tier 1
- 3 parallel opus agents (T-004+T-005 packet, T-006 solo, T-007 solo). All COMPLETE. Commits: 6f31378, e913cb5, 541db89. Build P, Tests 1666 (+18 ark-view).
- Codex tier-gate: 3 P2 findings — F-004 (HandleId `pub String` breaks opaque contract, fixed), F-005/F-006 (`#[non_exhaustive]` doc-overclaim on HandleKind/InvalidationCause wire compat — doc fixed). Gate PROCEED (no P0/P1).
- Next: Tier 2 — T-008..T-013 (Pane<V>, Stack<V>, TabHandle, PaneLike, marker-gated impls, ParamsHash, SuppressionPolicy).

### Wave 1 — 2026-04-18 — Tier 0
- 4 parallel opus agents (general-purpose, not ck:task-builder per memory). T-001/T-002/T-045 committed self; T-003 interrupted mid-commit, parent took ownership (per memory feedback_subagents — verify sha before marking DONE).
- Codex tier-gate review (`codex exec review --base 21d4c20`) found 3:
  - **F-001 [P1]** facet-kdl 0.42 sibling-Vec<T> ambiguity — **PRE-EXISTING** systemic limitation. Test file documents this explicitly; pre-T-003 `intents/events/views` fields already live with it. Workaround: extension.kdl authored manifests work by node name. **Deferred as T-046 (new) — needs facet-kdl upgrade or discriminator attr.**
  - **F-002 [P1]** `intent/register` removed without proto bump — **INTENTIONAL per T-043 build-site sequencing**. T-043 (last Tier 8 task) bumps `CURRENT_PROTOCOL_VERSION::new(1,0)→::new(1,1)`. Build site explicitly lands the version bump last so conformance tests catch missing surface. Deferred as-designed.
  - **F-003 [P2]** ExitReason emits object on FlatEvent — **fixed inline** (`3133529`). Scalar `exit` + separate `exit_message` projection.
- Tier 0 complete. 5 commits. 1648 tests pass (+3 from T-003 roundtrip).

## Deferred findings

- **F-001** → new task **T-046** (create as Tier 8.5 remediation): facet-kdl sibling-Vec discriminator. Either upgrade crate or add rename attrs. Blocks kit R4 end-to-end acceptance.
- **F-002** → tracked by existing T-043 (Tier 8 proto bump). No action beyond completing T-043.
