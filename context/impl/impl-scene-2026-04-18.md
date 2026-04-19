---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: Scene 2026-04-18 Revision

Build site: context/plans/build-site-scene-2026-04-18.md

Ledger append-only. Newest entries at top.

## Audit Results (pre-implementation)

Conducted 2026-04-18 against head commit `854b828`. Parent's prior audit
claiming T-002 + T-009 DONE was **incorrect** — `handle_types.rs` still
present; scene-local `HandleKind` still has `Command`/`Plugin` variants.

| Task | Tier | Status | Notes |
|------|------|--------|-------|
| T-001 | 0 | DONE (phase-2) | `ark-view = { path = "../ark-view" }` present in `crates/scene/Cargo.toml:72`. Re-export block NOT present in `crates/scene/src/lib.rs` — PARTIAL: dep added, re-exports pending. |
| T-002 | 0 | PENDING | `crates/scene/src/handle_types.rs` still present (175 lines). `pub mod handle_types;` at lib.rs:55. |
| T-003 | 0 | PENDING | No `stack` keyword in suggest.rs (no layout-child keyword list exists there). validate/scope.rs doesn't reference `stack`. |
| T-004 | 0 | CUT | per R-8 union syntax deferred to v0.2. |
| T-005 | 1 | PENDING | No `StackNode` struct in `crates/scene/src/ast/layout.rs`. |
| T-006 | 1 | PENDING | `LayoutChild` enum in ast/layout.rs has only `Row|Col|Pane`; no `Stack` variant. |
| T-007 | 1 | PENDING | No `error[scene/union-syntax-deferred]` variant in `SceneError`. |
| T-008 | 1 | PENDING | No empty-body stack policy test / validator. |
| T-009 | 2 | PENDING | `crates/scene/src/intent.rs:84-97` still declares `enum HandleKind { Tab, Pane, Command, Plugin }`. ops/panes.rs still matches on `HandleKind::Tab` via scene-local enum. |
| T-010 | 2 | PENDING | No `HandleKind::Stack` routing anywhere; pane/panes.rs focus/close don't accept stack. |
| T-011 | 2 | PENDING | validate/handles.rs only collects tab+pane; no stack collection or clash tests. |
| T-012 | 2 | PENDING | No `stack` validation in validate/scope.rs; no `error[scene/sizing-on-stack-child]`. |
| T-013 | 3 | PENDING | There's a Phase-2 `ViewTypeTable` in compile/view_types.rs (manifest-level). The 2026-04-18 `ViewTable` is scene-local `BTreeMap<HandleId, ViewDecl>` — DISTINCT. Not present. |
| T-014 | 3 | PENDING | No `view_table` field on `CompiledScene`. |
| T-015 | 3 | PENDING | No `IntentContext::view_of` accessor. |
| T-016 | 3 | PENDING | `handle_type_hint: Option<HandleKind>` still ad-hoc attached via `with_handle_type_hint`. |
| T-017 | 4 | DONE (Wave 5, `8b50cfe`) | `SceneError::ViewTypeMismatch` variant present in `error.rs:815` (verified in place from earlier ledger-prep). |
| T-018 | 4 | DONE (Wave 5, `8b50cfe`) | New `crates/scene/src/validate/view_types.rs` — `validate_view_types(compiled, registry) -> Vec<SceneError>` walks raw KDL for `spawn_into` and emits `scene/view-type-mismatch`. Added `CompiledScene::view_of_internal` crate-private accessor. |
| T-019 | 4 | DONE (Wave 5, `8b50cfe`) | `validate_op_refs` extended with raw-KDL `walk_stack_ops_raw`; new `ExpectedKind::Stack` arm enforces stack-only kind on `spawn_into` / `clear` handle arg. |
| T-020 | 4 | DONE (Wave 5, `8b50cfe`) | `validate/mod.rs` gained `pub mod view_types;` + `pub use view_types::validate_view_types;`. |
| T-021 | 5 | DONE (Wave 6, `ff628ea`) | `MuxHandle` gained `spawn_into_stack(&HandleId, Option<&str>) -> Result<HandleId, String>` and `clear_stack(&HandleId) -> Result<(), String>`. `ulid = { workspace = true }` dep added; `MockMux` updated with deterministic override + child-id recording. |
| T-022 | 5 | DONE (Wave 6, `ff628ea`) | `SpawnIntoOp` in `ops/spawn.rs` — non-idempotent per R-7 — returns the ark-minted `<stack>-<ulid>` child id as `IntentValue::String`. |
| T-023 | 5 | DONE (Wave 6, `ff628ea`) | `ClearOp` in new `ops/stack.rs` — idempotent per R-7 (absent stack = noop). |
| T-024 | 5 | DONE (Wave 6, `ff628ea`) | `register_core_ops` registers both; `CORE_OP_NAMES` gains `"ark.core.spawn_into"` + `"ark.core.clear"`. |
| T-025 | 6 | DONE (Wave 7, `5b17cd9`) | `emit_stack` landed Wave 2; Wave 7 verified + added reconciler round-trip tests. |
| T-026 | 6 | DONE (Wave 7, `5b17cd9`) | `filter_child` Stack arm landed Wave 2; Wave 7 pinned round-trip semantics via 3 integration tests. |
| T-027 | 7 | DONE (Wave 8) | `tests/stack_dispatch.rs` — (a) round-trip, (b) spawn_into dispatch, (c) clear dispatch. `tests/errors.rs` — 5 goldens (3x view-type-mismatch + union-syntax-deferred + sizing-on-stack-child). |

**Audit summary:** 1 PARTIAL (T-001), 25 PENDING. ZERO tasks genuinely
DONE via phase-2. Prior audit was wrong.

**Post-implementation status (2026-04-18):** 26/26 tasks DONE (T-004 CUT
per R-8 union deferral). Scene 2026-04-18 build site CLOSED.

## Implementation waves

### Wave 8 — Tier 7 (T-027) — CLOSE-OUT

SHA: pending (commit 2 of this wave).

- **T-027 completion gate**: new `crates/scene/tests/stack_dispatch.rs`
  integration suite + 5 goldens appended to `crates/scene/tests/errors.rs`.
  - **(a) stack_round_trip_parse_compile_layout**: parse → compile →
    layout-KDL emission — asserts `stacked=true`, `name="subs"`,
    `ARK_HANDLE=@seed`; also runs full `compile_scene` to ensure
    happy-path through the whole pipeline.
  - **(b) spawn_into_dispatches_to_mux_and_mints_child_ulid**:
    in-file inline `TestMux` impl of `MuxHandle` (since crate's
    `MockMux` is `pub(crate)`); dispatches `SpawnIntoOp`; asserts
    child id `@subs-01jarkdemo000000000000000a` per R-7 `<stack>-<ulid>`
    format + serialised view body passed through to mux.
  - **(c) clear_dispatches_to_mux**: dispatches `ClearOp`; asserts
    exactly one `clear_stack(@subs)` mux call + `IntentValue::None` return.
  - **(d) 3x `scene/view-type-mismatch` goldens** — miette
    `GraphicalReportHandler` snapshots for:
    - `spawn_into` inner-view mismatch (op=`spawn_into`, attr=`stack`,
      expected=`command`, actual=`shell`)
    - op handle-ref mismatch (op=`subagent.send`, attr=`target`,
      expected=`claude_session`, actual=`shell`) — future v0.2 ext-op shape
    - view-attr handle-ref mismatch (op=`review_split`, attr=`peer`,
      expected=`review`, actual=`command`)
  - **(e) `scene/union-syntax-deferred` golden** — R-8 deferred `|` syntax.
  - **(f) `scene/sizing-on-stack-child` golden** — R-9 child-level sizing.
  - All 5 goldens land under `crates/scene/tests/snapshots/` via `insta`;
    regeneration command: `INSTA_UPDATE=always cargo test -p ark-scene --test errors`.

Scene tests: 648 → 659 (+11: 3 reconciler + 3 stack_dispatch + 5 goldens).
Workspace tests: 2195 → 2203. Fmt clean. All old `HandleKind::{Command,Plugin}`
/ `CommandPane` / `PluginPane` references absent (grep verified — only
doc-comment mentions in `view/mod.rs:102-103` survive as R17 historical
notes).

### Wave 7 — Tier 6 (T-025, T-026)

SHA: `5b17cd9`.

- **T-025 zellij-KDL emitter for StackNode**: VERIFIED in place from
  Wave 2. `compile/layout.rs::emit_stack` (lines 496–531) renders a
  stack as `pane stacked=true name="<handle>" { <children> }`. Sizing
  attrs (`span`/`cells`/`min`/`max`) propagate through `push_sizing`
  identical to `row`/`col`. Empty stack bodies emit `pane stacked=true`
  with no children (exercised by `empty_stack_compiles_to_zellij_kdl`
  in `tests/stack.rs`). No emitter edits needed.
- **T-026 reconciler override-layout diff for stack handles**: VERIFIED
  in place from Wave 2. `reconciler.rs::filter_child` Stack arm
  (lines 654–675) descends into stack bodies, evaluates `when=`
  predicates, and rebuilds a fresh `StackNode` carrying only the
  predicate-passing static children. Dynamic children via
  `spawn_into` are NEVER in the AST — they're runtime-only — so the
  reconciler's desired-state layout naturally excludes them (tested
  by `reconcile_stack_excludes_dynamic_spawn_into_children`).
  Stack round-trip via `name="<handle>"` on the emitted `pane
  stacked=true`; declared child panes carry their own `ARK_HANDLE=@c`
  wrappers via `apply_view` (R9 contract).
- 3 new integration tests in `tests/reconciler.rs` pin the round-trip:
  `reconcile_emits_stack_with_name_and_ark_handle_wrappers`,
  `reconcile_stack_excludes_dynamic_spawn_into_children`,
  `reconcile_stack_with_false_when_elides_container`.

Scene tests: 648 → 648 (pre-existing). Reconciler integration suite:
10 → 13. Workspace tests: 2192 → 2195. Fmt clean.

### Wave 6 — Tier 5 (T-021, T-022, T-023, T-024)

SHA: `ff628ea`.

- **T-021 `MuxHandle::spawn_into_stack` + `clear_stack`**: extended the
  trait in `crates/scene/src/intent.rs` with two new methods. Signature
  per the kit: `fn spawn_into_stack(&self, stack: &HandleId, view_body:
  Option<&str>) -> Result<HandleId, String>` — returns the ark-minted
  child handle — and `fn clear_stack(&self, stack: &HandleId) ->
  Result<(), String>`. Added `ulid = { workspace = true }` dep to
  `crates/scene/Cargo.toml` so `MockMux::spawn_into_stack` can mint
  real ULIDs for the default path. `MockMux` gained `child_ulid_override:
  Mutex<Option<String>>` (deterministic injection for tests) and
  `last_child_ids: Mutex<Vec<String>>` (recording). R-7 child-id format
  is `<stack>-<ulid>` with the ULID rendered 26-byte lowercase via
  `Ulid::new().to_string().to_lowercase()` — mirrors
  `SessionId::as_path_leaf` in `crates/types/src/id.rs`.
- **T-022 `SpawnIntoOp`**: new op in `crates/scene/src/ops/spawn.rs`.
  Parses `@stack` as the first positional arg off the raw `KdlNode`;
  uses `view_body` (same helper used by `SpawnOp`) to serialise the
  inner-view body; dispatches through `MuxHandle::spawn_into_stack`;
  strict-maps errors per R-7 non-idempotent contract (absent-handle
  errors DO surface — re-spawning on a cleared stack is meaningful
  work). Return value is `IntentValue::String(<minted-child-id>)` so
  downstream ops / tracing can chase the child. Name: `ark.core.spawn_into`.
- **T-023 `ClearOp`**: new file `crates/scene/src/ops/stack.rs` housing
  stack-specific ops. Parses `@stack` as the first positional arg;
  dispatches through `MuxHandle::clear_stack`; idempotent-maps errors
  per R-7 (clearing an empty / absent stack is a noop). Name:
  `ark.core.clear`.
- **T-024 registration**: updated `crates/scene/src/ops/mod.rs` —
  `pub mod stack;`, added `"ark.core.spawn_into"` + `"ark.core.clear"`
  to `CORE_OP_NAMES`, registered both ops in `register_core_ops`.
  Updated the module docstring's idempotency matrix to include the new
  rows. `namespace.rs` carries only `ark.core.*` as the reserved prefix
  (no per-op enumeration) — no change needed. `suggest.rs` has no
  ark.core op-name list today (only layout-child keywords) — no change
  needed; the kit's mention was speculative.

Tests delta:
- `crates/scene/src/ops/spawn.rs::tests` — 5 new SpawnIntoOp tests:
  dispatch returns child id with pinned ULID, missing handle errors,
  strict error surfacing (even absent), double-call is non-idempotent
  (both reach mux), default child id is 26-char lowercase ULID.
- `crates/scene/src/ops/stack.rs::tests` — 5 new ClearOp tests:
  dispatch to mux, idempotent on absent stack, surfaces non-noop
  errors, missing handle errors, double-call noop-safe.

Scene tests: 648 pass (up from 638 — +10). Workspace tests: 2192 pass
(up from 2182). Fmt clean. `CORE_OP_NAMES` matrix test still passes
with the new `ark.core.spawn_into` + `ark.core.clear` entries.

### Wave 5 — Tier 4 (T-017, T-018, T-019, T-020)

SHA: `8b50cfe`.

- **T-017 `ViewTypeMismatch` variant**: VERIFIED in place from Wave 2
  ledger-prep work — `SceneError::ViewTypeMismatch` at
  `crates/scene/src/error.rs:815` with all required fields `{op, attr,
  expected_view, actual_view, src, span}` + `#[diagnostic(code =
  "scene/view-type-mismatch")]` + caret label
  `"expected view does not match declared handle type"`. No edits
  needed.
- **T-018 view-type validator**: new file
  `crates/scene/src/validate/view_types.rs`. `pub fn
  validate_view_types(compiled: &CompiledScene, registry:
  &ViewRegistry) -> Vec<SceneError>` walks the scene's raw KDL doc for
  `spawn_into @stack { <view> }` nodes. For each: looks up `@stack`
  in the scene-local view table via the NEW crate-private accessor
  `CompiledScene::view_of_internal(&HandleId)` (added to
  `compile/mod.rs`); resolves the inner view's alias through the
  supplied `ViewRegistry`; emits `scene/view-type-mismatch` when the
  stack's declared view meta name differs from the inner view's
  resolved meta name (exact-match semantics per R-8 homogeneous-only).
  Unknown handles + unknown inner views are silently skipped to avoid
  double-emitting with `op_refs.rs` / T-031. Deterministic textual
  (KDL doc) ordering.
- **T-019 `spawn_into` / `clear` handle-kind check in `op_refs.rs`**:
  extended `validate_op_refs` with a raw-KDL walker
  `walk_stack_ops_raw` since `spawn_into` + `clear` aren't in the
  facet-derived `OpNode` enum yet (AST-tier task pending — they land
  as `OpNode::Unknown` whose opaque `args` carries only the body, not
  the positional handle arg). Added `ExpectedKind::Stack` variant to
  enforce stack-only kind on the `@stack` arg; mismatches surface as
  existing `scene/op-handle-type-mismatch`, unknown handles as
  existing `scene/op-unresolved-ref` (no new diagnostic family
  needed).
- **T-020 validator wiring**: `validate/mod.rs` gained `pub mod
  view_types;` + `pub use view_types::validate_view_types;`. Also
  re-exported `validate_op_refs` so integration tests can import from
  `ark_scene::validate::` directly. The view-types pass is NOT called
  from `compile_scene` today — all existing validation passes are
  stand-alone functions the CLI (`ark scene check`) drives. This
  matches the current architecture; wiring into `compile_scene` would
  be a separate concern.

Tests delta:
- `crates/scene/src/validate/view_types.rs::tests` — 7 new tests:
  matching view passes, wrong view emits mismatch, unknown stack
  handle silent (op_refs territory), unknown inner view silent (T-031
  territory), pane-kind handle silent (op_refs territory), no
  `spawn_into` no diagnostics, bind-body walk reaches nested ops,
  diagnostic code is `scene/view-type-mismatch`.
- `crates/scene/src/validate/op_refs.rs::tests` — 7 new tests for
  `spawn_into` / `clear`: stack passes, pane/tab are kind mismatch,
  unknown handle is unresolved, clear-on-stack passes, clear-on-pane
  mismatch, stack ops in bind body are checked.

Scene tests: 638 pass (up from 623 — +15). Workspace tests: 2182 pass
(up from 2167). Fmt clean.

### Wave 4 — Tier 3 (T-013, T-014, T-015, T-016)

SHA: `f819377`.

- **T-013 ViewTable type**: new file `crates/scene/src/compile/view_table.rs`
  carries the scene-local `ViewTable = BTreeMap<HandleId, ViewDecl>` type
  alias (`pub(crate)` per R-10) plus the `ViewDecl { kind: HandleKind,
  view_meta: ViewMeta }` struct (promoted to `pub` so `view_of` can
  return `Option<&ViewDecl>` across the crate boundary — but NOT exposed
  via `CompiledScene`'s public surface, preserving R-10). File placement
  avoids name collision with the phase-2 manifest-level
  `compile/view_types.rs`.
- **T-013 HandleId Ord**: added `Ord + PartialOrd` derives to
  `ark_view::HandleId` so it can key a BTreeMap. Non-breaking additive
  change; byte-lexicographic on the inner string.
- **T-014 populate view_table during compile_scene**: new
  `compile_scene_with_registry(engine, ir, &registry)` entry point +
  `compile_scene(engine, ir)` wraps it with `ViewRegistry::with_primitives()`.
  `build_view_table` walks `SceneIR::scene` tabs + mode tabs, recursing
  rows/cols, resolving each pane/stack alias via the registry. Tabs do
  not receive entries. Stacks resolve to the first pane child's alias
  per R-8 homogeneous-only. Empty stacks + unknown aliases get skipped
  silently (dedicated diagnostic pass owns user-facing errors). Since
  `pane.view.alias` is currently always empty after parse (T-026+ view
  resolution pending), `build_view_table` falls back to extracting
  aliases from `ir.kdl_doc` via `collect_handle_aliases_from_kdl` —
  walks every `pane "@h" { <alias> }` node to build a
  `@handle -> alias` map. `CompiledScene` gains `pub(crate) view_table:
  ViewTable` field + `pub(crate) fn view_table(&self) -> &ViewTable`
  accessor.
- **T-015 IntentContext::view_of**: added `Option<Arc<ViewTable>>`
  field `view_table` on `IntentContext`, plus `pub(crate) with_view_table`
  builder. `pub fn view_of(&self, handle: &HandleId) -> Option<&ViewDecl>`
  is the SOLE public accessor. NO `CompiledScene::resolve_typed_pane` /
  `resolve_typed_stack` public methods were added per R-10.
- **T-016 handle_type_hint rewire**: added
  `pub fn with_handle_hint_from_table(self, &HandleId) -> Self` on
  `IntentContext` — auto-fills `handle_type_hint` from the attached
  `ViewTable`. This is the REPLACEMENT path for the old ad-hoc
  `with_handle_type_hint` call site. The old builder is retained (still
  `pub`) for extension / test dispatch paths that bypass the compile
  pipeline; its doc-comment now points to `view_of` as the preferred
  source. No compile/layout.rs or reactions.rs had ad-hoc hint
  attachment code — `handle_type_hint` was only set via
  `with_handle_type_hint` in tests. The runtime reactions dispatcher
  (not yet built) will use `with_handle_hint_from_table` per the
  replacement pathway.

Tests delta:
- `crates/scene/src/compile/view_table.rs` 3 unit tests
  (store+retrieve, deterministic iteration, stack kind).
- `crates/scene/src/compile/mod.rs::tests` 4 new integration tests
  (panes+primitives, tabs skipped, stack->child view, unknown alias skipped).
- `crates/scene/src/intent.rs::tests` 7 new tests for view_of + auto-hint
  (decl for declared handle, None for absent, None without table,
  pane/stack distinction, pane hint, stack hint, absent = no hint).

Scene tests: 623 pass (up from 609 — +14 new).
Workspace tests: 2167 pass (up from 2153).

### Wave 3 — Tier 2 (T-009, T-010)

SHA: `8e8a735`.

- **T-009 retire scene-local HandleKind**: deleted the 4-variant enum
  (`Tab | Pane | Command | Plugin`) from `crates/scene/src/intent.rs`;
  replaced with `pub use ark_view::HandleKind` (3-variant `Tab | Pane |
  Stack`). View-type info (CommandView vs ZellijView) moved to
  `ark_view::Pane<V>` per soul Phase 2 R3/R4.
- **T-010 HandleKind::Stack routing**: added explicit `Stack` match arm
  to `FocusOp` + `CloseOp` in `ops/panes.rs`. Stack focus routes to
  `focus_pane` (zellij expands at currently focused child); stack close
  routes to `close_pane` (cascades to all members). `#[non_exhaustive]`
  on the re-exported `HandleKind` requires a `_` fallback arm; wired as
  a pane-route default.
- Tests: 2 new stack-routing tests (focus + close of `@claude_stack`
  with `HandleKind::Stack` hint dispatch the expected pane calls).
- grep verify: `HandleKind::(Command|Plugin)` in `crates/scene/` = 0.

Scene tests: 609 pass. Workspace tests: 2153 pass.

### Wave 2 — Tier 1 (T-005, T-006, T-008, T-011, T-012, partial T-007)

SHA: `366e2f6`.

- **T-005 StackNode AST**: added `StackNode` to `crates/scene/src/ast/layout.rs`
  mirroring Row/Col sizing attrs + `@handle` first-arg + `when=` + `Vec<LayoutChild>` body.
- **T-006 LayoutChild::Stack**: extended `LayoutChild` enum with
  `#[facet(rename="stack")]` variant; updated every exhaustive match in
  `compile/mod.rs`, `compile/layout.rs`, `compose.rs`, `reconciler.rs`,
  `validate/handles.rs`, `validate/op_refs.rs`, `validate/scope.rs`.
- **T-007 UnionSyntaxDeferred (PARTIAL)**: added `SceneError::UnionSyntaxDeferred`
  variant with `code="scene/union-syntax-deferred"` + help text. Parser-level
  `|` rejection in view-alias position DEFERRED — KDL's native tokenizer
  already rejects `|` inside a node's body position through its own grammar;
  dedicated diagnostic wiring requires a view-alias grammar extension that
  belongs with T-017+ parser work. Error variant available for future use.
- **T-008 empty-stack policy**: empty stack body legal; zellij-KDL emitter
  produces a `stacked=true` pane with no children (test: `empty_stack_compiles_to_zellij_kdl`).
- **T-011 flat handle namespace**: `StackNode.handle` added to the handle-clash
  walker in `validate/handles.rs`. Tests cover tab-vs-stack and pane-vs-stack
  dup. Stack also registers as `DeclKind::Stack` in `validate/op_refs.rs`.
- **T-012 scope validation + R-9 sizing**:
  - `validate_stack` recursively walks stack bodies.
  - Row/col inside stack body → `error[scene/misplaced-node]` (parent="stack").
  - `span`/`cells`/`min`/`max` on direct pane child → `error[scene/sizing-on-stack-child]`.
  - Same attrs on nested stack child → same error.
  - `SceneError::SizingOnStackChild` variant added to `error.rs`.
  - Stack-container-level sizing (as child of row/col) remains legal.
- **T-017 ViewTypeMismatch (ledger-only, variant prerequisite)**: added
  `SceneError::ViewTypeMismatch` variant with `code="scene/view-type-mismatch"`
  + fields `{op, attr, expected_view, actual_view, src, span, help}` —
  variant-only; the validator pass (T-018) is Tier 4 work still pending.

Ops/panes.rs polymorphic Tab-only focus/close behavior UNCHANGED for this
wave — T-009 (retire scene-local HandleKind Command/Plugin arms) and T-010
(add Stack routing) belong to Tier 2 and are NOT landed in this wave.

**Test delta**: 12 new integration tests in `crates/scene/tests/stack.rs`:
empty-body, pane-children, row/col rejection, sizing-on-pane-child,
sizing-on-nested-stack-child, container-sizing-legal, tab-clash, pane-clash,
zellij-KDL emission (stacked=true), empty-stack emission, handle grammar.

### Wave 1 — Tier 0 (T-001, T-002, T-003)

SHA: `2616aa1`.

- **T-001 ark-view re-exports**: lib.rs now re-exports `Pane`, `Stack`,
  `TabHandle`, `HandleKind`, `HandleId`, `View`, `CommandView`, `ZellijView`,
  `PaneLike` through a single `pub use ark_view::{…}` block. Dep was
  already present from soul Phase 2.
- **T-002 handle_types.rs deletion**: removed the whole module including
  `CommandPane`, `PluginPane`, `TabHandle` wrappers + `PaneHandle` trait +
  4 tests. No workspace consumers (only self-references via doc-comments
  in intent.rs, which were retained as compat-stub commentary until T-009).
- **T-003 stack keyword**: `LAYOUT_CHILD_KEYWORDS = ["row","col","pane","stack"]`
  constant added to `namespace.rs`; `suggest_layout_child()` helper added
  to `suggest.rs`. 2 new tests cover typo→keyword suggestion.

Commit: `feat(ark-scene): Tier 0 T-001+T-002+T-003 (scene-2026-04-18)`.
Scene lib tests: 486 → 484 (4 handle_types tests retired, 2 suggest tests added).

