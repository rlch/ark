---
created: "2026-04-18"
last_edited: "2026-04-18"
parent: cavekit-soul.md
phase: 2
status: draft
---

# Cavekit: Soul Phase 2 — ark-view Runtime

## Scope

Defines the NEW `crates/ark-view` crate, the ext→host RPC method surface
that extensions use to operate on scene-declared handles, the handle
invalidation taxonomy, and the user-close suppression policy that lets
the host honour manual pane closures across reconciliation.

This kit owns the **types and contracts** for typed handles: the
`View` / `CommandView` / `ZellijView` trait trio, the parametric
`Pane<V>` / `Stack<V>` structs, typed `TabHandle`, the narrowed
`HandleKind`, the six `pane/*` + `stack/*` RPC methods, the
`ark.handle.invalidated` event with its three termination causes, and
the per-session `closed_by_user` suppression invariants.

Every decision herein is locked by
`context/plans/phase-2-design-decisions.md` §3 ("Typed handles:
ownership, RPC shape, invalidation"). This kit implements scene R17's
typed-handle contract (`context/kits/cavekit-scene.md:390-424`) at
runtime.

Sibling Phase 2 kits cover out-of-scope concerns:

- `cavekit-soul-phase-2-ext-surface.md` — lifecycle hooks
  (`on_session_start`, `on_session_end`), manifest registration,
  capability flag emission, `#[derive(View)]` / `#[derive(Extension)]`
  codegen spec.
- `cavekit-soul-phase-2-host-dispatch.md` — host-side dispatcher that
  calls these RPC methods, `ark list` / `ark doctor` / scene-validator
  / figment fan-in, supervisor storage of the `closed_by_user` map.
- `cavekit-soul-phase-2-tests.md` — stub-ext harness, version-mismatch
  matrices, invalidation-race goldens.

## Requirements

### R1: `crates/ark-view` workspace member

**Description:** A new crate at `crates/ark-view/` is added to the
workspace with a minimal dep budget. It owns the typed-handle type
surface; no RPC transport, no supervisor logic, no scene IR. Consumers
(per decision #3a): `crates/scene` for compile-time view-type checking,
`crates/ark-ext-proto` for re-export to extension authors, and
`crates/ark-ext-derive` for codegen targets.

**Acceptance Criteria:**

- [ ] `ls crates/ark-view/Cargo.toml` exists.
- [ ] Root `Cargo.toml` `[workspace.members]` lists `crates/ark-view`.
- [ ] `rg -n "^name\s*=\s*\"ark-view\"" crates/ark-view/Cargo.toml` prints exactly one hit.
- [ ] `ark-view` dependency budget is bounded: only `facet`, `serde`,
  `serde_json`, `thiserror`, and workspace internal crate(s) required
  to express `Handle` (an opaque id type). No `async_trait`, no
  `tokio`, no `kdl`, no `rhai`, no `ark-ext-proto`, no `ark-scene`.
  Verified by `cargo tree -p ark-view --depth 1` showing only the
  allowlisted deps.
- [ ] `cargo check -p ark-view` succeeds.
- [ ] `cargo test -p ark-view` succeeds.
- [ ] `crates/ark-ext-proto/Cargo.toml` depends on `ark-view` (so
  extension authors get `use ark_ext_proto::{Pane, Stack, TabHandle};`
  via re-export) — verified by `rg -n "ark-view" crates/ark-ext-proto/Cargo.toml`.
- [ ] `crates/scene/Cargo.toml` depends on `ark-view` — verified by
  `rg -n "ark-view" crates/scene/Cargo.toml`.

**Dependencies:** none (root of Phase 2 ark-view kit).

### R2: `HandleKind` narrowed to `{ Tab, Pane, Stack }`

**Description:** `ark-view` defines `HandleKind` as a three-variant
enum. The old `Command` and `Plugin` variants (which conflated render
mode with handle kind — see scene R17) are retired. View-type
information lives exclusively on the typed wrapper (`Pane<V>`), not on
`HandleKind`.

**Acceptance Criteria:**

- [ ] `rg -n "pub enum HandleKind" crates/ark-view/src/` prints exactly
  one hit.
- [ ] The enum has exactly three variants named `Tab`, `Pane`, `Stack`.
  Verified by a `cargo test -p ark-view` exhaustiveness test that
  pattern-matches every variant.
- [ ] No variants named `Command` or `Plugin` exist. `rg -n
  "HandleKind::(Command|Plugin)" crates/` prints zero hits
  workspace-wide.
- [ ] `HandleKind` serialises through `serde_json` in a stable
  lowercase tag form (`"tab"`, `"pane"`, `"stack"`) — verified by a
  per-variant roundtrip test.
- [ ] `HandleKind` is `Copy + Eq + Hash + Debug`.

**Dependencies:** R1.

### R3: `View`, `CommandView`, `ZellijView` trait trio

**Description:** `ark-view` defines the `View` base marker trait plus
two refining marker traits: `CommandView` (subprocess-rendered) and
`ZellijView` (wasm-plugin-rendered). Extension-defined view traits
(e.g. `trait DiffView: View`) must be able to extend `View` so
extensions can gate intents on capability groups (per scene R17).
Traits are markers only — they carry no required methods. Per-kind
affordances land on `Pane<V>` via `impl<V: CommandView> Pane<V>` /
`impl<V: ZellijView> Pane<V>` blocks (R4).

**Acceptance Criteria:**

- [ ] `rg -n "pub trait View\b" crates/ark-view/src/` prints exactly
  one hit.
- [ ] `rg -n "pub trait CommandView\s*:\s*View" crates/ark-view/src/`
  prints exactly one hit.
- [ ] `rg -n "pub trait ZellijView\s*:\s*View" crates/ark-view/src/`
  prints exactly one hit.
- [ ] `View` has no required methods. `CommandView` and `ZellijView`
  have no required methods of their own (they refine `View` only).
  Verified by a `cargo test -p ark-view` test that defines
  `struct X; impl View for X {} impl CommandView for X {}` and
  compiles.
- [ ] A downstream trait `trait DiffView: View {}` compiles cleanly
  against `ark-view`, verified by a `cargo test -p ark-view` trybuild
  or inline test.
- [ ] `View` is `Send + Sync + 'static` (or explicitly documented as
  not). Verified by a trait-object-bound compile test.

**Dependencies:** R1.

### R4: `Pane<V>`, `Stack<V>`, `TabHandle` + `PaneLike`

**Description:** `ark-view` defines `Pane<V: View>` and
`Stack<V: View>` as parametric wrappers around an opaque `Handle` plus
`PhantomData<V>`. `TabHandle` is non-parametric (tabs have no view
type). A common trait `PaneLike` exposes the handle + `emit` surface
every typed wrapper shares. Per scene R17 and decision #3a.
Marker-trait-gated affordance blocks expose `CommandView` methods
(`env`, `write_stdin`, `pid`) and `ZellijView` methods (`pipe`) only
when the view type implements the corresponding marker.

**Acceptance Criteria:**

- [ ] `rg -n "pub struct Pane<" crates/ark-view/src/` prints exactly
  one hit with signature `pub struct Pane<V: View>`.
- [ ] `rg -n "pub struct Stack<" crates/ark-view/src/` prints exactly
  one hit with signature `pub struct Stack<V: View>`.
- [ ] `rg -n "pub struct TabHandle\b" crates/ark-view/src/` prints
  exactly one hit; `TabHandle` is NOT generic.
- [ ] `rg -n "pub trait PaneLike" crates/ark-view/src/` prints exactly
  one hit. `PaneLike` exposes at minimum `fn handle(&self) -> &Handle`
  and `fn emit<E: Event>(&self, e: E)` (or an equivalent typed-event
  emit surface).
- [ ] `impl<V: View> PaneLike for Pane<V>` and
  `impl<V: View> PaneLike for Stack<V>` both exist, verified by a
  `cargo test -p ark-view` polymorphic test that accepts
  `&dyn PaneLike`.
- [ ] `impl<V: CommandView> Pane<V>` exposes `env`, `write_stdin`,
  `pid` methods. A negative trybuild test confirms these methods are
  NOT in scope when `V: ZellijView` and not `CommandView`.
- [ ] `impl<V: ZellijView> Pane<V>` exposes `pipe`. Symmetric negative
  trybuild test.
- [ ] `impl<V: View> Stack<V>` exposes `spawn_pane(attrs) ->
  Pane<V>`, `close_child(&Pane<V>)`, `children() -> Vec<Pane<V>>`,
  `clear()` (per scene R17).
- [ ] `Pane<V>`, `Stack<V>`, `TabHandle` are `Clone + Debug`.
- [ ] `Pane<V>` and `Stack<V>` serialise on the wire as their
  underlying opaque handle id string (see R5) — `V` is
  compile-time-only.

**Dependencies:** R1, R2, R3.

### R5: Handles on the wire — opaque strings

**Description:** Every typed handle (`Pane<V>`, `Stack<V>`,
`TabHandle`) collapses to a wire-format opaque string identifier
(`handle_id`). The `V` type parameter is compile-time-only and does
not cross the RPC boundary. Per decision #3b ("Type parameter V is
compile-time-only; does not cross the wire").

**Acceptance Criteria:**

- [ ] `ark-view` defines a `Handle` (or `HandleId`) type whose serde
  representation is a plain string.
- [ ] `Pane<V>`, `Stack<V>`, and `TabHandle` serialise through
  `serde_json` to the same string shape — verified by a `cargo test -p
  ark-view` test that constructs a `Pane<V>` with handle id
  `"abc-123"` and asserts the JSON is `"abc-123"` (not an object).
- [ ] No `V` type info appears in any serialised handle payload —
  verified by grep of round-tripped JSON for phantom-data artefacts.
- [ ] Deserialisation of a raw `"abc-123"` string into `Pane<V>` for
  any `V: View` succeeds — verified by a per-view-type roundtrip
  test.
- [ ] The `handle_id` format is documented as opaque; no Phase-2 code
  parses or pattern-matches its contents. Verified by `rg -n
  "handle.*split\|handle.*starts_with\|handle.*matches" crates/ark-view
  crates/ark-ext-proto` printing zero hits against handle payloads.

**Dependencies:** R1, R4.

### R6: Ext→host RPC method surface — method-per-op

**Description:** The ext→host direction of the RPC surface grows six
typed methods on `ArkExtension` (or the ext→host client type
consumers call), one per handle operation. Per decision #3b, each
method has its own request + response struct following the existing
`ark-ext-proto` one-struct-per-method convention. Generic
`ui_handle_request { action, ... }` shape is explicitly rejected.

Methods:

- `pane/emit { handle, event }` → ack
- `pane/replace_view { handle, view_body }` → ack
- `pane/close { handle }` → ack
- `stack/spawn_pane { stack, attrs }` → `{ handle }` (new pane id)
- `stack/close_child { stack, handle }` → ack
- `stack/clear { stack }` → ack

**Acceptance Criteria:**

- [ ] `crates/ark-ext-proto/src/lib.rs` (or a new submodule it
  re-exports) defines six request structs: `PaneEmitRequest`,
  `PaneReplaceViewRequest`, `PaneCloseRequest`, `StackSpawnPaneRequest`,
  `StackCloseChildRequest`, `StackClearRequest`. `rg -n "pub struct
  (PaneEmit|PaneReplaceView|PaneClose|StackSpawnPane|StackCloseChild|
  StackClear)Request" crates/ark-ext-proto/src/` prints six hits.
- [ ] A matching response struct exists for each (ack-shaped structs
  are permitted to be empty; per the crate convention every method has
  a dedicated response struct for MINOR-version evolution). Six `*Response`
  structs verified by grep.
- [ ] Every request struct carries the handle field as an opaque
  string (R5) — NO `V: View` type parameter on the wire structs.
  Verified by a compile test: none of the six request structs is
  generic over a `View` bound.
- [ ] Every request + response struct derives `Facet, Debug, Clone`
  and carries Rust `///` doc-comments on every field (per the existing
  `ark-ext-proto` LSP-hover convention).
- [ ] The JSON-RPC method names are exactly `"pane/emit"`,
  `"pane/replace_view"`, `"pane/close"`, `"stack/spawn_pane"`,
  `"stack/close_child"`, `"stack/clear"` — verified by a `cargo test
  -p ark-ext-proto` golden.
- [ ] `StackSpawnPaneResponse` returns a single `handle: HandleId`
  field (the new pane's opaque id). Callers typecast the result to
  `Pane<V>` in Rust; the type parameter is resolved from the
  `Stack<V>` the spawn was invoked on.
- [ ] Each method has a default `ArkExtension` trait impl returning
  `ExtensionError::method_not_found` (matching the existing
  convention at `crates/ark-ext-proto/src/lib.rs:1078-1086`).

**Dependencies:** R1, R4, R5. Cross-reference
`cavekit-soul-phase-2-host-dispatch.md` for the host-side dispatcher
that consumes these RPC methods.

### R7: Invalidation protocol — `ark.handle.invalidated` + `HandleGone`

**Description:** Every scene-declared handle has exactly one of three
termination causes. Per decision #3c, termination is signalled via
**both** an `ark.handle.invalidated { handle, cause }` `ExtEvent`
(broadcast on the event bus, push-style) AND a `HandleGone`
`ExtensionError` variant returned lazily on any subsequent op against
the dead handle (pull-style, belt-and-suspenders for race windows).
The three causes are `user_closed`, `scene_reload_dropped`,
`session_ended`. User-opened panes (no `ARK_HANDLE`) never produce a
typed handle and are therefore out of scope for invalidation.

**Acceptance Criteria:**

- [ ] An `InvalidationCause` enum exists (in `ark-view` or a location
  `ark-ext-proto` re-exports) with exactly these variants:
  `UserClosed`, `SceneReloadDropped`, `SessionEnded`. Verified by a
  `cargo test` exhaustiveness test.
- [ ] `InvalidationCause` serialises to the stable tag strings
  `"user_closed"`, `"scene_reload_dropped"`, `"session_ended"`.
- [ ] The `ark.handle.invalidated` event is specified with payload
  shape `{ handle: HandleId, cause: InvalidationCause }`. A
  `cargo test` golden asserts the JSON wire format.
- [ ] `ExtensionError` (or the Phase-2 additive variant on it) gains a
  `HandleGone { handle: HandleId, cause: InvalidationCause }` variant.
  `rg -n "HandleGone" crates/ark-ext-proto/src/` prints at least one
  hit.
- [ ] Contract: for any op in R6 called against an invalidated
  handle, the host MUST return `HandleGone` (not `method_not_found`,
  not a generic error). Specified in the R6 method doc-comments and
  covered by the tests sub-kit.
- [ ] The `session_ended` cause is documented as cascade-only: the
  host emits ONE session-ended marker (covered by the ext-surface
  sub-kit's `on_session_end` hook), NOT per-handle `invalidated`
  events, per decision #3c's "no per-handle events emitted" rule. A
  test asserts zero per-handle events during shutdown.
- [ ] User-opened panes (those without an `ARK_HANDLE` env wrapper —
  see scene R17 reconciler) never appear in any `invalidated` event
  and never produce `HandleGone` — they are invisible to extensions
  throughout their lifecycle. Covered by an integration test in the
  tests sub-kit.

**Dependencies:** R4, R5, R6.

### R8: User-close suppression — policy + params-hash override

**Description:** User-close suppression prevents the reconciler from
re-spawning a scene-declared pane that the user manually closed,
until the author materially changes the view's scene params. Per
decision #3d. This kit owns the POLICY and its invariants; the
`closed_by_user: Map<SceneHandleName, ParamsHash>` storage lives in
the supervisor (see `cavekit-soul-phase-2-host-dispatch.md`). Cross-
references are explicit; this kit does not re-specify supervisor
storage mechanics.

The six suppression invariants this kit locks:

1. On user-close of a scene pane, compute `params_hash` from the
   view's current resolved scene params; store `(handle_name,
   params_hash)` in the suppression set.
2. On every reconcile tick, for each declared pane, consult the set:
   absent → spawn; present with equal hash → skip spawn; present with
   differing hash → evict entry then spawn.
3. The suppression set is in-memory and session-scoped (its lifetime
   equals supervisor session lifetime; supervisor restart = new
   session = empty set).
4. `params_hash` is computed deterministically from the resolved
   scene params (Rhai-evaluated result, not source text) so that
   cosmetic KDL edits that produce identical params do NOT lift
   suppression.
5. Suppression applies ONLY to scene-declared top-level panes (those
   with a stable `handle_name` in the scene). Stack children are
   excluded — see R9.
6. User-close always fires `ark.handle.invalidated { cause:
   user_closed }` regardless of suppression state, so extensions
   observe the closure even if the host opts not to respawn.

**Acceptance Criteria:**

- [ ] This kit documents a `ParamsHash` type (byte-string or newtype
  over `[u8; 32]`-ish) with a stable hash function documented (e.g.
  blake3 of canonical-JSON-serialised resolved params). The exact
  hash algorithm is named here; supervisor sub-kit imports it.
- [ ] The six invariants above appear verbatim (or semantically
  equivalent) in a contract doc comment on the policy type
  (`SuppressionPolicy` or similar) — verified by `rg -n
  "SuppressionPolicy\|closed_by_user" crates/ark-view/src/`.
- [ ] The params-hash override rule (invariant #2 "differing hash →
  evict then spawn") has a dedicated acceptance test in the tests
  sub-kit (cross-referenced, not duplicated here).
- [ ] The policy is implementation-agnostic: this kit does not pin
  WHERE the reconciler consults the suppression set, only WHAT
  consulting it must yield. The ownership boundary (supervisor owns
  the map; reconciler reads it during tick) is stated once.
- [ ] `SceneHandleName` is the scene-author-written `@handle` name
  from scene R17, not the runtime opaque `HandleId`. The policy keys
  on the stable author-chosen name so suppression survives handle-id
  churn across reconciles.

**Dependencies:** R7. Cross-reference
`cavekit-soul-phase-2-host-dispatch.md` for storage impl.

### R9: Stack children excluded from user-close suppression

**Description:** User-close of a stack child pane is permanent for
that child instance and MUST NOT enter the suppression set. If the
extension's logical work continues, the extension re-spawns the child
via `stack/spawn_pane` (R6). Per decision #3d's last paragraph
("Stack-children are NOT subject to this suppression").

**Acceptance Criteria:**

- [ ] The suppression policy contract (R8) explicitly documents this
  exclusion — verified by `rg -n "stack.*child\|stack-child"
  crates/ark-view/src/` showing the exclusion in the policy type's
  doc-comment.
- [ ] User-close of a stack-child fires `ark.handle.invalidated {
  cause: user_closed }` (R7) but does NOT trigger any write to
  `closed_by_user`. Covered by a tests-sub-kit integration test.
- [ ] A re-invocation of `stack/spawn_pane` on the parent stack after
  a child user-close succeeds and produces a fresh pane with a new
  opaque `HandleId` — covered by a tests-sub-kit integration test.
- [ ] The exclusion is enforced by the policy itself, not by the
  reconciler: passing a stack-child `SceneHandleName` (if such a
  name existed) to the suppression-set insert API is either a
  compile-time or debug-assert error. Verified by a `cargo test -p
  ark-view` negative test.

**Dependencies:** R4, R7, R8.

### R10: Name-indexed handle lookup API

**Description:** Extensions currently receive handles only as opaque
`HandleId`s, yet the suppression mechanism (R8) is keyed on scene
handle names that can change their underlying id across reconciles
(user-close → params-change → evict → respawn = new id, same name).
Extensions need a name→handle lookup so they can re-attach after a
user-close-then-params-change sequence. Per decisions-doc open item
#3.

**Acceptance Criteria:**

- [ ] `ark-view` (or a crate it re-exports through) defines a
  `SessionHandles` (or equivalently named) context accessor that
  extensions receive in their session-start hook and that exposes at
  minimum:
  - `fn pane_by_name<V: View>(&self, name: &SceneHandleName) ->
    Option<Pane<V>>`
  - `fn stack_by_name<V: View>(&self, name: &SceneHandleName) ->
    Option<Stack<V>>`
  - `fn tab_by_name(&self, name: &SceneHandleName) -> Option<TabHandle>`
- [ ] The lookup returns `None` when the named handle is absent from
  the current reconciled scene (e.g. suppressed by user-close, or
  removed by scene edit) — NOT a stale `HandleGone`. A `cargo test`
  suppressed-handle lookup test asserts `None`.
- [ ] The lookup is a read against the host's current handle table;
  it does NOT produce an ext→host RPC call. Verified by a test that
  counts RPC calls during a lookup and asserts zero.
- [ ] Name resolution honours scene-scoped flat namespace (scene R17
  "Handle namespace: Flat, scene-scoped"). No crate-name or
  ext-name prefixing.
- [ ] Type-parameter `V` on the lookup matches against the scene-
  declared view type for that handle name. A mismatch (caller
  requests `Pane<X>` but scene declared the name as `Pane<Y>`)
  returns `None` AND emits a warn-level log; it is NOT a
  `HandleGone` error. This is the runtime-safety complement to
  scene's compile-time `error[scene/view-type-mismatch]` diagnostic
  (scene R17) and covers dynamic attach paths where the compile-time
  check can't run.

**Dependencies:** R4, R7, R8.

### R11: `ark-ext-derive` interface hooks — types only

**Description:** This kit owns the TYPES that `ark-ext-derive`
codegens against; the codegen itself lives in the ext-surface sub-
kit. Here we lock the interface boundary: which items of `ark-view`
must be nameable/importable from a derive macro, and which types must
carry `Facet` derives so their SHAPE is available to the derive
pipeline. Cross-references `cavekit-soul-phase-2-ext-surface.md` for
the actual `#[derive(View)]` / `#[derive(Extension)]` codegen spec.

**Acceptance Criteria:**

- [ ] `View`, `CommandView`, `ZellijView`, `Pane`, `Stack`,
  `TabHandle`, `PaneLike`, `HandleKind`, `InvalidationCause`,
  `HandleId` are all `pub` at `ark-view`'s crate root — verified by
  `cargo doc -p ark-view` producing a root-visible entry for each.
- [ ] All ark-view types that cross the wire (`HandleId`,
  `HandleKind`, `InvalidationCause`) derive `Facet` — verified by
  `rg -n "derive\(.*Facet.*\).*struct\s+(HandleId|HandleKind|
  InvalidationCause)" crates/ark-view/src/`.
- [ ] `ark-view` exposes no trait objects or `impl Trait` returns in
  its public API that would block `ark-ext-derive`'s codegen —
  verified by `cargo check -p ark-ext-derive` against a minimal
  extension that uses every typed handle.
- [ ] A stability marker (e.g. `#[non_exhaustive]` on
  `InvalidationCause` and `HandleKind`) preserves MINOR-version
  evolution room for new causes / kinds without breaking downstream
  derives. Verified by a `cargo test` compile-fail trybuild
  asserting exhaustive match on these enums requires the catch-all
  arm.
- [ ] This kit explicitly delegates the derive-codegen surface
  (`#[derive(View)]`, marker-trait detection, typed view-attr
  generation) to `cavekit-soul-phase-2-ext-surface.md` via a single
  "see also" reference; no codegen spec appears in this kit.

**Dependencies:** R2, R3, R4, R5, R7.

## Out of Scope

- Lifecycle hooks (`on_session_start`, `on_session_end`) and their
  session-context parameter shape — covered by
  `cavekit-soul-phase-2-ext-surface.md`.
- Manifest registration, `ark-ext-metadata` schema evolution,
  capability flag emission from manifest — covered by
  `cavekit-soul-phase-2-ext-surface.md`.
- `#[derive(View)]` / `#[derive(Extension)]` codegen implementation —
  codegen proper belongs to `cavekit-soul-phase-2-ext-surface.md`.
  This kit only guarantees the derive-addressable type surface (R11).
- Host-side dispatcher that receives the R6 RPC calls and routes them
  to zellij + supervisor — `cavekit-soul-phase-2-host-dispatch.md`.
- Supervisor storage of the `closed_by_user` map, reconcile-tick
  hook wiring, figment fan-in for ext config — `host-dispatch`
  sub-kit.
- `ark list` / `ark doctor` / scene validator fan-in — `host-
  dispatch` sub-kit.
- Stub-ext harness, version-mismatch matrices, invalidation-race
  tests, suppression-override goldens — `cavekit-soul-phase-2-tests.md`.
- Scene KDL grammar for `@handle` names + view attrs — already
  locked by scene R17; this kit consumes it unchanged.
- `ArkExtension` host→ext methods (intent dispatch, event notify,
  lifecycle) — pre-existing surface; Phase 2 adds only the ext→host
  methods in R6.

## Cross-References

- Parent kit: `cavekit-soul.md` (Phase 2 section at lines 581-590,
  which this kit replaces).
- Decisions: `context/plans/phase-2-design-decisions.md` §3 (ark-view
  crate home, RPC shape, invalidation taxonomy, user-close
  suppression) — AUTHORITATIVE for every decision in this kit.
- Handoff: `context/plans/handoff-2026-04-18-claude-code-first-pivot.md`
  lines 285-328 (gap analysis — this kit closes gaps #3 typed-handle
  runtime API and partially closes #7 back-compat around new methods).
- Scene contract: `cavekit-scene.md` R17 (typed-handle compile-time
  contract that R4 implements at runtime).
- Phase 1 foundation: `cavekit-soul-phase-1-types.md` R6 (`ExtEvent`
  shape that `ark.handle.invalidated` uses).
- Sibling Phase 2 kits:
  - `cavekit-soul-phase-2-ext-surface.md` — manifest, lifecycle,
    derives.
  - `cavekit-soul-phase-2-host-dispatch.md` — dispatcher, supervisor
    storage, fan-in.
  - `cavekit-soul-phase-2-tests.md` — stub ext, matrices, goldens.
- Ext consumer (v0.1): `cavekit-claude-code.md` — first consumer of
  this kit's typed-handle surface.
- Ext consumer (v0.2): `cavekit-pi.md` — second consumer; validates
  the surface generalises.

## Changelog

(empty)
