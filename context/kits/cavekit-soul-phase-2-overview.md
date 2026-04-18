---
created: "2026-04-18"
last_edited: "2026-04-18"
parent: cavekit-soul.md
phase: 2
status: draft
---

# Cavekit: Soul Phase 2 — Extension supervisor hooks (overview)

## Scope

Phase 2 adds the surface extensions use to plug into ark: lifecycle hooks,
manifest-driven registrations (views, intents, config sections, reload
gates), a typed view runtime (`Pane<V>` / `Stack<V>` with invalidation
semantics), and the host-side fan-in that consumes all of it. Protocol
version bumps once: `ark-ext-proto` `CURRENT_PROTOCOL_VERSION` 1.0 → 1.1,
batched, all additions landing together.

Phase 2 is the final surface needed before Phase 4's `extensions/claude-code/`
can be written. Phase 1 delivered the bare-ark path; Phase 2 delivers the
ext-surface; Phase 4 consumes both. Pi (v0.2, `cavekit-pi.md` DEFERRED) is
the second consumer once claude-code validates the contract.

## Sibling sub-kits

Phase 2 decomposes into four sub-kits plus this overview. Each sub-kit is
implementation-agnostic, scoped to one concern, and cross-references siblings
where boundaries meet.

| Sub-kit | Scope | R's |
|---|---|---|
| [`cavekit-soul-phase-2-ark-view.md`](cavekit-soul-phase-2-ark-view.md) | New `crates/ark-view` crate. `View` / `CommandView` / `ZellijView` traits, narrowed `HandleKind`, parametric `Pane<V>` / `Stack<V>` / typed `TabHandle`. Ext→host RPC method surface (`pane/emit`, `stack/spawn_pane`, etc.). Handle invalidation taxonomy + `HandleGone` lazy error. User-close suppression policy + stack-child exception. Name-indexed handle lookup. | 11 |
| [`cavekit-soul-phase-2-ext-surface.md`](cavekit-soul-phase-2-ext-surface.md) | What extensions DECLARE. New lifecycle methods on `ArkExtension` (`on_session_start`, `on_session_end`, `scene_compile_hook`, `control_verbs`, `doctor_checks`, `list_columns`). Manifest surfaces (`ViewDecl`, `ConfigSectionDecl`, `ReloadGateDecl`; intents unchanged, flow through existing derive+inventory). `ark-ext-derive` codegen (`#[derive(View)]`, marker-trait detection, capability-flag auto-emit). Phase 2 capability taxonomy (`view.pane.v1`, `view.stack.v1`, `ext.lifecycle.v1`, etc.). `intent_register` RPC method removed. Proto bump to 1.1. | 8 |
| [`cavekit-soul-phase-2-host-dispatch.md`](cavekit-soul-phase-2-host-dispatch.md) | What ark does with the declarations. `ark list` column fan-in, `ark doctor` check runner, figment config-section layering, scene compile-time view-type validator, reload-gate dispatcher, capability-aware RPC dispatch, host-declared capabilities on handshake, extension load sequence, user-close suppression map storage. | 9 |
| [`cavekit-soul-phase-2-tests.md`](cavekit-soul-phase-2-tests.md) | Shared stub in-proc `ArkExtension` harness (in-proc + NDJSON subprocess variants). Version / capability back-compat matrix. `trybuild` view-type-mismatch goldens. Manifest-driven intent registration tests. User-close suppression + handle-invalidation tests. CI integration. | 8 |

Total: **36 R's** across 4 sub-kits.

## Locked decisions

All cross-cutting design decisions that shaped the decomposition are in
[`context/plans/phase-2-design-decisions.md`](../plans/phase-2-design-decisions.md).
Four points, resolved through user interview on 2026-04-18:

1. **`permission_dispatcher` drops.** ACP gone (interview #2, Phase 1). No
   replacement surface needed.
2. **Intent registration: single manifest-driven path.** Compile-time
   `#[ark_intent]` derive + inventory → manifest (embedded in wasm custom
   section or `extension.kdl`). Ark reads manifest on extension load,
   populates `IntentRegistry` with RPC-dispatching shims. `intent_register`
   RPC and `register_intents` hook both deleted for v0.1. Dynamic
   registration deferred to v0.2 (pi's `pi.registerTool(...)` use case).
3. **Typed handles: new `crates/ark-view` + method-per-op RPC +
   three-cause invalidation + user-close suppression with params-hash
   override.** Full details in the decisions doc and the `ark-view` sub-kit.
4. **Two-tier back-compat: capability flags + `method_not_found` safety
   net.** LSP-shape. Host declares capabilities in
   `InitializeResponse.client_capabilities`. Phase 2 ships as a single
   MINOR bump (1.0 → 1.1). MAJOR is reserved for breaking changes only.

## Dependency order

For implementation (not kit-writing):

```
ark-view (new crate, foundational)
  │
  ├─► ext-surface (depends on ark-view for View trait + types in derive targets)
  │     │
  │     └─► host-dispatch (depends on ext-surface for manifest shape + trait surface)
  │           │
  │           └─► tests (depends on everything; stub harness instantiates all of it)
```

Kit-writing itself was done in parallel (all four sub-kits written simultaneously;
cross-references via decisions doc, not via inter-agent coordination).

Phase 2 implementation can partially parallelize along the same edges:
`ark-view` crate and manifest-surface scaffolding can land first, then
derive + host-dispatch in parallel, then tests fan-in.

## Open items flagged by sub-kits

Items sub-kits surfaced that the decisions doc did not settle — to be
resolved during implementation planning (`/ck:map-from-kits`) or by
follow-up interview:

1. **`SessionOutcome` home crate.** Phase 1 R5 deletes `Outcome` from
   core; Phase 4 would re-home. `on_session_end` needs a payload type.
   *Resolution path:* pin in Phase 4 kit or early in Phase 2 `ext-surface`
   impl when the trait method signature lands.
2. **`Contributions` shape for `scene_compile_hook`.** What exactly can
   an extension contribute at compile time — new views? new intents? new
   reactions? Pin in `host-dispatch` impl.
3. **Derive auto-capability-flag detection.** `#[derive(Extension)]` can
   inspect which methods the user overrode in the same impl block, but
   not in separate `impl` blocks. Hard-fail or warn?
4. **Capability-to-method mapping table ownership.** Static table lives
   where — dispatcher (`host-dispatch`) or taxonomy (`ext-surface`)?
5. **Stub call-count assertion surface.** Whether the shared stub
   exposes assertions — owned by `tests` sub-kit; resolve in impl.
6. **`reload.deferred` event wire shape.** R5 in `host-dispatch` needs
   the structured event spec pinned before impl.
7. **Stub crate name.** Proposed `crates/ark-ext-test-support`; confirm
   during impl.
8. **`ParamsHash` algorithm.** Decision #3d locks the KEY; hash fn
   input + algorithm is impl-detail pinned during `ark-view` impl.
9. **Capability-gate WARN log format.** Decision #4a mandates the line
   exists; exact format pinned during `host-dispatch` impl.
10. **Handle name→handle lookup API surface.** Decisions doc's own open
    item #3. Used by R10 (`ark-view`) + R7c (`tests`); final shape pinned
    in `ark-view` impl.
11. **Manifest format evolution for view-type declarations.** Decisions
    doc's own open item #2. Current metadata at
    `crates/ark-ext-metadata-types/src/lib.rs:97-102` has no
    view-type declarations; Phase 2 adds them.

None of these block kit-to-build-site mapping; they surface during
implementation.
