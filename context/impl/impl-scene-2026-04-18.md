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
| T-017 | 4 | PENDING | No `ViewTypeMismatch` variant in `SceneError`. |
| T-018 | 4 | PENDING | No `validate/view_types.rs` scene-local validator (the Phase-2 `view_types.rs` in `compile/` is manifest-level and different). |
| T-019 | 4 | PENDING | validate/op_refs.rs has no `spawn_into` stack inner-view check. |
| T-020 | 4 | PENDING | `validate_view_types` not registered. |
| T-021 | 5 | PENDING | `MuxHandle` trait has no `spawn_into_stack` / `clear_stack`. |
| T-022 | 5 | PENDING | No `SpawnIntoOp` in ops/spawn.rs. |
| T-023 | 5 | PENDING | No `ClearOp`. |
| T-024 | 5 | PENDING | spawn_into/clear not registered. |
| T-025 | 6 | PENDING | compile/layout.rs emitter has no `StackNode` case. |
| T-026 | 6 | PENDING | reconciler.rs no stack round-trip. |
| T-027 | 7 | PENDING | Completion gate tests not written. |

**Audit summary:** 1 PARTIAL (T-001), 25 PENDING. ZERO tasks genuinely
DONE via phase-2. Prior audit was wrong.

## Implementation waves

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

