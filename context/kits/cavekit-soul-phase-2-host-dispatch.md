---
created: "2026-04-18T00:00:00Z"
last_edited: "2026-04-18T00:00:00Z"
parent: cavekit-soul.md
phase: 2
status: draft
---

# Cavekit: Soul Phase 2 — Host Dispatch (ark-side fan-in)

## Scope

The ark-HOST side of Phase 2. How the supervisor and CLI CONSUME what
extensions declare: dispatches hooks, assembles `ark list` columns, runs
`ark doctor` checks, layers extension-declared config sections into
figment, validates scene KDL against extension-declared view symbol
tables, dispatches reload gates, and enforces capability-aware calls on
all Phase-2 RPC methods.

This kit is exclusively ark-internal. It does NOT define:

- `ArkExtension` trait method signatures, defaults, or the manifest
  format — those belong to the `ext-surface` sibling sub-kit.
- `View`, `Pane<V>`, `Stack<V>` types, marker traits, or handle-op RPC
  method wire shapes — those belong to the `ark-view` sibling sub-kit.
- Test harness / version-mismatch + capability-matrix goldens — those
  belong to the `tests` sibling sub-kit.

All cross-cutting decisions are locked in
`context/plans/phase-2-design-decisions.md`. Decision IDs are cited
inline (e.g. `[decision #4a]`).

## Requirements

### R1: `ark list` column fan-in

**Description:** `ark list` assembles its rendered column set by
iterating every loaded extension and collecting each extension's
declared list columns (via whatever manifest/trait surface the
`ext-surface` sub-kit defines). Core columns (`id`, `name`, `status`)
always render first in that fixed order; extension-declared columns
follow in a stable deterministic order — alphabetical by extension
name, then by column declaration order WITHIN each extension's
declaration list.

Columns are only collected from extensions that advertise the
`ext.list_columns.v1` capability flag [decision #4a]; extensions lacking
the capability contribute nothing and are not called.

**Acceptance Criteria:**
- [ ] `crates/cli/src/commands/list.rs` (or its replacement) iterates
      the loaded-extension set via the supervisor's extension registry
      rather than hard-coding column producers.
- [ ] Core columns `id`, `name`, `status` render at positions 0, 1, 2
      of the column vector in that order regardless of how many
      extensions contribute.
- [ ] Two extensions `ext-b` and `ext-a` each declaring columns
      `[c1, c2]` produce final extension-column ordering
      `[ext-a.c1, ext-a.c2, ext-b.c1, ext-b.c2]` (alpha by ext, then
      declaration order), verified by a table-test in
      `crates/cli/tests/` using stub extensions.
- [ ] An extension without the `ext.list_columns.v1` capability
      advertised is skipped entirely — `list_columns()` is never
      invoked against it — verified by a stub-extension test asserting
      zero calls on the stub.
- [ ] `ark list --json` serialises the column order identically (same
      stable ordering rule) so downstream consumers see determinism.
- [ ] An extension whose `list_columns` call panics, errors, or
      returns `method_not_found` despite advertising the capability
      is logged at WARN and its columns are simply absent from the
      rendered row; the whole `ark list` command does NOT fail.

**Dependencies:** `ext-surface` sub-kit (manifest / trait surface for
column declarations).

### R2: `ark doctor` check runner fan-in

**Description:** `ark doctor` iterates loaded extensions, collects each
extension's declared health checks (via the `ext-surface` sub-kit's
`doctor_checks()` surface), and runs each check. Results aggregate into
a per-extension report with per-check granularity — a failed check
never cancels or masks the remaining checks of that same extension, and
an extension's failures never cancel another extension's checks.

Each check resolves to exactly one of `ok | warn | fail`, with an
associated message. The aggregated report groups checks under their
owning extension's name. Core checks (disk, state dir writability, mux
reachability, etc.) render as a synthetic `core` group, first in the
output.

Only extensions advertising `ext.doctor.v1` [decision #4a] are queried.

**Acceptance Criteria:**
- [ ] `crates/cli/src/commands/doctor.rs` iterates the extension
      registry and collects checks rather than hard-coding the check
      list.
- [ ] An extension contributing three checks where check #2 returns
      `fail` still produces report entries for checks #1 and #3 —
      verified by a stub-extension test asserting three entries in the
      report for that extension.
- [ ] Two extensions where ext-A's first check panics produce a
      report with ext-A partially reported (remaining checks after
      the panic may be skipped with a `skipped` entry carrying the
      panic reason) AND ext-B's checks fully reported — verified by a
      stub-extension test.
- [ ] Exit code of `ark doctor` is non-zero iff any check resolved to
      `fail` (warnings do not fail the command); verified by a
      `cargo test -p ark-cli` exit-code test.
- [ ] `ark doctor --json` emits `{ group, check_id, status, message }`
      rows where `status` is the string `ok` | `warn` | `fail` |
      `skipped`.
- [ ] An extension lacking `ext.doctor.v1` is never queried for
      checks, verified by a stub-extension call-count assertion.

**Dependencies:** `ext-surface` sub-kit.

### R3: Figment config-section layering

**Description:** Extensions declare config sections via their manifest
(format per `ext-surface` sub-kit). At supervisor boot, ark's figment
layer loads each extension's declared section under the key prefix
`[extension.<ext-name>]` in the merged TOML tree, in registration order
of the extensions. Section schemas use the facet SHAPE types
co-declared in `crates/ark-ext-metadata-types/` (schema cross-reference
via `ext-surface` sub-kit); figment deserialises each section against
its declared SHAPE and surfaces schema-validation errors with the
originating extension name + section path in the error chain.

Missing optional sections deserialise as empty/default. Missing
required sections fail supervisor boot with a message naming the
extension and required section; the supervisor exits non-zero without
starting the session loop.

**Acceptance Criteria:**
- [ ] `crates/config/src/lib.rs` (or equivalent) exposes a config
      loader that accepts the extension-manifest set and produces a
      merged figment `Figment` whose keys include
      `extension.<ext-name>.<section>` for every advertised section.
- [ ] Two extensions declaring the same section name produce
      `extension.ext-a.<sec>` and `extension.ext-b.<sec>` — no key
      collision — verified by a `cargo test -p ark-config` test.
- [ ] A TOML file with `[extension.ext-a.feature]` populates ext-a's
      deserialised section but leaves other extensions' sections at
      default, verified by a `cargo test -p ark-config` round-trip
      test.
- [ ] A schema violation (e.g. wrong type for a facet-declared field)
      produces an error whose string representation contains the
      extension name AND the section path — verified by a
      `cargo test -p ark-config` error-message test.
- [ ] A missing required section fails `Config::load()` with
      `Err(_)` containing the extension name; supervisor boot path
      (`crates/supervisor/src/bootstrap.rs`) propagates the error and
      exits before any extension lifecycle method is called.
- [ ] A missing optional section deserialises to the SHAPE's default
      value without error.

**Dependencies:** `ext-surface` sub-kit (manifest + SHAPE declaration
format).

### R4: Scene compile-time view-type validator

**Description:** `crates/scene/src/compile/` reads the extension
manifest set (via the registry snapshot the supervisor exposes) and
builds a symbol table of every extension-declared view name and its
handle-typed attribute shape (from the `ark-view` sub-kit's `View` /
`Pane<V>` / `Stack<V>` types). Scene KDL compile-time validation
references the symbol table for every `pane` and `stack` alias
declaration: if a view name is unknown, or a declared attribute's type
mismatches the SHAPE the extension registered, compile fails with a
locatable error pointing to the offending KDL line and column.

The symbol table is built BEFORE any `OpNode` lowering occurs, so
downstream compile passes can trust view references. Scene compile
treats the symbol table as a compile-time input (cacheable with a hash
over the extension-manifest set) — the validator runs whether the
extension is loaded in-process or subprocess.

**Acceptance Criteria:**
- [ ] `crates/scene/src/validate/pane_views.rs` (or a replacement
      module) accepts a `ViewSymbolTable` built from the extension
      registry and validates every `pane` and `stack` alias in the
      KDL AST against it.
- [ ] A KDL snippet declaring
      `stack @subs { subagents: Stack<NonExistentView> }` fails
      `Scene::compile` with an error whose message contains the
      string `NonExistentView`, the source file path, and a line /
      column pointer — verified by a `cargo test -p ark-scene` test.
- [ ] A KDL snippet declaring
      `pane @x { view: DeclaredView }` against an extension that DID
      register `DeclaredView` compiles successfully — verified by a
      passing test using a stub-extension manifest.
- [ ] A KDL snippet declaring a handle-typed attribute whose SHAPE
      does not match the extension's declared attribute SHAPE (e.g.
      scalar declared, vec used) fails compile with a shape-mismatch
      error referencing both the view name and attribute name.
- [ ] The view symbol table's construction is pure and reproducible
      given a fixed set of extension manifests — verified by a
      `cargo test -p ark-scene` test hashing two constructions of the
      same table to equal values.
- [ ] Compile errors raised by this validator are ordinary
      `scene::Error` values — no panic; integration with
      `ark scene check` surfaces them via the existing error path.

**Dependencies:** `ark-view` sub-kit (SHAPE of view + attr decls);
`ext-surface` sub-kit (how exts ship the manifest).

### R5: Reload-gate dispatcher

**Description:** On every scene reload attempt, the supervisor iterates
the list of reload gates declared across loaded extensions (manifest
source per `ext-surface` sub-kit). Each gate invocation returns one of
`Proceed` or `Defer { reason: String }`. Multiple gates are ANDed —
reload proceeds iff every gate returns `Proceed`. If ANY gate returns
`Defer`, reload is held and the supervisor surfaces the defer reason
(with the contributing extension name) to the status writer and to any
`ark` CLI client driving the reload (e.g. `ark scene reload`). The
deferred reload does not retry automatically; reload is re-attempted
only when the next trigger fires (scene file change, explicit
`ark scene reload` invocation, etc.).

Only extensions advertising `ext.reload_gate.v1` [decision #4a] are
queried. A gate that errors or returns `method_not_found` despite
advertising the capability is treated as `Proceed` with a WARN log
(fail-open — a broken gate must not permanently block reloads).

**Acceptance Criteria:**
- [ ] `crates/supervisor/src/scene_runtime.rs` (or the reload path)
      invokes every advertised gate before calling the reconciler.
- [ ] Two gates both returning `Proceed` allow reload to proceed;
      verified by a stub-extension integration test.
- [ ] One gate returning `Defer { reason: "in-flight task" }` holds
      reload; the reconciler is NOT called; the status writer
      receives a structured `reload.deferred` event whose payload
      includes `ext`, `reason`; verified by a stub-extension test.
- [ ] With two gates where one defers and one proceeds, reload is
      held (ANDed); verified by a stub-extension test.
- [ ] A gate that errors is logged at WARN and counted as `Proceed`;
      verified by a stub-extension test that fails the gate method
      and asserts reload completes.
- [ ] No automatic retry loop — a deferred reload only re-runs when
      an external trigger fires, verified by a test that defers once
      and asserts no further gate calls for N seconds.

**Dependencies:** `ext-surface` sub-kit.

### R6: Capability-aware RPC dispatch

**Description:** The supervisor's RPC dispatcher gates every Phase-2
method call on the calling extension's advertised capability set
[decision #4a]. On extension initialisation the supervisor reads
`InitializeResponse.extension_capabilities` and records the capability
set per-extension-session. Before calling any Phase-2 method, the
dispatcher checks whether the method's declaring capability is in the
set. Absent → the dispatcher skips the call entirely (silent no-op, no
log). Present but the method returns JSON-RPC `-32601`
`method_not_found` → logged at WARN once per (ext, method) pair and the
feature is treated as unavailable for the remainder of that extension
session (no repeated WARN spam).

Capability-to-method mapping is a static table in the dispatcher that
maps each capability flag (e.g. `view.pane.v1`, `ext.doctor.v1`) to
the set of RPC methods it guards. The taxonomy is co-defined with the
`ext-surface` sub-kit; this kit consumes, it does not define.

**Acceptance Criteria:**
- [ ] Every Phase-2 method call site in `crates/supervisor/` routes
      through a single dispatch entry point that performs the
      capability check; no method is called via a direct client-call
      path that bypasses the check. Verified by a grep for direct
      `*Client::` RPC calls against Phase-2 methods — they all go
      through the dispatcher.
- [ ] An extension session whose advertised capabilities omit
      `view.pane.v1` never receives `pane/emit`, `pane/replace_view`,
      or `pane/close` calls — verified by a stub-extension call-count
      assertion.
- [ ] An extension advertising `view.pane.v1` but returning
      `method_not_found` for `pane/emit` causes exactly ONE WARN log
      for that (ext, `pane/emit`) pair; subsequent emits do not
      re-log; verified by a stub-extension test.
- [ ] Once a method has been marked unavailable for an extension
      session via `method_not_found`, dispatcher skips future calls
      to that method for that session without invoking the RPC.
- [ ] Skipped calls do NOT propagate as errors to callers inside
      supervisor code — the feature is semantically "this extension
      doesn't support X", which is a non-error state.

**Dependencies:** `ext-surface` sub-kit (capability taxonomy).

### R7: Host-declared capabilities on handshake

**Description:** During the initialise handshake, the supervisor
populates `InitializeRequest.client_capabilities` with the feature
groups THIS ark binary supports [decision #4b]. The capability set
matches the same taxonomy used on the extension side (e.g.
`view.pane.v1`, `view.stack.v1`, `ext.lifecycle.v1`,
`ext.doctor.v1`, `ext.list_columns.v1`, `ext.reload_gate.v1`), per the
`ext-surface` sub-kit's naming convention. The capability set is a
compile-time constant for a given ark release — populated once at
supervisor startup, not per-extension — so an extension can trust the
handshake is consistent across its concurrent sessions.

Extensions use the handshake response to downgrade behaviour when ark
lacks a capability they want (e.g. an ext fallback rendering path if
`view.stack.v1` is absent). This kit only specifies that ark advertises
its capabilities; extension-side interpretation is extension-level and
out of scope here.

**Acceptance Criteria:**
- [ ] `InitializeRequest.client_capabilities` sent from supervisor to
      every extension at handshake contains every Phase-2 capability
      flag ark supports, as a deterministic sorted list.
- [ ] The capability list is identical across two concurrent
      extension handshakes within the same supervisor process,
      verified by an integration test capturing both handshakes.
- [ ] A new capability added in a future version appears in the list
      without altering the structural format — consumers depending on
      specific flags see them; consumers ignoring unknown flags
      proceed unharmed.
- [ ] The ark version string (proto MAJOR.MINOR) is sent alongside
      the capability list per the existing `InitializeRequest`
      shape — this kit does not change the version field, only the
      capabilities population.

**Dependencies:** `ext-surface` sub-kit (capability taxonomy).

### R8: Extension load sequence

**Description:** Extension loading is a single linear sequence ark
performs per extension. The order is:

1. **Read manifest** — parse the extension's declared manifest
   (shape per `ext-surface` sub-kit).
2. **Initialise RPC handshake** — open the transport, send
   `InitializeRequest` (with host capabilities per R7), receive
   `InitializeResponse` (extension capabilities).
3. **Validate capabilities** — intersect extension-advertised
   capabilities with ark-supported capabilities; log at INFO the
   negotiated feature set; refuse to complete load if a manifest-
   declared feature group is not supported by ark (refusal surfaces
   as a boot-time error naming the extension and unsupported
   capability).
4. **Register intents** — for every intent declared in the manifest,
   construct an `Arc<dyn Intent>` shim that dispatches to
   `intent/dispatch` RPC on this extension session, and insert it
   into the shared `IntentRegistry` [decision #2]. Intents are a
   manifest-only path in v0.1; no `intent_register` RPC.
5. **Register views** — for every view declared in the manifest,
   insert it into the scene view symbol table (feeding R4) and the
   runtime view-dispatch registry.
6. **Register gates** — for every reload gate declared, register it
   in the reload-gate list (feeding R5).
7. **Ready** — the extension transitions to a `Ready` state in the
   supervisor registry; subsequent Phase-2 calls may target it via
   the capability-aware dispatcher (R6).

Any step failing aborts the load and transitions the extension to a
`Failed { reason }` state; later extensions still load (one bad
extension does not block the rest). Failed extensions contribute no
columns, no doctor checks, no gates, and are invisible to the scene
validator.

**Acceptance Criteria:**
- [ ] The load sequence is implemented in a single function in
      `crates/supervisor/src/` whose steps are observable in order
      (e.g. via structured logs or a test-mode hook). Verified by a
      stub-extension test that asserts step order via log capture.
- [ ] A stub extension whose manifest parse fails transitions to
      `Failed` without calling any later step; verified by a test.
- [ ] A stub extension whose handshake times out transitions to
      `Failed`; later extensions in the load list still reach
      `Ready`; verified by a multi-extension test.
- [ ] A stub extension whose manifest declares a capability ark does
      not support transitions to `Failed` with an error naming the
      unsupported capability; verified by a test.
- [ ] Intents registered via the manifest path appear in
      `IntentRegistry` and dispatch correctly to the extension's
      `intent/dispatch` RPC — verified by an integration test sending
      an intent and asserting the RPC was invoked.
- [ ] No `intent_register` RPC method is exposed on `ArkExtension`
      (cross-check with `ext-surface` sub-kit) — verified by grep:
      `rg -n "intent_register" crates/` prints zero hits outside of
      deletion comments or historical docs.

**Dependencies:** `ext-surface` sub-kit (manifest + RPC surface).

### R9: User-close suppression map storage

**Description:** The supervisor owns a session-scoped in-memory map
`closed_by_user: Map<SceneHandleName, ParamsHash>` per decision #3d.
Lifetime = supervisor session lifetime (no persistence across
supervisor restart). The supervisor writes to this map on two triggers:

- On zellij pane-list delta, when a pane is detected as closed AND the
  closed pane lacked an `ARK_HANDLE` marker in the env-vars (i.e. the
  user closed it via zellij keybind, not via ark's scene-driven
  close path), the supervisor records
  `(scene_handle_name, hash_of_current_scene_params)` into the map.

- The map is READ during scene reconcile: for each declared pane in
  the new scene, the reconciler consults this map via a lookup by
  scene handle name and applies the suppression policy defined in the
  `ark-view` sub-kit (skip-spawn when hash matches, evict-and-spawn
  when hash differs, spawn normally when absent).

This kit owns STORAGE + write trigger detection + read API for the
reconciler. The POLICY of how reconcile uses the lookup (params-hash
comparison, eviction-on-mismatch, stack-child exclusion) is defined
in the `ark-view` sub-kit.

Stack-children are NEVER entered into this map — the trigger only
fires for top-level scene-declared panes. User-closed user-spawned
panes (no `ARK_HANDLE`) are not scene-declared and have no
`SceneHandleName`; they are filtered out before the map write.

**Acceptance Criteria:**
- [ ] `crates/supervisor/src/` defines a session-scoped structure
      containing `closed_by_user: BTreeMap<String, ParamsHash>` (key
      is the scene handle name; `BTreeMap` for deterministic
      iteration).
- [ ] The map is constructed fresh at supervisor session start; its
      contents do not carry across a supervisor restart — verified
      by an integration test starting supervisor, recording an entry,
      restarting supervisor, and asserting the map is empty.
- [ ] When a zellij pane-close delta reports a pane lacking
      `ARK_HANDLE` that maps to a known scene handle name, the
      corresponding `(name, params_hash)` is inserted; verified by a
      pane-close simulation test.
- [ ] When a zellij pane-close delta reports a pane WITH
      `ARK_HANDLE` (i.e. ark-driven close), NO entry is written;
      verified by a test.
- [ ] Stack-child closure does not produce entries in this map;
      verified by a test simulating a stack-child close.
- [ ] The reconciler has a read API
      (e.g. `closed_by_user.lookup(name) -> Option<ParamsHash>`)
      that returns the stored hash without mutating the map; the
      reconciler's policy application (including eviction on
      mismatch) is defined in the `ark-view` sub-kit and consumes
      this API.
- [ ] `ark.handle.invalidated { cause: user_closed }` ExtEvent is
      broadcast at the same trigger point that writes the map entry;
      the event carries the scene handle name and MUST be emitted
      after the map write has succeeded (ordering guarantees ext
      subscribers can re-query state coherently).

**Dependencies:** `ark-view` sub-kit (ParamsHash shape, policy
semantics, invalidation event shape).

## Out of Scope

- `ArkExtension` trait method shapes, manifest format, derive-macro
  codegen. See `ext-surface` sub-kit.
- `View`, `Pane<V>`, `Stack<V>` types and every RPC method in
  `pane/*` and `stack/*`. See `ark-view` sub-kit.
- Any test harness, stub-extension implementation, or compat-matrix
  goldens. See `tests` sub-kit.
- Dynamic runtime intent registration (`intent/register_dynamic` or
  similar). Deferred to v0.2 [decision #2].
- ACP `permission_dispatcher`. Removed from Phase 2 entirely
  [decision #1].
- Proto MAJOR/MINOR bump wiring. Bumping `CURRENT_PROTOCOL_VERSION`
  from `1.0` to `1.1` at Phase 2 completion is noted in the decisions
  doc [decision #4c] but the bump itself is a one-line constant edit
  owned by the `ext-surface` sub-kit.

## Cross-References

- Decisions: `context/plans/phase-2-design-decisions.md`.
- Parent: `cavekit-soul.md` (Phase 2 section + Resolved Decisions).
- Sibling: `cavekit-soul-phase-2-ext-surface.md` (manifest format,
  trait methods, capability taxonomy).
- Sibling: `cavekit-soul-phase-2-ark-view.md` (`View`, `Pane<V>`,
  `Stack<V>`, handle-op RPC, invalidation protocol, suppression
  policy semantics).
- Sibling: `cavekit-soul-phase-2-tests.md` (integration test harness,
  version/capability matrix goldens).
- Phase 1 prior: `cavekit-soul-phase-1-types.md` (types this kit
  consumes: `SessionId`, `ExtEvent`, `ExtExtension`-registry, etc.).

## Open Items

- **Capability-to-method mapping table location.** R6 requires a
  static table mapping capability flags to RPC method names. Whether
  this table lives in the dispatcher (this kit) or in the
  `ext-surface` sub-kit (alongside the taxonomy) is a minor
  factoring question; if placed in `ext-surface`, this kit consumes
  it as a typed import. Decomposition agent for `ext-surface` to
  resolve.
- **Name-indexed handle lookup (decisions doc open item #3).** R9's
  read API is indexed by scene handle name. The decisions doc notes
  extensions currently see opaque handle IDs, not handle names. If
  extensions need a name-indexed re-attach API on `Pane<V>`, that
  surface lives in the `ark-view` sub-kit — this kit's storage is
  name-keyed regardless.

## Changelog

(empty)
