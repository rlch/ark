---
created: "2026-04-18"
last_edited: "2026-04-18"
parent: cavekit-soul.md
phase: 2
status: draft
---

# Cavekit: Soul Phase 2 — Tests + Stub Harness

## Scope

Defines the test surface Phase 2 requires: a reusable in-process /
subprocess stub-extension harness, a back-compat matrix exercising the
locked version + capability gating policy, view-type compile-error
goldens, and integration tests for manifest-driven intent registration,
user-close suppression, and handle invalidation.

This kit does NOT define trait signatures (`ext-surface` sub-kit), host
dispatcher behavior (`host-dispatch` sub-kit), or view types /
invalidation protocol (`ark-view` sub-kit) — it only describes tests
that exercise those contracts. Decisions locked in
`context/plans/phase-2-design-decisions.md`:

- Decision #2 — manifest-driven intent registration (single source of truth).
- Decision #3c — invalidation taxonomy (`user_closed`, `scene_reload_dropped`, `session_ended`).
- Decision #3d — user-close suppression keyed by `(handle_name, params_hash)`.
- Decision #4a — two-tier capability-flag + `method_not_found` gate.
- Decision #4b — host advertises `client_capabilities` in handshake for forward compat.
- Decision #4c — MAJOR vs MINOR semantics; Phase 2 = MINOR bump (1.0 → 1.1).

The harness requirements deliberately keep names unsettled (R1 proposes
a crate home and flags the decision as open); sibling sub-kits converge
on the final name in their own R's.

## Requirements

### R1: `StubExtension` in-process harness

**Description:** A reusable fixture crate provides a `StubExtension`
struct implementing `ArkExtension` whose per-method behavior is
configured at construction time. Used by claude-code and pi integration
tests, and by every Phase 2 host-dispatch test that needs an ext to
call into.

The stub MUST be feature-gated (test-only cfg / dev-dependency /
`[features]` flag) so it cannot leak into production binaries. Proposed
crate home: `crates/ark-ext-test-support` — see Open Items if the
parent decisions doc settles a different name.

The stub MUST be configurable along four independent axes:

1. **Hook toggles** — for each Phase-2 host-dispatch method (all
   `pane/*`, `stack/*`, lifecycle hooks, doctor, list-columns, reload
   gates), the test may register a closure `Fn(params) -> response` OR
   leave the method at its trait-default (`method_not_found`) OR mark it
   as "advertised-but-unimplemented" so the stub returns
   `method_not_found` even though the advertised capability bag claims
   support.
2. **Capability advertisement** — the set of capability flags the stub
   returns in its `InitializeResponse.extension_capabilities` is set
   per-test; empty set is valid.
3. **Manifest content** — the manifest-equivalent bag the loader sees
   (declared intents, declared view types, declared reload gates)
   is set per-test; defaults to empty.
4. **Protocol version reported in handshake** — defaults to
   `CURRENT_PROTOCOL_VERSION`; tests may override to exercise MAJOR /
   MINOR mismatches (see R3).

**Acceptance Criteria:**
- [ ] `rg -n "pub struct StubExtension" crates/` finds exactly one definition and it lives in a crate gated such that no production `[dependencies]` edge reaches it (verified by `cargo metadata --format-version=1 | jq` walking from `ark-supervisor` / CLI crates; the stub crate MUST appear only under `dev-dependencies` or behind a `test-support` feature).
- [ ] `StubExtension::builder()` (or equivalent builder API) exposes at least these configuration points: `.with_method(name, handler)`, `.advertise_capabilities(iter)`, `.with_manifest(manifest)`, `.with_protocol_version(ProtocolVersion)`, `.method_advertised_but_unimplemented(name)`, verified by unit tests inside the stub crate constructing one of each permutation.
- [ ] A test `stub_respects_hook_toggle` constructs two `StubExtension` instances differing only in one method override and observes the dispatched call on exactly one of them.
- [ ] A test `stub_capability_advertisement_round_trip` verifies the handshake response the host receives contains exactly the advertised capability set — no hidden defaults injected by the stub.
- [ ] A test `stub_manifest_visibility` verifies the loader-facing manifest accessor returns exactly what the test configured (no auto-injected entries).

**Dependencies:** `ext-surface` sub-kit (defines which methods exist).

### R2: NDJSON subprocess variant of the stub harness

**Description:** The stub crate from R1 ALSO ships a test binary target
(e.g. `cargo run --bin ark-stub-ext -- …`) that deserializes its
configuration from CLI args or env vars, then speaks NDJSON over stdio
using the existing `crates/ark-ext-proto/src/transport/ndjson.rs`
server. Used by full-roundtrip tests where ark spawns the ext via its
real supervisor code path (process launch + stdio wiring + handshake
+ dispatch).

The configured behavior MUST be identical (same hook-toggle, capability,
and manifest semantics) whether the stub is used in-proc or in
subprocess mode. Anything that passes against the in-proc stub MUST
pass against the subprocess stub for any method that also runs over
NDJSON, excluding genuinely transport-only concerns (timeouts,
cancellation).

**Acceptance Criteria:**
- [ ] The stub crate declares a `[[bin]]` target (or `[[test]]` bin) that accepts configuration input — either `--config <path>` pointing at JSON/KDL, an `ARK_STUB_CONFIG` env var, or a serialized arg string — sufficient to express the full R1 configuration axes.
- [ ] A round-trip test `stub_subprocess_matches_in_proc` runs the same conformance case against both transports (paralleling the existing `both_transports!` macro in `crates/ark-ext-proto/tests/conformance.rs`) and asserts identical observed responses for at least one method per Phase-2 feature group.
- [ ] A test `supervisor_spawns_stub_and_dispatches` uses the ark supervisor's real extension-launch path (not a hand-rolled `tokio::spawn`) to start the stub binary, perform handshake, and dispatch a `pane/emit` call; observing the call on the stub side confirms end-to-end wiring.
- [ ] The subprocess stub exits cleanly on `shutdown` RPC; an `assert_subprocess_exit_code(0)` guards against leaks.

**Dependencies:** R1, `host-dispatch` sub-kit (for supervisor launch path).

### R3: Version-mismatch test matrix

**Description:** Tests enumerate the outcomes of (ark proto version ×
ext proto version) handshake permutations per decision #4c.

Required cells:

| Ark MAJOR.MINOR | Ext MAJOR.MINOR | Expected outcome |
|---|---|---|
| 1.1 | 1.1 | Handshake OK, no warnings. |
| 1.1 | 1.0 | Handshake OK (same MAJOR, older MINOR tolerated). Capability bag on ext side is smaller; capabilities-added-in-1.1 simply aren't advertised. No warnings logged against the ext. |
| 1.0 | 1.1 | Handshake OK (same MAJOR, newer MINOR on ext side). Best-effort mode: ark logs a WARN enumerating capabilities the ext advertises that this ark doesn't recognize, but does NOT reject. |
| 2.0 | 1.1 | `HandshakeRejected` / `UnsupportedVersion` surfaced to caller; ext process torn down; no further RPCs attempted. |
| 1.1 | 2.0 | Same as above (symmetric). |

**Acceptance Criteria:**
- [ ] `rg -n "version_mismatch|handshake_version" crates/ark-ext-proto/tests/` finds a parameterized test covering each row above with a distinct `#[tokio::test]`.
- [ ] The `1.0 ↔ 1.1` rows assert that a WARN is captured in a test-attached log subscriber (e.g. `tracing_subscriber::fmt::test_writer`) with a message mentioning the unrecognized capability name; absence of the WARN fails the test.
- [ ] The `2.0 ↔ 1.1` rows assert the supervisor's ext-spawn result surfaces `ExtensionError::UnsupportedVersion(_)` (or the equivalent named variant in `ark-ext-proto`) and that NO subsequent RPC is attempted against the ext (observable by the stub's call-log being empty).
- [ ] Each row is driven by the R1 stub's `.with_protocol_version(...)` knob — no hand-rolled server wiring.

**Dependencies:** R1, R2, decision #4c.

### R4: Capability-gate test matrix

**Description:** Tests exhaust the four cases locked by decision #4a:

(a) **Advertised + implemented** — ext advertises `view.pane.v1`, host
calls `pane/emit`, call reaches stub handler. Observable: stub records
the call.

(b) **Not advertised** — ext does NOT advertise `view.pane.v1`, host's
reconcile path needs to call `pane/emit`. Expected: host SKIPS the
call entirely (does not even attempt RPC). Observable: stub call-log
empty AND no wire bytes sent (verifiable via an instrumented transport
or by using a blackhole transport and asserting no bytes written).

(c) **Advertised but unimplemented** — ext advertises `view.pane.v1`
but its handler returns JSON-RPC `-32601 method_not_found`. Expected:
host dispatcher logs a WARN (distinct from the R3 version warnings —
this one names the method and capability) and CONTINUES the reconcile
loop rather than crashing the session.

(d) **Method advertised as removed-in-MAJOR vs older ext** — for a
capability version that was removed between MAJOR versions, an older
ext that still implements it is NOT called on hosts that have dropped
support. (Phase 2 has no such method yet; the test is a goldens-style
stub that documents the expected shape and is xfail-marked until a
real removal occurs OR validated against a synthetic example.)

**Acceptance Criteria:**
- [ ] Four test functions (`capability_gate_advertised_and_called`, `capability_gate_not_advertised_skipped`, `capability_gate_advertised_but_unimplemented_warns`, `capability_gate_removed_method_not_called`) exist under `crates/ark-ext-proto/tests/` or the host-dispatch crate's `tests/`, each driven by the R1 stub.
- [ ] The "not advertised" case asserts zero calls via BOTH the stub's call-log AND a byte-counting transport wrapper (or equivalent) — the host MUST NOT speculatively send the request.
- [ ] The "advertised but unimplemented" case asserts the host's reconcile result is `Ok` (session survives) AND a WARN line containing both the method name and the capability flag name appears in the captured log output.
- [ ] The fourth case either runs green against a synthetic capability-removal fixture OR is explicitly marked `#[ignore]` with a TODO comment citing decision #4c pending a first real removal.

**Dependencies:** R1, `host-dispatch` sub-kit (for the dispatcher behavior under test).

### R5: View-type compile-error goldens

**Description:** A `trybuild`-based golden suite verifies that
scene-compile rejections produce error messages that point at the
offending KDL location (file + line + column when available) and
reference the view-type surface from the `ark-view` sub-kit.

Minimum case set — four compile-fail, two compile-pass:

**Compile-fail:**
1. `undeclared_view_type.rs` — KDL declares `pane(MyView)` but
   no extension declares `MyView` in its manifest.
2. `view_type_mismatch_on_handle_attr.rs` — an ext handler expects
   `Pane<EditorView>` but scene passes `Pane<TerminalView>`.
3. `stack_child_under_non_stack_parent.rs` — KDL nests a
   `spawn_into @parent` child but `@parent` resolves to `Pane<V>`,
   not `Stack<V>`.
4. `handle_typed_attr_takes_non_handle.rs` — an attribute declared
   in a manifest as `Pane<V>`-typed receives a string literal or
   other non-handle value.

**Compile-pass:**
1. `valid_pane_and_stack_decls.rs` — declares a `Pane<V>` and a
   `Stack<V>` with a child, both view types declared in the
   accompanying manifest.
2. `cross_ext_view_reference.rs` — scene in ext A references a
   view type declared by ext B, both loaded.

**Acceptance Criteria:**
- [ ] `cargo test -p <scene-compile-crate> --test trybuild_goldens` (or equivalent) runs the full suite; compile-fail cases produce `.stderr` goldens under `tests/ui/` (or trybuild's conventional path).
- [ ] Each compile-fail `.stderr` golden contains the KDL source path AND a line / column pointer — validated by a grep assertion on the golden file content (`rg -n "\.kdl:\d+:\d+" crates/<scene>/tests/ui/*.stderr` finds at least one hit per file).
- [ ] `TRYBUILD=overwrite cargo test -p <scene-compile-crate> --test trybuild_goldens` re-generates goldens cleanly, documented in the crate README or a comment at the top of the test entry-point.
- [ ] Each error message names the offending view-type / handle-kind in plain English (verified by a grep on the golden for the relevant type name).
- [ ] The two compile-pass cases actually compile (trybuild `pass` entries) and are NOT auto-generated from the fail cases — they live as distinct files.

**Dependencies:** `ark-view` sub-kit (defines view types + handle kinds), scene-compile crate (defines the diagnostic emission).

### R6: Intent-registration integration tests

**Description:** Per decision #2, the manifest is the SOLE source of
truth for intent registration in v0.1. Tests verify the full path:
manifest declaration → loader builds RPC shim → `IntentRegistry.names()`
contains the intent → scene op `X args` resolves and dispatches over
the wire → stub observes the dispatch.

**Acceptance Criteria:**
- [ ] A test `manifest_intent_appears_in_registry` constructs an R1 stub whose manifest declares intent `stub.hello`, loads it into a test supervisor, and asserts `registry.names()` contains `"stub.hello"`.
- [ ] A test `scene_op_dispatches_to_manifest_intent` compiles a scene containing `stub.hello "world"`, triggers the op, and observes the stub's call-log recording an `intent/dispatch { name: "stub.hello", args: "\"world\"" }` invocation.
- [ ] A test `intent_register_rpc_method_is_gone` asserts (via `cargo doc` inspection OR a compile-fail trybuild case OR a direct `rg` check) that `ArkExtension` no longer exposes an `intent_register` method — decision #2 deletes it.
- [ ] A test `undeclared_intent_scene_op_rejected_at_compile` uses the R5 trybuild infrastructure to confirm that a scene referencing an intent NOT in any loaded manifest produces a compile error pointing at the offending KDL line.

**Dependencies:** R1, R5, `ext-registrations` sub-kit (defines manifest shape), decision #2.

### R7: User-close suppression + handle-invalidation tests

**Description:** Tests verify the session-scoped suppression behavior
locked by decision #3d and the lazy `HandleGone` surface locked by
decision #3c.

Three suppression cases plus one invalidation case:

(a) User closes a scene-declared pane → supervisor records
`(handle_name, params_hash)` in `closed_by_user` → broadcasts
`ark.handle.invalidated { cause: "user_closed" }` as an `ExtEvent` →
subsequent `pane/emit` against the gone handle returns `HandleGone`.

(b) Reconcile runs with the scene unchanged (same params hash for that
pane) → supervisor SKIPS the spawn for that pane. Observable: no new
handle issued, stub sees no `Pane<V>` with the expected name.

(c) Reconcile runs after the scene author edits the view's params (new
hash) → supervisor EVICTS the entry from `closed_by_user` → respawns
the pane → suppression is lifted. Observable: new handle issued, stub
receives a fresh `Pane<V>` instance.

(d) Any `ark.handle.invalidated` broadcast (any cause) → next op
against that handle returns `HandleGone` error WITHOUT attempting the
RPC (lazy path; belt-and-suspenders per decision #3c).

**Acceptance Criteria:**
- [ ] A test `user_close_records_suppression_and_emits_invalidated` uses a fake zellij pane-list delta (or supervisor test hook) to simulate user-close, asserts both the in-memory `closed_by_user` entry AND the broadcast of `ark.handle.invalidated { cause: "user_closed", handle: ... }` as an `ExtEvent` observable on the bus.
- [ ] A test `reconcile_same_params_skips_spawn_after_user_close` drives reconcile twice with the same scene, interleaves a user-close between, and asserts zero new handles issued on the second reconcile for the closed pane.
- [ ] A test `reconcile_new_params_respawns_after_user_close` drives reconcile, user-closes, mutates the scene's params for that pane, reconciles again, asserts (i) entry removed from `closed_by_user`, (ii) new handle issued, (iii) stub sees the fresh `Pane<V>`.
- [ ] A test `pane_op_after_invalidation_returns_handle_gone` constructs a scenario where the stub retains a stale `Pane<V>` reference after a broadcast `ark.handle.invalidated`, calls `pane/emit`, and asserts `ExtensionError::HandleGone(_)` (or the equivalent named variant from `ark-view`).
- [ ] The suppression tests DO NOT persist `closed_by_user` across supervisor restart — a `supervisor_restart_clears_suppression` test exercises this (per decision #3d's in-memory-only lifetime).
- [ ] Stack-children are explicitly NOT subject to suppression: a test `stack_child_user_close_does_not_suppress_respawn` confirms a user-closed stack child is gone-forever-for-that-instance but a subsequent `stack/spawn_pane` call creates a new child unimpeded.

**Dependencies:** R1, `ark-view` sub-kit (defines invalidation protocol + `HandleGone` error), `host-dispatch` sub-kit (defines reconcile + pane-list delta path).

### R8: CI integration

**Description:** All Phase 2 test suites MUST run under the existing
workspace-wide test harness — `cargo test --workspace --tests` — with
no extra flags required for normal runs. Trybuild goldens are
bless-gated via the standard `TRYBUILD=overwrite` env var (documented
inline at the test entry-point). New test crates and `[[test]]` targets
are wired into the workspace `Cargo.toml`.

**Acceptance Criteria:**
- [ ] `cargo test --workspace --tests` (no features, no env vars) runs every R1-R7 test to green on a clean checkout.
- [ ] The R1 stub crate AND the R2 subprocess binary are listed in the root `Cargo.toml` workspace `members` array. `rg -n "ark-ext-test-support" Cargo.toml` (or the final crate name from Open Item #1) finds a hit.
- [ ] The R5 trybuild entry-point carries a comment documenting `TRYBUILD=overwrite cargo test -p <crate> --test trybuild_goldens` as the blessing command.
- [ ] Running the full suite with `TRYBUILD=overwrite` does not change any committed `.stderr` golden (i.e. goldens are already at their blessed state on main).
- [ ] No Phase 2 test depends on a live zellij session, real filesystem watchers outside `tempfile::tempdir`, or network access — verified by running the suite inside a sandbox that denies those (existing CI constraint) with zero failures.

**Dependencies:** R1-R7.

## Out of Scope

- Defining the `ArkExtension` trait method set — owned by the
  `ext-surface` sub-kit. Tests here reference whatever that sub-kit
  settles on.
- Defining host-side reconcile / dispatcher behavior — owned by the
  `host-dispatch` sub-kit. These tests EXERCISE the dispatcher's locked
  behavior but do not specify it.
- Defining the view-type algebra (`Pane<V>`, `Stack<V>`, marker traits,
  `HandleGone`, invalidation event schema) — owned by the `ark-view`
  sub-kit.
- Defining the manifest schema extensions for view types / reload gates
  / config — owned by the `ext-registrations` sub-kit; tests here
  assume that schema is in place.
- Performance / load tests. Phase 2 lands behavior; Phase 3+ can add
  throughput suites if needed.
- Replacing the existing `crates/ark-ext-proto/tests/conformance/`
  suite. Phase 2 EXTENDS that file's shape (same `both_transports!`
  pattern, same `ExtensionClient` abstraction) rather than forking it.

## Cross-References

- Parent spec: `cavekit-soul.md` (Phase 2, lines 581-590).
- Format reference: `cavekit-soul-phase-1-types.md`.
- Decisions: `context/plans/phase-2-design-decisions.md` (#2, #3c, #3d, #4a, #4b, #4c).
- Existing test shape being extended: `crates/ark-ext-proto/tests/conformance.rs` + `crates/ark-ext-proto/tests/conformance/suite.rs`.
- Sibling sub-kits (not yet written at time of drafting):
  `cavekit-soul-phase-2-ext-surface.md`,
  `cavekit-soul-phase-2-host-dispatch.md`,
  `cavekit-soul-phase-2-ark-view.md`.

## Open Items

1. **Stub crate name.** R1 proposes `crates/ark-ext-test-support`; the
   decisions doc does not settle this. Sibling sub-kits (in particular
   the `ext-surface` sub-kit, which may need a companion test-only
   crate of its own) should converge on a single name before build-site
   decomposition.
2. **Params-hash algorithm.** Decision #3d keys suppression on a
   `ParamsHash` but does not specify the hash input or algorithm.
   R7 cases (b) and (c) depend on whatever the `ark-view` / `host-dispatch`
   sub-kits settle — tests MUST use the same hash function the
   supervisor uses, not reimplement it.
3. **Capability-gate WARN format.** Decision #4a mandates a log line on
   the advertised-but-unimplemented case; the exact format (fields,
   structured vs unstructured, `tracing` target) is left to the
   `host-dispatch` sub-kit. R4's grep assertions should be tightened
   once that format is locked.
4. **Handle name→handle lookup API.** Decisions doc Open Item #3
   flags this as unresolved. R7(c) ("reconcile with new params
   respawns") implicitly requires such a lookup; tests may need a
   follow-up pass once `ark-view` settles the API.

## Changelog

(empty)
