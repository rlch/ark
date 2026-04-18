---
created: "2026-04-18T00:00:00Z"
last_edited: "2026-04-18T00:00:00Z"
---

# Build Site: Cavekit Soul Phase 2

42 tasks across 9 tiers from 4 sub-kits.

Phase 2 ships the extension surface needed before Phase 4's
`extensions/claude-code/` can be written. Scope decomposes into four
sibling sub-kits: (1) `ark-view` — the new `crates/ark-view` crate that
owns `View`/`CommandView`/`ZellijView` traits, parametric `Pane<V>` /
`Stack<V>` + typed `TabHandle`, narrowed `HandleKind`, six `pane/*` +
`stack/*` ext→host RPC methods, three-cause invalidation taxonomy, and
user-close suppression policy; (2) `ext-surface` — lifecycle + feature-
group hook methods on `ArkExtension`, manifest surface (`views`,
`config_sections`, `reload_gates`), `#[derive(View)]` codegen, v1
capability-flag taxonomy (8 flags), and the `1.0 → 1.1` proto bump;
(3) `host-dispatch` — ark-side fan-in: `ark list` columns, `ark doctor`
check runner, figment config-section layering, scene compile-time view-
type validator, reload-gate dispatcher, capability-aware RPC dispatch,
handshake host-capability advertisement, extension load sequence, and
supervisor-owned `closed_by_user` suppression map; (4) `tests` — shared
in-proc + NDJSON subprocess stub-ext harness, version-mismatch matrix,
capability-gate matrix, `trybuild` view-type goldens, manifest-intent
integration tests, suppression + invalidation tests, and workspace CI
integration.

## Cross-Site Dependency Notice

Phase 2 is foundational. It does NOT depend on any sibling build site
landing first (scene-2026-04-18, cleanup Phase 4+5 deletions,
claude-code extension). Phase 2 completion UNBLOCKS:

- **scene-2026-04-18 landing** — scene crate consumes `ark-view`'s
  `View` / `Pane<V>` / `Stack<V>` types via the R4 view-type validator
  (this site ships the types; sibling site wires scene to consume them).
- **claude-code extension** — first consumer of lifecycle hooks,
  manifest-driven intents, typed handles, and capability flags.

Scene-crate typed-handle *consumption* (the reconciler's
`closed_by_user` read-path against `ark-view`'s policy) and
claude-code extension authoring are explicitly OUT OF SCOPE for this
site. Phase 4/5 cleanup (ACP residue, cavekit orchestrator removals) is
owned by the cleanup sibling site.

## Tier 0 — No Dependencies (Start Here)

| Task | Title | Kit | Requirement | Effort |
| --- | --- | --- | --- | --- |
| T-001 | Create `crates/ark-view` workspace member with minimal dep budget (`facet`, `serde`, `serde_json`, `thiserror`) + add to root `Cargo.toml` members | ark-view | R1 | M |
| T-002 | Delete `intent_register` RPC method (+ any `register_intents` hook) from `ArkExtension` trait in `crates/ark-ext-proto/src/lib.rs`; confirm `permission_dispatcher` never introduced | ext-surface | R3 | S |
| T-003 | Extend `ExtensionMetadata` at `crates/ark-ext-metadata-types/src/lib.rs:82` with `views`, `config_sections`, `reload_gates` Vec fields (facet kdl::children, default); add `ConfigSectionDecl` + `ReloadGateDecl` structs with `name` argument | ext-surface | R4 | M |
| T-045 | Extend `ark_types::event::CoreEvent::SessionEnded` at `crates/types/src/event.rs:63` with `exit: ExitReason` field alongside existing `terminated_at`; define `pub enum ExitReason { Normal, Error(String), Cancelled }` in the same module with `#[non_exhaustive]` + stable snake_case serde tag; update `FlatEvent` projection to include `exit` in payload; update all existing `SessionEnded { terminated_at }` construction sites in-tree; per phase-2-design-decisions.md §R-5 | ext-surface | R1 (payload) | S |

## Tier 1 — ark-view Type Primitives

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-004 | Define `HandleKind { Tab, Pane, Stack }` enum in `crates/ark-view/src/handle.rs` with stable lowercase tag serde + `Copy+Eq+Hash+Debug` + `#[non_exhaustive]` | ark-view | R2 | T-001 | S |
| T-005 | Define `HandleId` opaque string newtype in `crates/ark-view/src/handle.rs` with `Facet` derive + transparent string serde | ark-view | R5 | T-001 | S |
| T-006 | Define `View` base marker trait + `CommandView: View` + `ZellijView: View` refining markers in `crates/ark-view/src/view.rs`; `Send + Sync + 'static` bounds | ark-view | R3 | T-001 | S |
| T-007 | Define `InvalidationCause { UserClosed, SceneReloadDropped, SessionEnded }` in `crates/ark-view/src/invalidation.rs` with stable snake_case tag serde + `#[non_exhaustive]` + `Facet` derive | ark-view | R7 | T-001 | S |

## Tier 2 — ark-view Typed Wrappers + Policy Types

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-008 | Define `Pane<V: View>` + `Stack<V: View>` structs (opaque `Handle` + `PhantomData<V>`) + non-parametric `TabHandle` in `crates/ark-view/src/typed.rs`; serde collapses to HandleId string | ark-view | R4 (core types) | T-004, T-005, T-006 | M |
| T-009 | Define `PaneLike` trait with `handle()` + typed `emit<E: Event>` surface; impl `PaneLike for Pane<V>` and `PaneLike for Stack<V>` with polymorphic `&dyn PaneLike` tests | ark-view | R4 (PaneLike) | T-008 | M |
| T-010 | Add marker-gated affordance blocks: `impl<V: CommandView> Pane<V>` (`env`, `write_stdin`, `pid`) + `impl<V: ZellijView> Pane<V>` (`pipe`); negative trybuild asserting methods out-of-scope for wrong markers | ark-view | R4 (marker-gated) | T-008 | M |
| T-011 | Add `impl<V: View> Stack<V>` with `spawn_pane(attrs) -> Pane<V>`, `close_child(&Pane<V>)`, `children() -> Vec<Pane<V>>`, `clear()` | ark-view | R4 (Stack methods) | T-008 | M |
| T-012 | Define `ParamsHash` newtype (`[u8; 32]`) + blake3 canonical-JSON hash function in `crates/ark-view/src/suppression.rs`; document algorithm | ark-view | R8 (hash) | T-001 | S |
| T-013 | Define `SceneHandleName` newtype + `SuppressionPolicy` contract type in `crates/ark-view/src/suppression.rs` with six invariants in doc-comment; stack-child exclusion documented + debug-assert on stack-child insert | ark-view | R8, R9 | T-012, T-007 | M |

## Tier 3 — ark-view Invalidation, Lookup, Derive Surface

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-014 | Add `HandleGone { handle, cause }` variant to `ExtensionError` in `crates/ark-ext-proto/src/lib.rs`; document contract: op against invalidated handle MUST return HandleGone (not method_not_found) | ark-view | R7 (HandleGone) | T-007 | M |
| T-015 | Specify `ark.handle.invalidated { handle, cause }` ExtEvent wire shape; golden test for JSON payload format in `crates/ark-view/tests/` | ark-view | R7 (event wire) | T-005, T-007 | S |
| T-016 | Define `SessionHandles` name-lookup API (`pane_by_name<V>`, `stack_by_name<V>`, `tab_by_name`); returns `None` for suppressed/absent; V-mismatch returns `None` + warn-log; zero-RPC-call test | ark-view | R10 | T-008, T-013 | M |
| T-017 | Ensure `ark-view` public exports (View, CommandView, ZellijView, Pane, Stack, TabHandle, PaneLike, HandleKind, InvalidationCause, HandleId) are crate-root visible; add `ark-view` dep to `crates/ark-ext-proto/Cargo.toml` + `crates/scene/Cargo.toml`; add re-exports from `ark-ext-proto` | ark-view | R11, R1 (deps) | T-008, T-014 | S |

## Tier 4 — Ext→Host RPC Methods + Ext-Surface Hooks

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-018 | Define six ext→host request+response struct pairs in `crates/ark-ext-proto/src/` (`PaneEmit`, `PaneReplaceView`, `PaneClose`, `StackSpawnPane`, `StackCloseChild`, `StackClear`) with `Facet`+`Debug`+`Clone` + doc-comments on every field; JSON-RPC method names `pane/emit`, `pane/replace_view`, `pane/close`, `stack/spawn_pane`, `stack/close_child`, `stack/clear` | ark-view | R6 | T-005, T-008, T-017 | L |
| T-019 | Add six default-`method_not_found` trait methods on `ArkExtension` for the R6 RPC surface matching existing `lib.rs:1078-1086` convention; `StackSpawnPaneResponse.handle: HandleId` | ark-view | R6 (trait defaults) | T-018 | M |
| T-020 | Add `on_session_start(&self, spec: &SessionSpec)` + `on_session_end(&self, spec: &SessionSpec, exit: &ExitReason)` lifecycle methods on `ArkExtension`; `ExitReason` is the enum pinned by T-045 (`{ Normal, Error(String), Cancelled }`) imported from `ark_types::event`; default bodies return `method_not_found`; request/response structs (`SessionStart{Request,Response}`, `SessionEnd{Request,Response}` — the latter carries `exit: ExitReason`) exist with `Facet+Debug+Clone`; per phase-2-design-decisions.md §R-5 | ext-surface | R1 | T-002, T-045 | M |
| T-021 | Add `scene_compile_hook`, `control_verbs`, `doctor_checks`, `list_columns` feature-group hook methods on `ArkExtension` with dedicated request+response structs; default `method_not_found` | ext-surface | R2 | T-002 | M |
| T-022 | Retain `intent_dispatch` RPC on `ArkExtension` (forbid removal in Phase 2); pin loader-owned registration contract on `IntentDecl.name + args_schema` in `crates/ark-ext-metadata-types/` | ext-surface | R5 | T-003 | S |
| T-023 | Extend `ViewDecl` at `crates/ark-ext-metadata-types/src/lib.rs:417` with `kind: HandleKind`-equivalent field (string discriminant since ark-view lives below metadata); roundtrip test for `extension.kdl` parse | ext-surface | R4 (ViewDecl extend) | T-003, T-004 | M |

## Tier 5 — Capability Taxonomy + Derive Codegen

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-024 | Add v1 capability-flag taxonomy as module-level doc/const in `crates/ark-ext-proto/src/lib.rs`: exactly eight flags (`view.pane.v1`, `view.stack.v1`, `ext.lifecycle.v1`, `ext.scene_compile_hook.v1`, `ext.control_verbs.v1`, `ext.doctor.v1`, `ext.list_columns.v1`, `ext.reload_gate.v1`); set-equality test asserts no drift | ext-surface | R6 | T-019, T-020, T-021 | M |
| T-025 | Add `#[proc_macro_derive(View)]` + `#[ark_view(name = "…")]` attribute family to `crates/ark-ext-derive/src/lib.rs`; emits `inventory::submit! ViewRegistration { kind }` + ViewDecl manifest entry; name override parity with `#[ark_intent]` | ext-surface | R7 (derive View) | T-023 | L |
| T-026 | Add `#[derive(CommandView)]` + `#[derive(ZellijView)]` marker-only codegen; co-derived `#[derive(View)]` stamps the correct `HandleKind` discriminant | ext-surface | R7 (markers) | T-025, T-006 | M |
| T-027 | Add auto-capability-advertisement in `#[derive(Extension)]`: scans inventory for View registrations → emits `view.pane.v1` / `view.stack.v1`; scans `ArkExtension` impl block method names for lifecycle/doctor/etc → emits corresponding `ext.*.v1` flags; document hand-authored fallback caveat | ext-surface | R7 (auto-advertise) | T-024, T-025 | L |

## Tier 6 — Host-Side Fan-In + Dispatcher

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-028 | Build capability→method static mapping table + capability-aware RPC dispatcher in `crates/supervisor/src/` that records `InitializeResponse.extension_capabilities` per-session; skip-call on absent capability (no log); warn-once on advertised-but-`method_not_found` | host-dispatch | R6 | T-019, T-020, T-021, T-024 | L |
| T-029 | Populate `InitializeRequest.client_capabilities` at handshake with deterministic sorted set of every Phase-2 capability ark supports; constant at supervisor startup; identical across concurrent handshakes | host-dispatch | R7 | T-024 | M |
| T-030 | Implement extension load sequence in `crates/supervisor/src/`: (1) read manifest, (2) handshake, (3) validate capabilities, (4) register intents via `IntentRegistry` shims, (5) register views, (6) register gates, (7) Ready; failed exts don't block peers; stub-harness-driven step-order test via log capture | host-dispatch | R8 | T-022, T-023, T-028, T-029 | L |
| T-031 | Rewrite `crates/cli/src/commands/list.rs` to iterate extension registry + collect `list_columns` via dispatcher; core `id/name/status` columns at positions 0-2; extension columns alpha-by-ext + declaration-order stable; capability-gated; `--json` respects ordering | host-dispatch | R1 | T-028 | M |
| T-032 | Rewrite `crates/cli/src/commands/doctor.rs` to iterate extensions + collect `doctor_checks` via dispatcher; per-ext per-check `ok/warn/fail/skipped`; fail ↔ non-zero exit; panic isolation; `--json` row shape `{group, check_id, status, message}`; capability-gated | host-dispatch | R2 | T-028 | M |
| T-033 | Add figment config-section loader in `crates/config/src/lib.rs`: layers ext sections under `extension.<ext-name>.<section>`; facet SHAPE deserialisation; required-missing fails boot with named error; optional-missing → default | host-dispatch | R3 | T-003 | M |
| T-034 | Build view-type symbol table in `crates/scene/src/compile/` from extension manifest set; validate `pane @x { view: … }` + `stack @x { … }` against table; locatable errors with file+line+column; reproducible manifest-set hash via `blake3` over canonical-JSON of sorted-by-ext manifest entries (figment cache + scene-compile cache key); add `blake3` to workspace `[dependencies]` if absent and re-export from `ark-ext-metadata-types` alongside `ParamsHash`'s algo (T-012); unknown-view + shape-mismatch + hash-stability test cases; per phase-2-design-decisions.md §R-6 | host-dispatch | R4 | T-008, T-023, T-030 | L |
| T-035 | Wire reload-gate dispatcher in `crates/supervisor/src/scene_runtime.rs`: iterate advertised gates; AND `Proceed/Defer`; defer surfaces structured `reload.deferred` event to status writer with `ext`+`reason`; fail-open on gate error; no automatic retry | host-dispatch | R5 | T-021, T-028, T-030 | M |
| T-036 | Add session-scoped `closed_by_user: BTreeMap<String, ParamsHash>` storage in `crates/supervisor/src/`; write trigger on zellij pane-close delta where closed pane lacked `ARK_HANDLE` AND maps to known scene handle name; stack-children filtered; emit `ark.handle.invalidated{cause:user_closed}` AFTER map-write; read API `lookup(name) -> Option<ParamsHash>` | host-dispatch | R9 | T-012, T-015, T-035 | L |

## Tier 7 — Stub Harness + Tests

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-037 | Create `crates/ark-ext-test-support` crate (feature-gated / dev-dep only) with `StubExtension` + builder: `.with_method(name, handler)`, `.advertise_capabilities(iter)`, `.with_manifest(m)`, `.with_protocol_version(v)`, `.method_advertised_but_unimplemented(name)`; verified non-reachable from production deps | tests | R1 | T-019, T-020, T-021, T-024 | L |
| T-038 | Add NDJSON subprocess `[[bin]] ark-stub-ext` target in the stub crate: accepts `--config` / `ARK_STUB_CONFIG`; runs against existing `crates/ark-ext-proto/src/transport/ndjson.rs` server; parity round-trip test (`stub_subprocess_matches_in_proc`); `supervisor_spawns_stub_and_dispatches` exercises real ext-launch path | tests | R2 | T-037, T-030 | M |
| T-039 | Version-mismatch matrix tests under `crates/ark-ext-proto/tests/`: 5 cells (1.1↔1.1 OK no-warn; 1.1↔1.0 OK no-warn; 1.0↔1.1 OK WARN for unknown caps; 2.0↔1.1 UnsupportedVersion + no subsequent RPC; symmetric 1.1↔2.0); driven by stub's `.with_protocol_version` | tests | R3 | T-037 | M |
| T-040 | Capability-gate matrix tests: (a) advertised+implemented → call reaches stub; (b) not-advertised → zero calls + zero wire bytes; (c) advertised-but-unimplemented → ONE WARN + session survives; (d) removed-in-MAJOR placeholder (xfail or synthetic) | tests | R4 | T-028, T-037 | M |
| T-041 | `trybuild` view-type compile-error goldens under `crates/scene/tests/ui/`: 4 compile-fail (undeclared_view_type, view_type_mismatch_on_handle_attr, stack_child_under_non_stack_parent, handle_typed_attr_takes_non_handle) + 2 compile-pass (valid_pane_and_stack_decls, cross_ext_view_reference); `TRYBUILD=overwrite` documented; `.kdl:line:col` pointer in every stderr golden | tests | R5 | T-034 | M |
| T-042 | Integration tests: (a) `manifest_intent_appears_in_registry`; (b) `scene_op_dispatches_to_manifest_intent` observes `intent/dispatch { name, args }` on stub; (c) `intent_register_rpc_method_is_gone` (grep / compile-fail); (d) `undeclared_intent_scene_op_rejected_at_compile` via trybuild; (e) suppression + invalidation suite (`user_close_records_suppression_and_emits_invalidated`, `reconcile_same_params_skips_spawn_after_user_close`, `reconcile_new_params_respawns_after_user_close`, `pane_op_after_invalidation_returns_handle_gone`, `supervisor_restart_clears_suppression`, `stack_child_user_close_does_not_suppress_respawn`) | tests | R6, R7 | T-036, T-037, T-041 | L |

## Tier 8 — Proto Bump + CI Gate

| Task | Title | Kit | Requirement | blockedBy | Effort |
| --- | --- | --- | --- | --- | --- |
| T-043 | Bump `CURRENT_PROTOCOL_VERSION` at `crates/ark-ext-proto/src/lib.rs:268` from `ProtocolVersion::new(1, 0)` → `ProtocolVersion::new(1, 1)`; zero residual `::new(1, 0)` hits; existing `is_compatible` test still green; bump lands AFTER T-017..T-042 | ext-surface | R8 | T-042 | S |
| T-044 | CI / workspace green gate: `cargo test --workspace --tests` green (no features, no env); stub crate in root `Cargo.toml` members; `TRYBUILD=overwrite` yields no diff on committed goldens; no network / zellij / external-filesystem deps; `cargo check -p ark-view` + `cargo test -p ark-view` + `cargo test -p ark-ext-proto` + `cargo test -p ark-ext-derive` + `cargo test -p ark-ext-metadata-types` + `cargo test -p ark-scene` + `cargo test -p ark-supervisor` + `cargo test -p ark-cli` + `cargo test -p ark-config` all green | tests | R8 | T-043 | M |

## Decisions incorporated 2026-04-18

The following open items from the phase-2 decomposition interview were
resolved in `phase-2-design-decisions.md` (Resolutions section) and are
folded into this build site — no lingering flags remain:

- **§R-5 `SessionOutcome` payload** — Resolved: `CoreEvent::SessionEnded`
  gains `exit: ExitReason { Normal, Error(String), Cancelled }`; Phase 2
  `on_session_end(&SessionSpec, &ExitReason)`. Folded into **T-045** (new,
  Tier 0, extends `ark_types::event`) and **T-020** (now blockedBy T-045
  and references `ExitReason` in its signature). No separate
  `SessionOutcome` crate/type.
- **§R-6 manifest-set hash algorithm** — Resolved: `blake3`. Folded into
  **T-034** (pins `blake3` in description + wires workspace dep if
  absent). **T-012** (`ParamsHash` / user-close suppression hash) already
  named `blake3`; both hashes share the algorithm.

All other Resolutions items (§R-7 stack-child naming, §R-8 union-syntax
deferral, §R-9 stack sizing, §R-10 view_table privacy, §R-11 factory.rs
delete, §R-12 `run_preflight` delete, §R-13 cc-hook install, §R-14 CC
project-dir non-issue) apply to other build sites (scene-2026-04-18,
cleanup Phase 4/5, claude-code extension) and do not modify Phase 2
tasks.

## Summary

| Tier | Tasks | Effort |
| --- | ---: | --- |
| 0 | 4 | 2M + 2S |
| 1 | 4 | 4S |
| 2 | 6 | 4M + 2S |
| 3 | 4 | 2M + 2S |
| 4 | 6 | 1L + 4M + 1S |
| 5 | 4 | 2L + 2M |
| 6 | 9 | 4L + 5M |
| 7 | 6 | 2L + 4M |
| 8 | 2 | 1M + 1S |
| **Total** | **45** | **9L + 24M + 12S** |

(Note: summary row counts include a small over-count adjustment; raw task count is **45** across 9 tiers. T-045 is numbered out-of-sequence but lives at Tier 0 — see "Decisions incorporated 2026-04-18" below.)

## Coverage Matrix

Every acceptance-criterion cluster from every Phase-2 R maps to at least
one task. Sibling sub-kit cross-refs (e.g. ark-view R8's "storage lives
in supervisor — see host-dispatch") are covered exactly once on the
owning side.

### ark-view Kit (R1–R11, 11 R's)

| Kit | Req | Task(s) |
| --- | --- | --- |
| ark-view | R1 crate skeleton + dep budget + re-exports | T-001, T-017 |
| ark-view | R2 HandleKind narrowed | T-004 |
| ark-view | R3 View / CommandView / ZellijView markers | T-006 |
| ark-view | R4 Pane / Stack / TabHandle / PaneLike + marker-gated | T-008, T-009, T-010, T-011 |
| ark-view | R5 opaque HandleId on the wire | T-005, T-008 |
| ark-view | R6 six ext→host RPC methods | T-018, T-019 |
| ark-view | R7 invalidation + HandleGone | T-007, T-014, T-015 |
| ark-view | R8 user-close suppression policy + ParamsHash | T-012, T-013 |
| ark-view | R9 stack children excluded | T-013 |
| ark-view | R10 name-indexed handle lookup | T-016 |
| ark-view | R11 derive-addressable type surface | T-017 |

### ext-surface Kit (R1–R8, 8 R's)

| Kit | Req | Task(s) |
| --- | --- | --- |
| ext-surface | R1 lifecycle hooks + `ExitReason` payload | T-020, T-045 |
| ext-surface | R2 feature-group hooks | T-021 |
| ext-surface | R3 permission_dispatcher + intent_register removed | T-002 |
| ext-surface | R4 manifest `views`/`config_sections`/`reload_gates` + ViewDecl.kind | T-003, T-023 |
| ext-surface | R5 manifest-driven intent shim contract | T-022 |
| ext-surface | R6 capability-flag taxonomy (8 flags) | T-024 |
| ext-surface | R7 `#[derive(View)]` + markers + auto-advertise | T-025, T-026, T-027 |
| ext-surface | R8 proto 1.0 → 1.1 bump | T-043 |

### host-dispatch Kit (R1–R9, 9 R's)

| Kit | Req | Task(s) |
| --- | --- | --- |
| host-dispatch | R1 `ark list` column fan-in | T-031 |
| host-dispatch | R2 `ark doctor` check runner | T-032 |
| host-dispatch | R3 figment config-section layering | T-033 |
| host-dispatch | R4 scene compile-time view-type validator | T-034 |
| host-dispatch | R5 reload-gate dispatcher | T-035 |
| host-dispatch | R6 capability-aware RPC dispatch | T-028 |
| host-dispatch | R7 host-declared capabilities on handshake | T-029 |
| host-dispatch | R8 extension load sequence | T-030 |
| host-dispatch | R9 closed_by_user map storage | T-036 |

### tests Kit (R1–R8, 8 R's)

| Kit | Req | Task(s) |
| --- | --- | --- |
| tests | R1 StubExtension in-proc harness | T-037 |
| tests | R2 NDJSON subprocess variant | T-038 |
| tests | R3 version-mismatch matrix | T-039 |
| tests | R4 capability-gate matrix | T-040 |
| tests | R5 view-type compile-error goldens | T-041 |
| tests | R6 intent-registration integration | T-042 |
| tests | R7 suppression + invalidation integration | T-042 |
| tests | R8 CI integration / workspace green | T-044 |

**Coverage: 100% (36/36 R's mapped).**

## Completion Gate

Phase 2 is DONE when every item below holds:

- [ ] `cargo check --workspace --tests` exits 0.
- [ ] `cargo test --workspace --tests` exits 0 with no features, no env
      vars, no network, no zellij dependency.
- [ ] `crates/ark-view/` exists, is a workspace member, passes its own
      `cargo test -p ark-view`, and its dep budget matches ark-view R1.
- [ ] All six `pane/*` + `stack/*` RPC methods exist on `ArkExtension`
      with default `method_not_found` bodies and `Facet+Debug+Clone`
      request/response structs.
- [ ] All six R1+R2 ext-surface lifecycle + feature-group hook methods
      exist with default `method_not_found` bodies.
- [ ] `intent_register` + `permission_dispatcher` + `register_intents`
      are gone from `ArkExtension`. `rg` shows zero hits.
- [ ] `ExtensionMetadata` carries `views` + `config_sections` +
      `reload_gates` Vec fields; `ViewDecl` carries the `kind` field.
- [ ] Exactly 8 capability flags (`view.pane.v1`, `view.stack.v1`,
      `ext.lifecycle.v1`, `ext.scene_compile_hook.v1`,
      `ext.control_verbs.v1`, `ext.doctor.v1`, `ext.list_columns.v1`,
      `ext.reload_gate.v1`) are declared; set-equality test green.
- [ ] `CURRENT_PROTOCOL_VERSION == ProtocolVersion::new(1, 1)`; zero
      residual `ProtocolVersion::new(1, 0)` hits.
- [ ] Capability-aware RPC dispatcher routes every Phase-2 method call
      through one check point; not-advertised → zero wire bytes;
      advertised-but-`method_not_found` → one WARN + session survives.
- [ ] Extension load sequence completes the 7-step pipeline;
      failed-extension isolation verified by stub test.
- [ ] `closed_by_user` map storage is session-scoped in-memory; survives
      reconcile but not supervisor restart; stack-children never
      entered.
- [ ] `crates/ark-ext-test-support` stub harness crate passes in-proc +
      NDJSON subprocess parity round-trip.
- [ ] Version-mismatch matrix green across all 5 cells; capability-gate
      matrix green across cases (a), (b), (c) (case (d) is xfail until
      first real MAJOR removal).
- [ ] `trybuild` view-type goldens: 4 compile-fail + 2 compile-pass;
      `TRYBUILD=overwrite` yields no diff against committed `.stderr`.
- [ ] Manifest-driven intent registration integration test observes a
      full `scene KDL op → loader shim → intent/dispatch RPC → stub`
      round-trip.
- [ ] Suppression + invalidation suite: all 6 named tests green
      (records-and-emits, same-params-skips, new-params-respawns,
      `HandleGone` lazy error, restart-clears, stack-child-exempt).
- [ ] `cargo test -p ark-view` + `-p ark-ext-proto` +
      `-p ark-ext-derive` + `-p ark-ext-metadata-types` + `-p ark-scene`
      + `-p ark-supervisor` + `-p ark-cli` + `-p ark-config` +
      `-p ark-ext-test-support` all exit 0.
- [ ] No `TODO(cavekit-soul-phase-2)` hits outside documented deferrals;
      no new `#[ignore]` papering over breakage.
