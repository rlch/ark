---
created: "2026-04-18T00:00:00Z"
last_edited: "2026-04-18T00:00:00Z"
---

# Build Site: Scene 2026-04-18 Revision (typed view-parametric handles + `stack`)

26 active tasks (T-001..T-027 with T-004 CUT) across 7 tiers from cavekit-scene.md.

**Patched 2026-04-18** per `phase-2-design-decisions.md` Resolutions section:
R-7 pins stack-child naming to `<stack-handle>-<ulid>` (ark-generated);
R-8 defers union syntax (`pane @h { A | B }`, `stack @h { A | B }`)
entirely to v0.2 — all union-parsing / `OneOf` / `UnionAlias` tasks CUT;
R-9 pins child-level sizing attrs on stack as `error[scene/sizing-on-stack-child]`;
R-10 keeps `view_table` private behind `IntentContext::view_of(&HandleId)` —
no `pub view_table` accessor.

Scene-crate scope for the 2026-04-18 revision: retire the
`CommandPane` / `PluginPane` types + narrow `HandleKind` to
`{ Tab, Pane, Stack }`; re-export parametric `Pane<V>` / `Stack<V>` +
`TabHandle` from `crates/ark-view` through the scene API; add the
`stack @h { … }` layout primitive (AST + facet-kdl parse + validator +
reconciler emission); add the `spawn_into @stack { <view> }` and
`clear @stack` ops; extend the view alias grammar so `pane @h { foo }`
/ `stack @h { foo }` declarations produce a scene-compile view-type
table (homogeneous-only per R-8; union syntax deferred to v0.2); wire
the compile pipeline to populate a private `view_table` keyed by
`Handle` so typed `Pane<V>` handles can be re-materialised at intent
dispatch via `IntentContext::view_of` (R-10); land the view-type
validator that walks typed handle refs in view attrs against the
declared handle-type and emits `error[scene/view-type-mismatch]` on
divergence; reject `|` in view-alias position with a clear
"union syntax deferred to v0.2" diagnostic; reject child-level
sizing attrs inside `stack` bodies with `error[scene/sizing-on-stack-child]`
(R-9); rewrite every test that assumed the old
`HandleKind::{Command,Plugin}` variants or the `CommandPane` /
`PluginPane` wrappers; add scene-only integration tests for stack
spawn/clear round-trips and view-type mismatch goldens (trybuild-level
mismatches belong to `cavekit-soul-phase-2-tests.md`, so this site
only carries scene-IR golden diagnostics).

**Cross-site deps:**

- **Blocked by:** `crates/ark-view` must already exist with `View` /
  `CommandView` / `ZellijView` traits, `Pane<V>`, `Stack<V>`,
  `TabHandle`, and narrowed `HandleKind` landed per soul Phase 2
  (`cavekit-soul-phase-2-ark-view.md` R1–R4). Host-dispatch +
  `#[derive(View)]` codegen from Phase 2 ext-surface / host-dispatch
  kits are also prerequisites for the macro-adjacent tasks here
  (T-013, T-017).
- **Unblocks:** claude-code extension build site — the ext consumes the
  scene-validated view-type table + `Pane<V>` re-export chain +
  `spawn_into @stack` op.
- **Cross-cleanup:** soul Phase 2 cleanup site handles the workspace
  `cargo check` gate; this site's final tier only guarantees
  `cargo test -p ark-scene --all-targets` green.

## Tier 0 — No Dependencies (Start Here)

| Task | Title | Kit | Requirement | Effort |
| --- | --- | --- | --- | --- |
| T-001 | Add `ark-view` dep to `crates/scene/Cargo.toml`; re-export `Pane`, `Stack`, `TabHandle`, `HandleKind`, `View`, `CommandView`, `ZellijView` from `crates/scene/src/lib.rs` under a single `pub use ark_view::…` block | scene | R6, R17 | S |
| T-002 | Delete `crates/scene/src/handle_types.rs` entirely (every type + test moves behind the `ark-view` re-export; no scene-local definitions remain) | scene | R17 | S |
| T-003 | Add `stack` keyword to the scene grammar's reserved-keyword list in `namespace.rs` / `suggest.rs`; add `"stack"` to the layout-child suggestion set so unknown-node diagnostics surface it alongside `row` / `col` / `pane` | scene | R3 | S |
| ~~T-004~~ | ~~Introduce `UnionAlias` type in `crates/scene/src/view/mod.rs`...~~ **CUT per R-8** — union syntax deferred to v0.2; no `UnionAlias` type, pane/stack declarations carry a single resolved `ViewMeta` ref | scene | R6 (deferred) | — |

## Tier 1 — AST + Parser for `stack` primitive

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-005 | Add `StackNode` struct to `crates/scene/src/ast/layout.rs` mirroring `RowNode`/`ColNode` sizing attrs + `@handle` first-arg + optional `when=` + heterogeneous `Vec<StackChild>` body | scene | R3 | T-003 | M |
| T-006 | Extend `LayoutChild` enum in `crates/scene/src/ast/layout.rs` with `Stack(StackNode)` variant; add `#[facet(rename = "stack")]` attr; update exhaustive matches across `compile/layout.rs` + `validate/handles.rs` + `validate/scope.rs` + `reconciler.rs` to handle the new arm | scene | R3 | T-005 | M |
| T-007 | Reject `|` in pane/stack view-alias position with `error[scene/union-syntax-deferred]`: emit a miette diagnostic pointing at the `|` token with help text "union syntax deferred to v0.2; declare a single view alias or wrap mode-switching in a `ViewMode` enum inside one view impl". Parser keeps single-alias `pane @h { foo }` / `stack @h { foo }` path; this task only adds the negative diagnostic for the deferred union form (R-8) | scene | R6 (deferred via R-8) | T-003 | S |
| T-008 | Empty-body stack policy: `stack @h { }` is legal and compiles to a homogeneous container whose view type is resolved at first `spawn_into`; assert this path does not trip the pane-single-view-child validator (which must skip stacks). Heterogeneous `Stack<dyn View>` semantics deferred with R-8; empty stack remains legal for dynamic-only population | scene | R3 | T-006 | S |

## Tier 2 — Narrowed `HandleKind` + Validator Rewrites

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-009 | Retire scene-local `HandleKind` in `crates/scene/src/intent.rs`; every use switches to the re-exported `ark_view::HandleKind`; delete the `Command` and `Plugin` variants and every `match` arm that referenced them (`ops/panes.rs` polymorphic `focus` / `close`) | scene | R7, R17 | T-001 | M |
| T-010 | Add `HandleKind::Stack` routing to the polymorphic `focus` / `close` ops in `ops/panes.rs`: stack-handle focus = expand stack at focused child; stack-handle close = close the whole container (R7 `focus @handle` / `close @handle` on stack) | scene | R7 | T-009 | M |
| T-011 | Extend `validate/handles.rs` to enforce the flat handle namespace across tab + pane + stack (R2) — duplicate `@h` across any combination = `error[scene/handle-clash]`; add tests for tab-vs-stack and pane-vs-stack clashes | scene | R2 | T-006 | S |
| T-012 | Extend `validate/scope.rs` to allow `stack` inside `tab` / `row` / `col` / nested `stack`; reject bare `stack` at layout root and inside `pane`; update `error[scene/misplaced-node]` tests. Also add `error[scene/sizing-on-stack-child]` variant to `SceneError` and reject `span` / `cells` / `min` / `max` attrs on any direct child of a `stack` body (pane or nested stack) per R-9 — stack container itself keeps those attrs; only children are forbidden. Miette diagnostic caret points at the offending attr token with help text "stack children cannot declare sizing; zellij owns expand/collapse stacking. Move sizing attrs to the enclosing stack or its parent row/col" | scene | R2, R-9 | T-006 | M |

## Tier 3 — View-Type Table + Compile Pipeline

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-013 | Introduce `ViewTable` in `crates/scene/src/compile/mod.rs`: `BTreeMap<HandleId, ViewDecl>` where `ViewDecl` carries `(HandleKind, ViewMeta)` — homogeneous only per R-8; pane → `ViewDecl` with single `ViewMeta`; stack → `ViewDecl` with single `ViewMeta` (child view type, even if the stack body is empty and resolution is deferred to first `spawn_into`); tab → no entry. Type is `pub(crate)` — does not appear on `CompiledScene`'s public surface per R-10 | scene | R6, R17 | T-006 | L |
| T-014 | Populate `view_table` during `compile_scene` by walking `SceneIR::scene` tabs + layout children; resolve each pane + stack view alias via the `ViewRegistry` (T-026 shipped/primitive resolution already landed); store the result on `CompiledScene` as `pub(crate) view_table: ViewTable` — PRIVATE per R-10, no `pub view_table` surface | scene | R17, R-10 | T-013 | M |
| T-015 | Add `IntentContext::view_of(&HandleId) -> Option<&ViewDecl>` as the sole public accessor per R-10 — reactions dispatcher at intent dispatch re-materialises `Pane<V>` from opaque wire handle IDs via this accessor. Do NOT add `CompiledScene::resolve_typed_pane` or `resolve_typed_stack` public methods; internal compile-pipeline lookups can use a crate-private helper | scene | R17, R-10 | T-014 | M |
| T-016 | Replace `IntentContext::handle_type_hint: Option<HandleKind>` wiring in `compile/layout.rs` / `reactions.rs` so the hint is sourced from the private `view_table` lookup (`ctx.view_of(&handle)?.kind`) rather than an ad-hoc per-handle attachment; remove the old hint-attachment pathway | scene | R7, R17, R-10 | T-014, T-015 | M |

## Tier 4 — View-Type Validator + Attr Reference Checks

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-017 | Add `error[scene/view-type-mismatch]` variant to `SceneError` in `crates/scene/src/error.rs` with fields `{op, attr, expected_view, actual_view, src, span, help}`; wire the miette diagnostic with a caret at the offending `@handle` token | scene | R6, R7 | T-013 | S |
| T-018 | New pass `crates/scene/src/validate/view_types.rs`: walks every op body + every view attr that declares a typed handle ref (per view's facet SHAPE); looks up the referenced `@handle` in the private `view_table` (via a crate-private helper, NOT the `IntentContext::view_of` accessor which is runtime-only); emits `scene/view-type-mismatch` when the declared expected view does not match the resolved `ViewDecl` — exact-match semantics only per R-8 (union/OneOf/heterogeneous compat deferred to v0.2) | scene | R6, R7, R-8, R-10 | T-013, T-017 | L |
| T-019 | Extend `validate/op_refs.rs` so `spawn_into @stack { <view> }` validates the inner view against the stack's declared view type via `ViewTable`; out-of-type inner view = `scene/view-type-mismatch`; unknown `@stack` handle = existing `scene/op-unresolved-ref` | scene | R7 | T-013, T-017 | M |
| T-020 | Register `validate_view_types` in `validate/mod.rs` and run it from `parse.rs` / `compile_scene` after `ViewTable` population; ensure deterministic diagnostic ordering (stable by source span) | scene | R6, R7 | T-018 | S |

## Tier 5 — New Ops (`spawn_into`, `clear`)

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-021 | Extend the `MuxHandle` trait in `intent.rs` with `spawn_into_stack(stack: &HandleId, view_body: Option<&str>) -> Result<HandleId, String>` (returns the ark-generated child handle) + `clear_stack(stack: &HandleId) -> Result<(), String>`; update `MockMux` in tests to record calls. Child handle naming pinned per R-7: ark auto-generates `<stack-handle>-<ulid>` (lowercase 26-byte ULID of the per-call timestamp-random) — extensions do NOT name children; they receive the generated `Pane<V>` handle as the return value of the op. Mirrors `SessionId::as_path_leaf()` `<name>-<ulid>` pattern; collision-free under async races; sortable | scene | R7, R-7 | T-009 | M |
| T-022 | Implement `SpawnIntoOp` in `crates/scene/src/ops/spawn.rs`: parses `@stack` first arg + inner `{ <view> <attrs> }` body; dispatches via `MuxHandle::spawn_into_stack` (which returns the generated `<stack>-<ulid>` child handle per R-7); strict-maps errors (non-idempotent per R7 — re-spawning on a cleared stack is meaningful work); the returned handle is the dynamic child's identity — not recorded in the scene's `view_table` (which is compile-time only) | scene | R7, R-7 | T-021 | M |
| T-023 | Implement `ClearOp` in `crates/scene/src/ops/control.rs` (or `ops/stack.rs` if created): parses `@stack` first arg; dispatches via `MuxHandle::clear_stack`; idempotent per R7 semantics — clearing an empty stack is a noop | scene | R7 | T-021 | S |
| T-024 | Register `spawn_into` + `clear` in `register_core_ops` (`crates/scene/src/ops/mod.rs`); update the `ark.core.*` namespace list in `namespace.rs` and the op-name suggestion list in `suggest.rs`; add unit tests covering dispatch round-trip via `MockMux` | scene | R7 | T-022, T-023 | S |

## Tier 6 — Reconciler + Layout Emission

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-025 | Extend `compile/layout.rs`'s zellij-KDL emitter so `StackNode` renders as a zellij pane stack (`pane stacked=true { … }` or the equivalent zellij 0.44.1 syntax confirmed by existing `compile/layout.rs` emission code); sizing attrs (`span` / `cells` / `min` / `max`) propagate identically to `row` / `col`; empty stack emits a zellij pane stack with no children | scene | R3 | T-006 | L |
| T-026 | Extend `reconciler.rs` override-layout diff path so stack handles round-trip via the same `env ARK_HANDLE=@<h>` wrapper as panes (R9); dynamic children spawned via `spawn_into` are *not* put through override-layout (they live outside the desired-state rendering); static children declared in scene continue to reconcile | scene | R9 | T-025 | M |

## Tier 7 — Completion Gate

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-027 | Completion gate: `cargo test -p ark-scene --all-targets` green; new integration tests cover (a) `stack @h { foo }` round-trip through parse → compile → layout-KDL emission, (b) `spawn_into @stack { foo attrs }` dispatch via `MockMux` records the expected call AND returns a `<stack>-<ulid>` child handle per R-7, (c) `clear @stack` dispatch records the expected call, (d) view-type-mismatch goldens for `spawn_into` inner view / op handle-ref mismatch / view-attr handle-ref mismatch — three miette snapshot tests pinning `error[scene/view-type-mismatch]` output, (e) `error[scene/union-syntax-deferred]` golden for `pane @h { foo | bar }` per R-8, (f) `error[scene/sizing-on-stack-child]` golden for `stack @h { pane @c span=2 { … } }` per R-9; every rewritten test (old `HandleKind::{Command,Plugin}` / `CommandPane` / `PluginPane`) stays green on the new surface | scene | R3, R6, R7, R17, R-7, R-8, R-9 | T-008, T-010, T-011, T-012, T-016, T-019, T-020, T-024, T-026 | M |

## Completion Gate

- `cargo check -p ark-scene --all-targets` exits 0.
- `cargo test -p ark-scene --all-targets` exits 0.
- Scene-only integration tests green:
  - `stack @h { foo }` parse → compile → zellij-KDL emission round-trip.
  - `spawn_into @stack { foo attrs }` dispatch records mux call via `MockMux`.
  - `clear @stack` dispatch records mux call via `MockMux`.
- Miette golden snapshots pin `error[scene/view-type-mismatch]` output for
  three cases: `spawn_into` inner-view mismatch, op `@handle` arg
  mismatch, view-attr `@handle` ref mismatch. (Compile-time `#[ark::intent]`
  trybuild goldens live in `cavekit-soul-phase-2-tests.md`, not here.)
- Miette golden snapshot pins `error[scene/union-syntax-deferred]` output
  for `pane @h { foo | bar }` per R-8.
- Miette golden snapshot pins `error[scene/sizing-on-stack-child]` output
  for `stack @h { pane @c span=2 { … } }` per R-9.
- Zero `HandleKind::Command|Plugin` hits workspace-wide under `crates/scene/`.
- Zero `scene::handle_types::CommandPane|PluginPane` hits workspace-wide.
- `ark-view` re-export compiles through scene: `use ark_scene::{Pane, Stack,
  TabHandle, View, CommandView, ZellijView};` resolves.
- `view_table` populated (via `IntentContext::view_of` lookups) for every
  handle declared in fixture scenes under `crates/scene/fixtures/` that
  declare panes or stacks; no `CompiledScene::view_table` public accessor
  exists per R-10.
- `UnionAlias`, `pub view_table`, and `resolve_typed_pane` /
  `resolve_typed_stack` symbols absent from `crates/scene/` (deferred via
  R-8 / forbidden via R-10).

## Summary

| Tier | Tasks | Effort |
| --- | ---: | --- |
| 0 | 3 (1 CUT: T-004) | 3S |
| 1 | 4 | 2M + 2S |
| 2 | 4 | 3M + 1S (T-012 upgraded S→M per R-9 sizing validator) |
| 3 | 4 | 1L + 3M |
| 4 | 4 | 1L + 2M + 1S |
| 5 | 4 | 2M + 2S |
| 6 | 2 | 1L + 1M |
| 7 | 1 | 1M |
| **Total** | **26 active + 1 cut (T-004)** | **3L + 13M + 10S** |

Post-patch effort sums: 3L + 13M + 10S = 26 active tasks (T-004 CUT per R-8).

## Ambiguous Kit Points (flagged rather than invented)

All four originally-flagged ambiguities **resolved 2026-04-18** by
`context/plans/phase-2-design-decisions.md` Resolutions section. Kept
here as historical record; no unresolved items remain.

- **RESOLVED (R-7):** Dynamic-child handle generation for `spawn_into`
  (T-021) — ark auto-generates `<stack-handle>-<ulid>` on every
  `spawn_into` / `stack.spawn_pane()` call (lowercase 26-byte ULID,
  timestamp-random). Extensions do NOT name children; they receive the
  generated `Pane<V>` handle as the op return value. Collision-free
  under async races; sortable; mirrors `SessionId::as_path_leaf()`
  `<name>-<ulid>` pattern. See `phase-2-design-decisions.md` R-7.
- **RESOLVED (R-8):** Union syntax member count — moot; union syntax
  (`A | B`) DEFERRED entirely to v0.2. Scene ships homogeneous-only for
  v0.1 (`pane @h { foo }`, `stack @h { foo }`). Parser emits
  `error[scene/union-syntax-deferred]` on encountering `|`. T-004 /
  T-007's original scope CUT accordingly. See
  `phase-2-design-decisions.md` R-8.
- **RESOLVED (R-9):** Stack sizing sibling policy — `span` / `cells` /
  `min` / `max` on `stack` container govern its share of the parent
  row/col; child-level sizing attrs inside stack bodies = hard compile
  error `error[scene/sizing-on-stack-child]` (zellij owns expand/collapse
  for stacked children). T-012 extended to enforce. See
  `phase-2-design-decisions.md` R-9.
- **RESOLVED (R-10):** `view_table` accessor shape — PRIVATE.
  `CompiledScene::view_table` is `pub(crate)` only. Runtime access via
  `IntentContext::view_of(&HandleId) -> Option<&ViewDecl>` as the sole
  public accessor. No `resolve_typed_pane` / `resolve_typed_stack`
  surface. Widen later if `ark scene debug` tooling needs it. See
  `phase-2-design-decisions.md` R-10.
