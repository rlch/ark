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
| T-018 | 4 | ark-view R6 | DONE | `ad001b7` | 6 pane/stack RPC req+resp struct pairs (OpaqueJson for payload/attrs per existing pattern) |
| T-019 | 4 | ark-view R6 | DONE | `ad001b7` | 6 default `method_not_found` trait methods on ArkExtension |
| T-020 | 4 | ext-surface R1 | DONE | `7a24239` | on_session_start + on_session_end hooks; OnSessionEndRequest carries ExitReason (OpaqueJson pattern since SessionSpec lacks Facet) |
| T-021 | 4 | ext-surface R2 | DONE | `ad001b7` | 4 feature-group hooks: scene_compile_hook/control_verbs/doctor_checks/list_columns |
| T-022 | 4 | ext-surface R5 | DONE | `ad001b7` | intent_dispatch retention pin (doc-comment + regression test) |
| T-023 | 4 | ext-surface R4 | DONE | `d6a67bf` | ViewDecl.kind = Option<StringNode> ("pane"\|"stack"); downstream construction sites patched |
| T-003 doctest fix | 4 | — | DONE | `8815508` | ark-ext-metadata doctest construction missed config_sections/reload_gates |
| F-010/F-011 fix | 4 | — | PARTIAL | `<pending>` | F-010 kind=stack gap now warn-logged in scene/ext/binding.rs (full consumption at T-034); F-011 gen-extension-spec serde-transparent limitation deferred to tooling task |
| T-024 | 5 | ext-surface R6 | DONE | `117af17` | PHASE_2_CAPABILITY_FLAGS const with exactly 8 flags; set-equality + naming-convention tests |
| T-025 | 5 | ext-surface R7 | DONE | `380700c` | #[derive(View)] + #[ark_view(...)] attribute; auto-name from PascalCase→kebab-case |
| T-026 | 5 | ext-surface R7 | DONE | `d691db7` | #[derive(CommandView)] + #[derive(ZellijView)] marker derives (PATH A — minimal, no cross-derive magic) |
| T-027 | 5 | ext-surface R7 | DONE | `d691db7` | #[ark_extension(capabilities="...")] auto-advertise via inherent ARK_CAPABILITIES const |
| F-012/F-014 fix | 5 | — | DONE | `bfde279` | generics preserved via split_for_impl() (Extension/View/CommandView/ZellijView); attribute rename doc |
| T-028 | 6 | host-dispatch R6 | DONE | `c5dbf78` | capability→method table + per-ext capability registry + should_dispatch + warn-once |
| T-029 | 6 | host-dispatch R7 | DONE | `a949d40`* | HOST_PHASE_2_CAPABILITIES slate (8 flags, sorted); *commit also absorbed T-033 via git-race |
| T-030 | 6 | host-dispatch R8 | DONE | `5d742b7`* | 7-step load sequence in ext_loader; structured tracing events; step-order test. *commit also absorbed T-031/T-032 via git-race |
| T-031 | 6 | host-dispatch R1 | DONE | `5d742b7` | ExtensionColumnProvider trait; core id/name/status at 0-2; ext cols alpha-sorted |
| T-032 | 6 | host-dispatch R2 | DONE | `5d742b7` | ExtensionCheckProvider; per-check ok/warn/fail/skipped; fail → non-zero exit |
| T-033 | 6 | host-dispatch R3 | DONE | `a949d40` | figment `extension.<ext>.<section>` loader; required/optional validation |
| T-034 | 6 | host-dispatch R4 | DONE | `afa46d3` | ViewTypeTable + validate_view_reference + manifest_set_hash (blake3) |
| T-035 | 6 | host-dispatch R5 | DONE | `890dd17` | reload gate dispatcher; AND Proceed/Defer; fail-open; ReloadDeferredPayload |
| T-036 | 6 | host-dispatch R9 | DONE | `59c1293` | ClosedByUserMap (BTreeMap); consult() → Spawn/Skip/EvictAndSpawn |
| F-015 fix | 6 | — | DONE | `4a594b7` | opt-out on method_not_found — future should_dispatch returns false |
| T-037 | 7 | tests R1 | DONE | `3863b1a` | new crate `ark-ext-test-support`; StubExtension + builder (5 config axes); 12 ArkExtension methods; call_log accessor; OpaqueJson payloads (not serde_json::Value — gotcha) |
| T-041 | 7 | tests R5 | DONE | `6d20f9c` | new crate `crates/scene-macros/` (proc-macro); `ark_scene_macros::validate_scene!` re-exported as `ark_scene::validate_scene`; parses inline manifest + scene KDL via `kdl` crate (bypasses facet-kdl's bare-`item` Vec limitation); emits `compile_error!("<path>.kdl:<line>:<col>: <msg>")` with plain-English diagnostics; 4 R5 compile-fail goldens (undeclared_view_type / view_type_mismatch_on_handle_attr / stack_child_under_non_stack_parent / handle_typed_attr_takes_non_handle) + 2 compile-pass fixtures enhanced with `validate_scene!` green path; each new stderr carries 1 `.kdl:line:col` hit; harness updated with `TRYBUILD=overwrite` doc; pre-existing Rust-level fixtures kept; workspace lib tests 1807 P (unchanged); duplicate-~30-LOC fallback used to avoid the scene↔scene-macros dep cycle |
| T-038..T-040, T-042..T-044 | 7-8 | various | PENDING | — | see handoff-2026-04-18-phase-2-tier-7-mid.md |

## Wave Log

### Waves 7a-e — 2026-04-18 — Tier 6 (host-dispatch, 9 tasks)
- Wave 7a (parallel): T-028 (c5dbf78), T-029+T-033 (both landed in a949d40 via git-index race), T-033 (absorbed). Supervisor capability dispatcher + host slate + figment loader.
- Wave 7b (parallel): T-030 + T-031/T-032 bundled (all in 5d742b7 via second git-index race). Extension load sequence + CLI list/doctor refactor.
- **Race pattern saved to memory** (feedback_parallel_git_collision.md): parallel agents on main tree collide at git-add time even when crates are disjoint. Switched to SERIAL dispatch for Waves 7c-e.
- Wave 7c (serial): T-034 (afa46d3) scene view-type table + blake3 manifest hash. 16 tests.
- Wave 7d (serial): T-035 (890dd17) reload-gate dispatcher. 7 tests.
- Wave 7e (serial): T-036 (59c1293) closed_by_user suppression storage. 10 tests.
- Codex tier-gate: 2 findings. F-015 [P1] fixed inline (4a594b7) — dispatcher now opts out on method_not_found. F-016 [P2] deferred — doctor per-extension panic isolation needs real dispatcher wiring (post-Tier-7).
- 1799 tests pass workspace-wide (+76 since Tier 5).
- Next: Tier 7 — T-037..T-042 (stub harness + capability matrix + trybuild goldens + integration tests).

### Wave 5 — 2026-04-18 — Tier 4
- Packet A (T-018..T-022 bundled, ext-proto lib.rs + transports): ad001b7 landed 4 of 5 (T-018/T-019/T-021/T-022). T-020 BLOCKED by agent on ark-ext-proto→ark-types dep cycle fear. Parent verified no cycle (ark-types has zero ext-proto deps), added the dep in Cargo.toml, re-dispatched T-020.
- Packet B (T-023 metadata-types): d6a67bf. ViewDecl.kind = Option<StringNode>; downstream construction sites patched in ark-ext-metadata, scene/ext/binding + registry.
- Follow-up: T-020 unblocked at 7a24239 (OpaqueJson pattern since neither SessionSpec nor ExitReason derive Facet). Doctest in ark-ext-metadata at `:256` fixed at 8815508 (T-003 follow-up — construction site the T-003 agent missed).
- Codex tier-gate: 2 findings. F-010 [P1] ViewDecl.kind stored but unconsumed in scene binding — partially mitigated with warn-log (full consumption in T-034); F-011 [P2] gen-extension-spec emits HandleId as struct-with-field-0 rather than transparent-string — tooling limitation deferred. Gate: advance with accepted deferral of F-010 consumption (T-034) + F-011 (tooling).
- 1721 tests pass workspace-wide (+7 since Tier 3).
- Next: Tier 5 — T-024..T-027 (capability-flag taxonomy + derive View/CommandView/ZellijView + auto-advertise).

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
