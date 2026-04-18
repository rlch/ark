---
created: "2026-04-18"
status: decisions
feeds_into: cavekit-soul-phase-2-*.md (decomposition not yet written)
author: interview — main agent + user
---

# Phase 2 design decisions (interview, 2026-04-18)

Resolves the 4 open design points flagged in `handoff-2026-04-18-claude-code-first-pivot.md:285-328` gap analysis before Phase 2 sub-kit decomposition. Each decision locks a contract; decomposition agent treats these as inputs.

---

## #1 — `permission_dispatcher` drops

**Decision:** Removed from Phase 2 scope. ACP was fully deleted in Phase 1 (interview #2). No replacement — scene reactions + ext-emitted events cover the reactive surface.

Action for decomposition: omit from `supervisor-hooks` sub-kit. No ceremony.

---

## #2 — Intent registration: single manifest-driven path

**Problem:** Three paths existed — trait RPC `intent_register`, derive `#[ark::intent]` + `inventory::submit!`, and a proposed `register_intents` lifecycle hook. Needed one story.

**Structural clarification:** The three paths aren't at the same layer. Derive + inventory produces COMPILE-TIME METADATA (ark-ext-metadata manifest); trait RPC is RUNTIME dispatch binding; hook was proposed batch-at-init replacement for trait RPC.

**Decision:** Manifest is the single source of truth for v0.1.

- Compile-time derive + inventory + `ark-ext-metadata` → extension's manifest (embedded as wasm custom section or emitted as `extension.kdl`).
- Ark reads the manifest on extension load. For each declared intent, ark creates an `Arc<dyn Intent>` shim that dispatches to `intent/dispatch` RPC on the extension.
- `IntentRegistry.register(name, shim)` is called from the loader, not from the extension.
- Scene compile-time validation reads the SAME manifest's symbol table — no divergence possible.
- `intent_register` RPC method: DELETED from `ArkExtension` trait for v0.1.
- `register_intents` lifecycle hook: DELETED from Phase 2 scope (redundant).

**Deferred to v0.2:** Dynamic runtime registration (pi-style `pi.registerTool(...)`). When it lands, it reintroduces a single RPC method (`intent/register_dynamic` or similar) explicitly scoped to "mid-session dynamic intent injection" — not the default path.

**Precedent:** LSP convention — static `InitializeResult.capabilities` covers 99% of cases; `client/registerCapability` is rare dynamic case. Our manifest = their static capabilities; deferred dynamic RPC = their registerCapability.

Action for decomposition:
- `ext-registrations` sub-kit: drops `register_intents` hook.
- Manifest-reading at extension load is a Phase-2 R requirement in the new `fan-in` or `view-runtime` sub-kit (unclear which; decomposition resolves).
- ark-ext-derive codegen spec (`derive-macros` sub-kit) explicitly notes intents flow through manifest, not RPC.

---

## #3 — Typed handles: ownership, RPC shape, invalidation

### 3a. Crate home — NEW `crates/ark-view`

New crate `crates/ark-view` owns:

- Trait `View`, marker traits `CommandView` and `ZellijView`
- Parametric types `Pane<V: View>`, `Stack<V: View>`
- Typed `TabHandle` (replaces current mux-side opaque `TabHandle`)
- `HandleKind` enum narrowed to `{ Tab, Pane, Stack }` (retires `Command`, `Plugin` per scene R17)

Consumers:
- `crates/scene` — compile-time KDL validation against view types.
- `crates/ark-ext-proto` — re-exports `ark-view` types for extensions.
- `crates/ark-ext-derive` — codegen targets (see 3b).

Rationale: scene doesn't otherwise depend on RPC proto; mixing view types into ext-proto conflates layers. Small isolated crate mirrors how tower-lsp separates `lsp-types` from `tower-lsp` server glue.

### 3b. RPC shape — method-per-op

Each handle operation gets its own typed RPC method on `ArkExtension` (ext calls host direction):
- `pane/emit { handle, event }`
- `pane/replace_view { handle, view_body }`
- `pane/close { handle }`
- `stack/spawn_pane { stack, attrs } -> Pane<V>` (returns new handle)
- `stack/close_child { stack, handle }`
- `stack/clear { stack }`

Handles on wire are opaque strings (`handle_id`). Type parameter `V` is compile-time-only; does not cross the wire.

Rationale: self-documenting proto surface; per-method capability gating (see #4); matches existing `ui_pane_request` / `event_emit` pattern. Generic `ui_handle_request { action, ... }` alternative rejected — weaker typing, harder version gating.

### 3c. Invalidation taxonomy

Every scene-declared handle has exactly one of three termination causes, broadcast as `ark.handle.invalidated { handle, cause }` ExtEvent **and** surfaced lazily as `HandleGone` error on subsequent ops:

| Cause | Trigger | Respawn policy |
|---|---|---|
| `user_closed` | User closes pane via zellij keybind; ark detects missing ARK_HANDLE from pane-list delta | **Stays closed.** Entered into per-session suppression set. Respawns only if view's scene params hash changes on next reload (see 3d). |
| `scene_reload_dropped` | New compiled scene has no matching handle | Gone. If author re-adds later, new handle ID. |
| `session_ended` | Supervisor shutdown | All handles cascade; no per-handle events emitted |

Belt-and-suspenders: lazy `HandleGone` on next op catches races where ext hasn't yet processed the invalidated event.

Handles issued to extensions are ONLY for scene-declared panes + stack-children. User-opened panes (via zellij keybinds) have no `ARK_HANDLE`, are invisible to extensions, and never produce a `Pane<V>` instance — invalidation concern does not apply.

### 3d. User-close suppression + params-hash override

Per-session state in supervisor: `closed_by_user: Map<SceneHandleName, ParamsHash>`.

On user-close of a scene pane:
1. Compute hash of the view's current scene params.
2. Store `(handle_name, params_hash)` in `closed_by_user`.
3. Broadcast `ark.handle.invalidated { cause: user_closed }`.

On next reconcile (after scene edit or otherwise):
1. For each declared pane, check `closed_by_user`:
   - Not present → spawn as normal.
   - Present, stored hash == current params hash → **skip spawn** (honor user's close).
   - Present, stored hash != current params hash → evict from `closed_by_user` → spawn (author changed the view; it's logically a new rendering).

Stack-children are NOT subject to this suppression. Their closure by user is permanent for that child instance; extensions re-spawn via `spawn_into @stack` if the logical work continues.

Suppression set lives in-memory in supervisor; its lifetime = session lifetime. No persistence across supervisor restart (supervisor restart in v0.1 = new session).

Action for decomposition:
- `view-runtime` sub-kit: owns crate setup + types + invalidation protocol + suppression logic.
- `supervisor-hooks` sub-kit: owns the session-scoped `closed_by_user` map + reconcile hook for checking it.
- Keep RPC method surface scoped to `view-runtime` sub-kit.

---

## #4 — Back-compat / version gating: two-tier LSP-style

Existing machinery (already in tree — `crates/ark-ext-proto/src/lib.rs:197-268`, `503-539`): `ProtocolVersion(MAJOR, MINOR)`, `InitializeRequest/Response`, `client_capabilities` / `extension_capabilities` bags, default trait impls returning `method_not_found` (lib.rs:1078-1086), conformance tests for version mismatch. LSP-shape. Solid. No new structural work; Phase 2 locks POLICY.

### 4a. Gate mechanism — both capability flags and method_not_found

Two-tier, matching LSP:

1. **Capability flags** — primary gate for FEATURE presence. Extension advertises in `InitializeResponse.extension_capabilities` which Phase-2 feature groups it supports. Ark checks before calling. Examples:
   - `view.pane.v1` — ext can receive `Pane<V>` in intent handlers.
   - `view.stack.v1` — ext can receive `Stack<V>` and receive stack-child lifecycle events.
   - `ext.lifecycle.v1` — ext implements `on_session_start` / `on_session_end`.
   - `ext.doctor.v1` — ext provides `doctor_checks`.
   - `ext.list_columns.v1` — ext contributes `ark list` columns.
   - `ext.reload_gate.v1` — ext registers reload gates.

2. **`method_not_found` safety net** — transport-level drift within a feature group. If an ext advertises `view.pane.v1` but (bug) doesn't implement `pane/emit`, ark's call returns `-32601`; dispatcher logs + skips the op rather than crashing the session.

Capability taxonomy naming convention: `<domain>.<feature>.v<N>` where `v<N>` is feature-group version (not proto version). Bumping the feature version (e.g. `view.pane.v2`) = breaking change WITHIN the feature, handled by adding parallel methods; old methods stay for `v1` consumers.

### 4b. Forward compat — host declares capabilities on handshake

`InitializeRequest.client_capabilities` (ark→ext on handshake) is populated with the feature groups THIS ark version supports. Extensions read the response handshake's client capabilities and downgrade their own behavior if ark is missing a capability they need (e.g. an ext that uses `stack/spawn_pane` checks for `view.stack.v1` in ark's capabilities; if missing, the ext falls back to non-stack rendering or surfaces a degraded-mode warning).

No per-call probing; one handshake, full picture. Matches LSP exactly.

### 4c. MAJOR vs MINOR

- **MAJOR** — removing a method, changing its params, changing a wire-serialized type shape, breaking session-token semantics, or any change that a correct ext of version N would misbehave on when talking to ark of version N+1.
- **MINOR** — adding methods, adding optional fields, adding capabilities, adding new ExtEvent kinds, adding intent arguments with defaults.

Phase 2 is **MINOR** (1.0 → 1.1). All additions (~15 methods, 6+ capability flags) ship together; no drip release.

### 4d. Implication for Phase 2 decomposition

- `supervisor-hooks` sub-kit: ark-side dispatcher respects capability flags; calls feature method only when ext advertises capability. Graceful `method_not_found` handling even with capability advertised.
- `ext-registrations` sub-kit: manifest surface adds declared capability flags (derived from which methods ext implements).
- `derive-macros` sub-kit: `#[derive(Extension)]` auto-emits capability advertisement for each implemented method group.
- `tests` sub-kit: version-mismatch + missing-capability + present-capability-but-method_not_found matrices.

No new proto version constants required. `CURRENT_PROTOCOL_VERSION` at ark-ext-proto/lib.rs:268 bumps from `1.0` to `1.1` at the end of Phase 2.

---

## Open items for decomposition agent

1. **Capability taxonomy finalization.** Names above are illustrative; decomposition sub-kits should finalize the exact set + their methods.
2. **Manifest format evolution.** `ext-registrations` sub-kit must specify whether new Phase-2 data (view types, config schema, reload gates) lives in the existing `extension.kdl` / wasm-custom-section or needs a new manifest revision. Current metadata at `crates/ark-ext-metadata-types/src/lib.rs:97-102` has `ark_range` but no view-type declarations.
3. **Name-indexed handle lookup.** Decision 3d implies reconcile walks scene and cross-references `closed_by_user` by scene handle name. Extensions currently receive handles as opaque IDs. Need a name→handle lookup API on `Pane<V>` / session context so exts can re-attach after user-close-then-params-change.
4. **Proto version vs capability version.** `ProtocolVersion(MAJOR, MINOR)` is protocol-wide. Capability versions (`view.pane.v1`) are per-feature. No conflict but the decomposition should cover the interaction in `tests` sub-kit goldens.
