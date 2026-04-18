---
created: "2026-04-18T00:00:00Z"
last_edited: "2026-04-18T00:00:00Z"
parent: cavekit-soul.md
phase: 2
status: draft
---

# Cavekit: Soul Phase 2 — Ext Surface (what extensions declare)

## Scope

The **surface extensions use to declare themselves to ark**. Covers three
layers of that surface, unified per decision #2 in
`context/plans/phase-2-design-decisions.md`:

1. The runtime RPC trait (`ArkExtension` in `crates/ark-ext-proto`) —
   new lifecycle + feature-group hook methods extensions may override.
2. The compile-time manifest (`ExtensionMetadata` in
   `crates/ark-ext-metadata-types`) — new declaration vectors for views,
   config sections, and reload gates that extend the existing intents /
   events / config surface.
3. The derive codegen (`crates/ark-ext-derive`) — new `#[derive(View)]`
   + marker traits + auto-advertisement that stamp manifest entries
   and capability flags from annotated Rust items.

All three paths that the OLD design had (trait RPC `intent_register`,
derive+inventory, proposed `register_intents` lifecycle hook) collapse
to the single manifest-driven path per decision #2. `permission_dispatcher`
is dropped per decision #1 (ACP gone).

Sibling Phase 2 kits own the concerns this kit cross-references:

- `cavekit-soul-phase-2-view-runtime.md` (sub-kit 1) — owns the `View`
  / `CommandView` / `ZellijView` trait shapes, `Pane<V>` / `Stack<V>`
  parametric types, handle invalidation protocol, and the `pane/*` +
  `stack/*` RPC method bodies on `ArkExtension`.
- `cavekit-soul-phase-2-host-dispatch.md` (sub-kit 3) — owns the
  ark-side behaviour: loading manifests, assembling list columns,
  running doctor checks, invoking scene compile hooks, reconcile +
  `closed_by_user` bookkeeping.
- `cavekit-soul-phase-2-tests.md` (sub-kit 4) — owns the compat
  matrices (version mismatch, capability-flag drift, `method_not_found`
  safety-net).

All decisions referenced here are locked in
`context/plans/phase-2-design-decisions.md`. Cross-refs name the exact
decision id (e.g. "per decision #2") — no re-litigation in this kit.

## Requirements

### R1: Lifecycle hook methods on `ArkExtension`

**Description:** Extend the `ArkExtension` trait at
`crates/ark-ext-proto/src/lib.rs:1090` with two session-lifecycle hook
methods that ark calls at session boundaries so extensions can attach
per-session state without polling the event stream. Each method follows
the existing convention at lines 1078-1086: `async fn` with a default
body returning `ExtensionError::method_not_found("<method>")`.

New methods:

```rust
async fn on_session_start(
    &self,
    _req: OnSessionStartRequest,
) -> ExtResult<OnSessionStartResponse> {
    Err(ExtensionError::method_not_found("on_session_start"))
}

async fn on_session_end(
    &self,
    _req: OnSessionEndRequest,
) -> ExtResult<OnSessionEndResponse> {
    Err(ExtensionError::method_not_found("on_session_end"))
}
```

Request bodies carry the Phase 1 `SessionSpec` (per
`cavekit-soul-phase-1-types.md` R1); `OnSessionEndRequest` additionally
carries a `SessionOutcome` (Phase 1 defines the enum, or a placeholder
stays until a future move — the exact type lives in whichever crate
owns it, this kit only pins the method shape). Response bodies may be
empty; keeping a typed struct rather than `()` preserves the MINOR-bump
room per decision #4c rule "adding optional fields is MINOR".

**Acceptance Criteria:**

- [ ] `rg -n "async fn on_session_start" crates/ark-ext-proto/src/lib.rs`
      prints exactly one hit inside the `ArkExtension` trait body.
- [ ] `rg -n "async fn on_session_end" crates/ark-ext-proto/src/lib.rs`
      prints exactly one hit inside the `ArkExtension` trait body.
- [ ] The default body of each method returns
      `Err(ExtensionError::method_not_found("<method>"))`, matching the
      existing pattern at lines 1078-1086.
- [ ] `OnSessionStartRequest` carries the Phase 1 `SessionSpec` by value
      or reference (exact shape pinned at implementation time — this
      kit allows either).
- [ ] `OnSessionEndRequest` carries a `SessionSpec` and a
      `SessionOutcome` (or equivalent terminal-state value).
- [ ] An extension that does NOT override these methods compiles and
      loads; ark's dispatcher treats `method_not_found` as "ext opted
      out" per decision #4a "method_not_found safety net".

**Dependencies:** Phase 1 R1 (SessionSpec).

### R2: Feature-group hook methods on `ArkExtension`

**Description:** Add four feature-group hook methods to the
`ArkExtension` trait, each following the same
default-returns-`method_not_found` convention as R1.

```rust
async fn scene_compile_hook(
    &self,
    _req: SceneCompileHookRequest,   // carries a PartialScene snapshot
) -> ExtResult<SceneCompileHookResponse> {  // carries Contributions
    Err(ExtensionError::method_not_found("scene_compile_hook"))
}

async fn control_verbs(
    &self,
    _req: ControlVerbsRequest,
) -> ExtResult<ControlVerbsResponse> {  // carries Vec<ControlVerb>
    Err(ExtensionError::method_not_found("control_verbs"))
}

async fn doctor_checks(
    &self,
    _req: DoctorChecksRequest,
) -> ExtResult<DoctorChecksResponse> {  // carries Vec<DoctorCheck>
    Err(ExtensionError::method_not_found("doctor_checks"))
}

async fn list_columns(
    &self,
    _req: ListColumnsRequest,
) -> ExtResult<ListColumnsResponse> {  // carries Vec<ColumnSpec>
    Err(ExtensionError::method_not_found("list_columns"))
}
```

Semantics (each is ark→ext direction; ark calls when assembling the
corresponding surface):

- `scene_compile_hook` — ark calls during scene compile, passes the
  partial scene AST; ext returns `Contributions` (extra layout nodes,
  intent bindings, event subscriptions it wants merged).
- `control_verbs` — ark calls to populate the `ark control` dispatch
  table; ext returns verb specs (name + arg schema).
- `doctor_checks` — ark calls during `ark doctor`; ext returns the set
  of named checks (or check results, depending on host-dispatch
  contract — sub-kit 3 pins the exact shape).
- `list_columns` — ark calls during `ark list`; ext returns the columns
  it contributes (name + value-resolver reference).

Request / response type bodies (e.g. what `PartialScene`, `ControlVerb`,
`DoctorCheck`, `ColumnSpec` look like) are **placeholders in this
kit**; their concrete shape is decided in `host-dispatch` sub-kit. This
kit pins method names, direction, and the `method_not_found` default.

**Acceptance Criteria:**

- [ ] `rg -n "async fn (scene_compile_hook|control_verbs|doctor_checks|list_columns)" crates/ark-ext-proto/src/lib.rs`
      prints four hits, all inside the `ArkExtension` trait body.
- [ ] Each method's default body returns
      `Err(ExtensionError::method_not_found("<method_name>"))`.
- [ ] Each method has a dedicated request + response struct (no tuple
      args / unit returns); response structs may be empty but exist.
- [ ] Each response struct carries the domain payload named in this R
      (`Contributions`, `Vec<ControlVerb>`, `Vec<DoctorCheck>`,
      `Vec<ColumnSpec>` or equivalent) as a public field.
- [ ] An ext that implements none of these compiles + loads; ark's
      host dispatcher skips the corresponding surface per decision #4a
      "method_not_found safety net".

**Dependencies:** R1 (shares the feature-group-hook convention).

### R3: `permission_dispatcher` removed; `intent_register` removed

**Description:** Per decision #1, `permission_dispatcher` is NOT added
to `ArkExtension` in Phase 2 — ACP was fully deleted in Phase 1 and no
replacement is needed. Per decision #2, `intent_register` as an RPC
method on `ArkExtension` is DELETED for v0.1 because the manifest is
the single source of truth for intents. Similarly, the proposed
`register_intents` lifecycle hook is NOT added.

**Acceptance Criteria:**

- [ ] `rg -n "fn permission_dispatcher" crates/ark-ext-proto/src/lib.rs`
      prints zero hits inside the `ArkExtension` trait body.
- [ ] `rg -n "fn intent_register" crates/ark-ext-proto/src/lib.rs`
      prints zero hits inside the `ArkExtension` trait body.
- [ ] `rg -n "fn register_intents" crates/ark-ext-proto/src/lib.rs`
      prints zero hits inside the `ArkExtension` trait body.
- [ ] If `intent_register` already exists on the trait at the start of
      Phase 2, it is removed (or not added if never introduced); the
      removal is documented in the proto changelog as a breaking change
      gated by the Phase-2 MINOR bump (decision #4c puts all Phase 2
      work in a single MINOR — if removing an unused scaffold method
      escalates to MAJOR, revisit in `tests` sub-kit).
- [ ] No stub / deprecated alias survives (no `#[deprecated] fn
      intent_register`; decision #2 says deleted, not deprecated).

**Dependencies:** none.

### R4: Manifest surface — `views`, `config_sections`, `reload_gates`

**Description:** Extend `ExtensionMetadata` at
`crates/ark-ext-metadata-types/src/lib.rs:82` with three new declaration
vectors. These declare what ark discovers at extension LOAD time (the
manifest is the source of truth per decision #2). Existing `intents`
and `events` fields are unchanged.

Required new fields:

```rust
pub struct ExtensionMetadata {
    // ... existing fields (name, version, ark_range, zellij_range,
    // requires, intents, events, config, views, capabilities) ...

    /// Named views the extension contributes. Replaces/augments the
    /// existing `views: Vec<ViewDecl>` with the Phase-2 view-kind
    /// info — see R7 for handle-kind codegen semantics.
    pub views: Vec<ViewDecl>,

    /// Named config sub-sections the extension exposes to scene
    /// `use "<ext>" { config { <section> { … } } }`. Layered on top
    /// of the existing flat `config: ConfigSchema`; each section is
    /// a named, independently-schema'd config bag.
    pub config_sections: Vec<ConfigSectionDecl>,

    /// Named reload gates the extension registers. Ark's scene reload
    /// machinery queries each gate before a reload commits; gate
    /// refusal aborts the reload.
    pub reload_gates: Vec<ReloadGateDecl>,
}
```

`ViewDecl` already exists at
`crates/ark-ext-metadata-types/src/lib.rs:417`; Phase 2 extends it
(per R7) with a `kind` field pinning the handle-kind (`"pane"` or
`"stack"` for v0.1 — matches HandleKind in view-runtime sub-kit).
`ConfigSectionDecl` and `ReloadGateDecl` are new — exact field
schemas pinned in implementation. This kit requires each declaration
type to carry at least a `name` (arg-position KDL string) and an
unambiguous payload (schema string for config-sections, a gate-id /
description for reload-gates).

Per decision #2 open item #2 in decisions doc: all new declarations
live in the EXISTING `extension.kdl` / wasm-custom-section manifest
format (no new manifest revision). Adding fields is MINOR per decision
#4c rule "adding optional fields is MINOR" and R16 rule #3 (unknown
fields ignored by older consumers).

**Acceptance Criteria:**

- [ ] `rg -n "pub struct ExtensionMetadata" crates/ark-ext-metadata-types/src/lib.rs`
      still prints exactly one hit.
- [ ] `ExtensionMetadata` has public fields named `views`,
      `config_sections`, and `reload_gates`; each is a `Vec<T>` with
      `#[facet(kdl::children, default)]` so the manifest renders as
      zero-or-more child nodes.
- [ ] `ConfigSectionDecl` and `ReloadGateDecl` types exist in
      `crates/ark-ext-metadata-types/src/lib.rs`, derive `Facet`, and
      each carries a `name: String` via `#[facet(kdl::argument)]`
      (matching the `IntentDecl` / `EventDecl` naming convention).
- [ ] An extension that declares `views { view "foo" … }`,
      `config_sections { section "bar" … }`, and `reload_gates { gate
      "baz" … }` in `extension.kdl` parses through `facet_kdl::from_str`
      into the corresponding vectors, verified by a
      `cargo test -p ark-ext-metadata-types` roundtrip test.
- [ ] Parsing an `extension.kdl` that omits all three new fields
      succeeds (the `default` attribute populates empty `Vec`s),
      verified by a test using a manifest emitted by an older ext.
- [ ] `cargo test --workspace` passes.

**Dependencies:** none (sibling to R1-R3).

### R5: Intent registration flows through manifest — LOAD-time shim codegen

**Description:** Intent registration flows through the manifest
(decision #2), not through an RPC method. This R pins the **wire
contract** the manifest must satisfy for ark's loader to do its job —
the loader-side behaviour lives in `host-dispatch` sub-kit.

Contract the manifest must provide so ark can, at extension LOAD time:

1. Enumerate `ExtensionMetadata.intents: Vec<IntentDecl>` (already
   present at `crates/ark-ext-metadata-types/src/lib.rs:118`).
2. For each `IntentDecl`, read a **fully-qualified intent name**
   (`<ext-name>.<intent>`) and a **JSON-Schema for args validation**.
   `IntentDecl` already carries `name` + `args_schema` — contract
   preserved.
3. Construct an `Arc<dyn Intent>` RPC-dispatching shim (the
   host-dispatch sub-kit owns this constructor) that, when the shim
   is invoked, sends `intent/dispatch { name, args }` over the
   ArkExtension RPC transport to the owning extension.
4. Register the shim in `IntentRegistry.register(name, shim)` —
   ownership reversal per decision #2 ("`IntentRegistry.register` is
   called from the loader, not from the extension").

The shim-construction step relies on an existing `intent/dispatch`
RPC method on `ArkExtension` (already present as of Phase 1). This kit
pins that the method remains available in 1.1 (it is called by the
shims every dispatch). Removing or renaming it would be MAJOR per
decision #4c; this kit forbids that change in Phase 2.

**Acceptance Criteria:**

- [ ] `ExtensionMetadata.intents: Vec<IntentDecl>` exists and each
      `IntentDecl` carries `name: String` (KDL argument) and
      `args_schema: StringNode` (KDL child) — verified by
      `rg -n "pub struct IntentDecl" crates/ark-ext-metadata-types/src/lib.rs`
      showing one hit with both fields.
- [ ] The `ArkExtension` trait retains an `intent_dispatch` (or
      equivalent runtime dispatch) method after Phase 2 lands — removing
      it would break the shim-dispatch contract. Verified by
      `rg -n "fn intent_dispatch" crates/ark-ext-proto/src/lib.rs`
      still printing at least one hit.
- [ ] No extension in tree calls `IntentRegistry.register` directly
      from its own init or lifecycle code — the loader owns all
      registrations. Verified by
      `rg -n "IntentRegistry::register" crates/` showing only
      loader-side call sites (crate `ark-scene` or successor
      host-dispatch crate), not extension crates.
- [ ] Compile-time symbol table used by `ark-scene` for intent-name
      validation reads from the SAME `extension.kdl` manifest as the
      loader — no parallel source of truth. (Already true post-Phase-1;
      this R forbids regressions.)

**Dependencies:** R4 (manifest already holds intents; this R scopes
the contract).

### R6: Capability-flag taxonomy (v1 slate)

**Description:** Pin the v1 capability-flag taxonomy that extensions
advertise in `InitializeResponse.extension_capabilities` (existing
field at `crates/ark-ext-proto/src/lib.rs:526`) and that ark advertises
in `InitializeRequest.client_capabilities` (line 509). Per decision
#4a, these are the primary gate for feature presence; `method_not_found`
is the transport-level safety net.

**Phase 2 introduces exactly these capability flags and nothing else**
(resolving open item #1 in the decisions doc for v0.1):

| Flag                     | Covers                                                                   |
|--------------------------|--------------------------------------------------------------------------|
| `view.pane.v1`           | Ext can receive `Pane<V>` handles (view-runtime sub-kit).                |
| `view.stack.v1`          | Ext can receive `Stack<V>` and stack-child lifecycle (view-runtime).     |
| `ext.lifecycle.v1`       | Ext implements `on_session_start` + `on_session_end` (this kit R1).      |
| `ext.scene_compile_hook.v1` | Ext implements `scene_compile_hook` (this kit R2).                    |
| `ext.control_verbs.v1`   | Ext implements `control_verbs` (this kit R2).                            |
| `ext.doctor.v1`          | Ext implements `doctor_checks` (this kit R2).                            |
| `ext.list_columns.v1`    | Ext implements `list_columns` (this kit R2).                             |
| `ext.reload_gate.v1`     | Ext declares `reload_gates` in its manifest (this kit R4).               |

Naming convention per decision #4a: `<domain>.<feature>.v<N>`. Bumping
`v<N>` (e.g. `view.pane.v2`) is a breaking change WITHIN the feature
group — the v1 method stays for v1 consumers, the v2 method lands in
parallel.

The flags travel on the wire as individual entries in the object-of-
objects `Capabilities` bag (existing shape at
`crates/ark-ext-proto/src/lib.rs:282-339`). The dotted segment before
`.v<N>` is the capability identifier fed to `Capabilities::allows` and
mapped to RPC method names by host-dispatch sub-kit.

**Acceptance Criteria:**

- [ ] A documentation constant or module-level doc in
      `crates/ark-ext-proto/src/lib.rs` enumerates exactly these eight
      v1 capability flag names; no extras, no omissions. Verified by a
      `cargo test -p ark-ext-proto` test that collects the declared
      flags and asserts set equality with the above list.
- [ ] Adding a flag outside this list in Phase 2 is treated as a kit
      violation — verified by the same test failing if an extra flag
      appears.
- [ ] Per-flag-to-method mapping is exhaustive for every Phase 2
      method added by R1 and R2 (`on_session_start`, `on_session_end`
      → `ext.lifecycle.v1`; `scene_compile_hook` →
      `ext.scene_compile_hook.v1`; etc.). Verified by a structural
      test in sub-kit 3 (cross-reference).
- [ ] `Capabilities::allows("view.pane.v1")` returns `true` when the
      ext's `InitializeResponse.extension_capabilities` JSON contains
      `{ "view": { "pane": { "v1": true } } }` (or flat `"view.pane.v1"`
      — whichever the existing wire layer supports). No new wire shape
      introduced; only new identifiers.

**Dependencies:** R1, R2, R4 (flags advertise methods + manifest
surfaces added by those R's).

### R7: `#[derive(View)]` + marker-trait codegen in `ark-ext-derive`

**Description:** Add a `#[derive(View)]` proc-macro to
`crates/ark-ext-derive/src/lib.rs`, alongside an `#[ark_view(...)]`
attribute family that mirrors the shape of `#[ark_intent(...)]` at
lines 252-367. The derive inspects the annotated struct for marker-
trait implementations (`CommandView` / `ZellijView` from view-runtime
sub-kit) and stamps:

1. One `inventory::submit!` call emitting an updated `ViewRegistration`
   (already present at
   `crates/ark-ext-metadata-types/src/lib.rs:523`) plus a new field
   `kind: HandleKind` (or a string equivalent since `HandleKind` lives
   in the new `ark-view` crate, cross-reference
   `cavekit-soul-phase-2-view-runtime.md` — this kit doesn't pin the
   import path, only the codegen behaviour).
2. A `ViewDecl` (compile-time manifest entry, included in the
   auto-generated `extension.kdl`) carrying the same `kind` field.
3. Capability-flag auto-advertisement: if any struct in the crate
   implements `Pane<V>`-taking handler methods, the derive crate
   emits a hidden inventory record flagging `view.pane.v1`; same for
   `view.stack.v1`; same for the `ext.*` flags in R6 based on which
   `ArkExtension` methods the extension actually overrides.

`#[ark_view(name = "custom")]` overrides the auto-derived name exactly
as `#[ark_intent(name = "custom")]` does. `#[derive(CommandView)]` /
`#[derive(ZellijView)]` are marker-only derives (no body codegen
beyond implementing the empty marker trait from view-runtime sub-kit)
— their presence influences the `kind` field emitted by
`#[derive(View)]`.

Automatic capability advertisement logic runs at `#[derive(Extension)]`
expansion (the ext's top-level derive): it scans the crate's inventory
submissions at MACRO expansion time and stamps the union of implied
flags into the `ExtensionMetadata.capabilities` entries. If the derive
cannot see an implementation (e.g. a hand-authored `impl` not adjacent
to a derive), the ext author writes the flag explicitly in
`extension.kdl` — derive is a convenience, not a gate.

**Acceptance Criteria:**

- [ ] `rg -n "pub fn derive_view|#\[proc_macro_derive\(View" crates/ark-ext-derive/src/lib.rs`
      prints at least one hit after Phase 2 lands.
- [ ] `#[derive(View)]` on a struct `MyPanel` emits an
      `inventory::submit! { ViewRegistration { name: "my-panel", …,
      kind: <HandleKind> } }` block — verified by a
      `cargo test -p ark-ext-derive` macro-expansion test.
- [ ] `#[ark_view(name = "custom")]` overrides the default
      snake-to-kebab name derivation, verified by the same test
      asserting `name == "custom"` on the submitted record.
- [ ] `#[derive(CommandView)]` emits an `impl CommandView for
      MyPanel {}` block (marker trait from view-runtime sub-kit) and
      causes any co-derived `#[derive(View)]` to stamp `kind =
      HandleKind::Pane` with `command-view` discriminant (exact naming
      in view-runtime sub-kit; this R requires the derive to route the
      marker through).
- [ ] Auto-advertisement: an extension crate that contains at least
      one `#[derive(View)]` struct produces a manifest whose
      `capabilities` entries include `view.pane.v1` (or `view.stack.v1`
      if the view targets a stack), verified by a
      `cargo test -p ark-ext-derive` end-to-end test expanding a tiny
      fixture crate and reading back the manifest.
- [ ] Auto-advertisement: an extension whose `impl ArkExtension`
      block overrides `on_session_start` causes the emitted manifest
      capabilities to include `ext.lifecycle.v1`. (Derive inspects
      method names present in the `impl` block; if detection is
      impractical at macro time, R7 falls back to requiring the user
      to include the flag — update acceptance accordingly and record
      the deviation as an open item.)

**Dependencies:** R1, R2 (methods being advertised), R4 (ViewDecl
shape), R6 (capability flag names). Cross-refs view-runtime sub-kit
for the marker-trait and `HandleKind` definitions.

### R8: Proto version bump 1.0 → 1.1 at Phase 2 end

**Description:** Per decision #4c, Phase 2 is a **MINOR** proto bump.
`CURRENT_PROTOCOL_VERSION` at
`crates/ark-ext-proto/src/lib.rs:268` moves from `ProtocolVersion::new(1,
0)` to `ProtocolVersion::new(1, 1)` as the final change of Phase 2,
batched (decision #4c: "all additions (~15 methods, 6+ capability
flags) ship together; no drip release").

The bump is performed AFTER all R1-R7 work has landed and the
capability-flag taxonomy of R6 is frozen. Landing it earlier advertises
features that aren't yet implemented, which breaks extensions doing
version-gated feature detection.

**Acceptance Criteria:**

- [ ] `rg -n "CURRENT_PROTOCOL_VERSION" crates/ark-ext-proto/src/lib.rs`
      points to `ProtocolVersion::new(1, 1)` after Phase 2 lands.
- [ ] `rg -n "ProtocolVersion::new\(1, 0\)" crates/ark-ext-proto/src/`
      prints zero hits at the end of Phase 2 (no stale references).
- [ ] No patch-version field is added; the `(MAJOR, MINOR)` shape is
      preserved per decision #4c ("MAJOR vs MINOR; no patch").
- [ ] The version bump commit lands AFTER the commits that add R1-R7
      surfaces — verified by git-log inspection at merge time, not by
      an automated test.
- [ ] `is_compatible` semantics (existing at
      `crates/ark-ext-proto/src/lib.rs:260`) unchanged: `1.1` is
      MAJOR-compatible with any `1.x` client. Verified by the existing
      compat test still passing.

**Dependencies:** R1-R7 (bump is gated on all Phase 2 surface work).

## Out of Scope

- `View` / `CommandView` / `ZellijView` trait *definitions*,
  `Pane<V>` / `Stack<V>` parametric types, `HandleKind` enum, the
  `ark-view` crate setup itself. Covered by
  `cavekit-soul-phase-2-view-runtime.md`.
- RPC methods for handle operations (`pane/emit`, `pane/replace_view`,
  `pane/close`, `stack/spawn_pane`, `stack/close_child`, `stack/clear`).
  Covered by `cavekit-soul-phase-2-view-runtime.md` per decision #3b.
- Handle-invalidation protocol and user-close suppression. Covered by
  `cavekit-soul-phase-2-view-runtime.md` per decision #3c + #3d.
- Host-side dispatcher behaviour: how ark CONSUMES the return values
  from `scene_compile_hook`, how it assembles `ark list` columns from
  multiple exts' `list_columns` returns, how it interleaves
  `doctor_checks` with core doctor, how it reads manifests at load
  time and builds the `Arc<dyn Intent>` shims. Covered by
  `cavekit-soul-phase-2-host-dispatch.md`.
- Per-session `closed_by_user` map, name-indexed handle lookup,
  reconcile loop. Covered by `cavekit-soul-phase-2-host-dispatch.md`
  (and, for the suppression-set lifetime, by the supervisor layer
  referenced from decision #3d).
- Test-harness design, compat matrices, version-mismatch goldens,
  capability-present-but-method-missing tests. Covered by
  `cavekit-soul-phase-2-tests.md`.
- Dynamic runtime intent registration (pi-style `pi.registerTool`).
  Explicitly deferred to v0.2 per decision #2.

## Cross-References

- Parent spec: `cavekit-soul.md` (see Phase 2 section at lines 581-590
  and the Resolved Decisions block).
- Design decisions: `context/plans/phase-2-design-decisions.md`.
  Decisions #1-#4 are authoritative; every R above cites the specific
  decision id it conforms to.
- Spawning doc: `context/plans/handoff-2026-04-18-claude-code-first-pivot.md`
  (gap analysis at lines 285-328).
- Sibling Phase 2 sub-kits (each owns a disjoint slice of Phase 2):
  - `cavekit-soul-phase-2-view-runtime.md` — view traits + handle RPC
    + invalidation + `ark-view` crate setup.
  - `cavekit-soul-phase-2-host-dispatch.md` — ark-side consumption of
    manifests + feature-hook returns + reconcile.
  - `cavekit-soul-phase-2-tests.md` — compat matrices + golden
    fixtures.
- Phase 1 dependencies: `cavekit-soul-phase-1-types.md` (R1
  `SessionSpec`, R6 `CoreEvent` / `ExtEvent` — this kit's R1 method
  payloads reference `SessionSpec`).

## Open Items Not Pinned by Decisions Doc

1. **`SessionOutcome` home crate.** R1 references `SessionOutcome` for
   `on_session_end`'s payload but Phase 1 (see Phase 1 R5) explicitly
   deletes `Outcome` from core `ark-types` and re-homes it in an
   extension at Phase 4. This kit does not re-introduce it; the
   `OnSessionEndRequest` may carry an enum or a `String` discriminant
   pending the outcome of host-dispatch sub-kit's design choice. Flag
   this to the host-dispatch agent for resolution.
2. **`Contributions` shape for `scene_compile_hook`.** R2 leaves the
   `Contributions` type shape unpinned; host-dispatch sub-kit owns the
   concrete struct. This kit pins only that it is a distinct response
   type (not `()`), MINOR-bump-room-preserving.
3. **Auto-capability-flag detection via `#[derive(Extension)]`.** R7's
   auto-advertisement step inspects which `ArkExtension` methods the
   ext crate overrides to emit capability flags. This is straightforward
   at attribute-macro time (inspect the `impl` block), but if the ext
   uses a separate `impl` that the derive cannot see, the flag must be
   written manually. Documenting this caveat is deferred to the
   `derive` implementation task; the kit does not pin whether the
   derive hard-fails or warns when it can't detect a method-override.

## Changelog

(empty — initial draft)
