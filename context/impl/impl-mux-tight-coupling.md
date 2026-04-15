---
created: "2026-04-15"
last_edited: "2026-04-15"
domain: mux-tight-coupling
---

# Implementation Tracking — Mux Tight-Coupling Revision

## Status

**Complete** (Wave A kit edits + Wave B code alignment). Build site
`context/plans/build-site-mux-tight-coupling.md` lands M-1 through M-13.

## Decision rationale

The v1 `Multiplexer` trait was introduced with a stated "portability in
principle" goal (cavekit-architecture.md R4, pre-revision text). In practice
that portability was theatrical:

- The picker + status plugins compile to `wasm32-wasi` and run inside
  zellij's `wasmtime` plugin host via `zellij-tile`. Tmux has no wasm host;
  a `TmuxMux` would have to rewrite every plugin from scratch.
- The host never links zellij crates. Integration is CLI shell-out + `zellij
  pipe` + wasm layout declarations. There was no rich API to abstract over.
- `TmuxMux` appeared only in deferred-scope lists. Nothing was planned.

The trait cost real coupling surface: `async_trait` indirection, `Box<dyn
Multiplexer>` / `Arc<dyn Multiplexer>` dispatch, three duplicate `MockMux` /
`StubMux` / `NoopMux` implementations alongside the existing `StubExecutor`,
a `mux_contract_suite` kept in sync with the trait, and a `dyn`-safety
constraint that would block upcoming zellij-native capability additions
(floating panes, swap layouts, typed pipe source metadata).

An intermediate proposal to split `Multiplexer` into narrow consumer-facing
traits (`TabOps`, `PluginPipe`, `StatusChannel`) for test mockability was
**rejected**: a single-impl trait existing only for tests is a Rust
anti-pattern that leaks `dyn`/generic noise into every call site, lies
about extensibility, and is dominated by two alternatives — functional-core
factoring (deferred) or a concrete type with a stubbed command executor at
the subprocess boundary (what this revision adopted).

## Research citations

The "no replacement trait" decision was grounded in:

- matklad, **Concrete Abstraction**
  (<https://matklad.github.io/2024/09/06/fantastic-learning-resources.html>
  and related posts). Concrete types first; introduce traits only when a
  second implementation exists or an external caller forces the seam.
- **faux** README (<https://github.com/nrxus/faux>). Even in crates whose
  reason for existing is mocking, the maintainers acknowledge the
  community-consensus view that single-impl traits exist for test
  mockability is an anti-pattern — `faux` is the "if you must" escape
  hatch, not the recommended path.
- **quinn**, **gitoxide**, **nextest** codebases. All three exemplify
  the sans-IO / concrete-type pattern: pure logic returns data, I/O is
  stubbed at the subprocess / filesystem boundary, no dyn dispatch for
  test seams.
- **zero2prod** (Luca Palmieri, 2022). Chapter on testability explicitly
  favours concrete types with injectable I/O adapters over trait-per-port
  ports-and-adapters.
- HN / Reddit threads on "should I extract a trait" where the consensus
  response for Rust (unlike, say, Java) is "not until the second impl".

## What was deleted

**Files removed** (from `git status -D`):

- `crates/core/src/multiplexer.rs` — `Multiplexer` trait + inline
  `MockMux` test helper.
- `crates/core/src/mux_contract.rs` — `MuxHarness` / `RecordedCall` /
  `mux_contract_suite`.
- `crates/mux/zellij/tests/contract.rs` — the downstream contract-suite
  driver (consumed the deleted suite).

**Structs deleted inline** in files that were kept:

- `NoopMux` in `crates/core/src/orchestrator.rs` (tests).
- `MockMux` in `crates/core/src/orchestrator_contract.rs` (was
  `pub` for downstream reuse; replaced by `ZellijMux::for_test`).
- `MockMux` in the old `crates/core/src/consumers/status_pipe.rs`
  (file relocated to supervisor in M-9).
- `StubMux` in `crates/supervisor/src/{orchestration,kill,auto_close}.rs`
  (tests).
- `StubMux` in `crates/orchestrators/{cavekit,claude-code}/src/lib.rs`
  and `crates/orchestrators/cavekit/src/watchers/review_tab.rs`
  (tests).

**Re-exports removed** from `crates/core/src/lib.rs`:

- `pub use consumers::{..., status_pipe}` → now just `{hook_dispatcher,
  state_writer}`.
- The `Multiplexer` / `MuxHarness` / `RecordedCall` / `mux_contract_suite`
  / `MockMux` public names (none of these existed as separate files by
  the time this pass ran — the files had already been deleted in Wave
  A's pre-work; Wave B reconciled the import sites).

## What replaced them

- **`ZellijMux` inherent methods.** `mux.rs` now has `impl ZellijMux {
  pub async fn ensure_session / create_tab / close_tab / rename_tab /
  pipe / kind }`. Same signatures as the old trait; `async_trait`
  attribute dropped (inherent async fns need no attribute). Callers
  that were `Arc<dyn Multiplexer>` / `&dyn Multiplexer` are now
  `Arc<ZellijMux>` / `&ZellijMux` throughout.

- **`ZellijMux::for_test(scripted: Vec<CommandOutput>) -> (Self,
  Arc<StubExecutor>)`** — the canonical downstream test seam. Gated
  behind a `test-support` cargo feature (default off). A sibling
  `ZellijMux::for_test_in_zellij(...)` forces the `in_zellij = true`
  branch for tests whose scenarios depend on the inside-zellij spawn
  path (executor-driven `zellij action switch-session`) rather than
  the outside-zellij pty path (which would require a real zellij
  binary).

- **`StubExecutor` as canonical test seam.** Tests assert on the
  `(program, argv)` tuples recorded by the shared `Arc<StubExecutor>`
  returned from `for_test`. Downstream tests inspect the argv for
  `switch-session`, `new-tab`, `close-tab-at-index`, `pipe --name
  ark-status`, etc. This replaces the prior "did mock record this
  method name" assertions with real zellij-cli conformance checks.

## Non-obvious refactors

### Dependency inversion: `ark-core` now depends on `ark-mux-zellij`

Pre-revision direction: `ark-mux-zellij` → `ark-core` (for the
`Multiplexer` trait). After the trait was removed, `ark-mux-zellij` had
zero remaining refs to `ark-core` (verified via `Grep`). The
`ark-core -> ark-mux-zellij` flip was therefore safe and non-circular.

The resulting graph:

```
ark-core -> ark-mux-zellij -> ark-types
         -> ark-types
         -> ark-config

ark-supervisor -> ark-core
               -> ark-mux-zellij   (already)
               -> ark-orchestrators-*

ark-orchestrators-* -> ark-core
                    -> ark-mux-zellij   (added in this pass for tests;
                                         also needed at runtime now
                                         that World.mux: Arc<ZellijMux>
                                         uses a concrete type)
```

This keeps `World` in `ark-core` unchanged — it now holds
`Arc<ZellijMux>` concrete. Orchestrators consume the concrete type
directly; no trait seam needed.

### `ark-mux-zellij/test-support` feature enabled unconditionally in `ark-core`

`orchestrator_contract_suite` is `pub` surface in `ark-core` — downstream
orchestrator crates call it from their tests. Its implementation
constructs `ZellijMux::for_test_in_zellij(...)`, so the constructor must
be reachable from `ark-core` library code (not just its own tests).
`ark-core`'s main dep on `ark-mux-zellij` therefore enables
`features = ["test-support"]` unconditionally. The cost is a few hundred
bytes of constructor code in release builds — trivially acceptable. The
`ark-mux-zellij` feature itself remains default-off so other downstream
consumers opt in via their own `[dev-dependencies]` entries.

### `status_pipe` relocated to `ark-supervisor::consumers`

Previously `ark_core::consumers::status_pipe`. The consumer closed over
`mux.pipe` and `mux.rename_tab`, which required the caller's `Arc<Mux>`.
With the trait gone, holding `Arc<ZellijMux>` in ark-core would have
required the dep-direction flip described above **before** the trait was
removed — which was circular. Moving the consumer to supervisor (which
already depends on both crates) side-steps the sequencing problem and
removes a spurious mux dependency from `ark-core`'s public surface.

Supervisor crate now has `crates/supervisor/src/consumers/{mod.rs,
status_pipe.rs}`. The `event_kind_slug` helper is duplicated between
`ark_core::consumers` (for `hook_dispatcher`) and
`ark_supervisor::consumers` (for `status_pipe`) rather than promoted to
`pub` — the two modules are conceptually separate and the duplication is
< 20 lines.

### Test migration caveats where `ZellijMux` is strictly less failable

`ZellijMux::pipe` is fire-and-forget by spec (R4): non-zero exit and
io::Error both downgrade to `warn!` and return `Ok(())`. Similarly
`ZellijMux::close_tab` swallows failures into `Ok(())` for idempotency
(R3). That means two prior test-only branches become unreachable at the
mux API level:

- `status_pipe`'s "both pipes failed → rename_tab fallback" branch.
- `watch_phase_and_review`'s "close_tab failed → keep handle, retry on
  next transition" branch (F-423).
- `apply_auto_close_policy`'s "one close failed, continue with others,
  suppress TabClosed for the failed one" branch.

Rather than reintroduce a test-only failure surface on `ZellijMux`
(which would smuggle a trait-equivalent in the back door), the relevant
tests were:

- **`status_pipe` fallback tests**: kept the dead code path (defensive
  guard against a hypothetical future mux whose `pipe` returns `Err`);
  ignored the tests with `#[ignore = "ZellijMux::pipe never returns
  Err; reinstate under MuxOp follow-up"]`.
- **`watch_phase_and_review::close_tab_failure_retries_...`**: ignored
  with a pointer to the `MuxOp` follow-up. The F-423 invariant is
  enforced in source (see the match arm around line 166 of
  `review_tab.rs`).
- **`apply_auto_close_policy` "one close fails" test**: rewritten to
  assert the adjacent positive invariant ("every tab receives a close
  attempt"). The failure-side is documented in the doc-comment above
  the replacement test.

These are tracked under the deferred **MuxOp functional-core
factoring** follow-up below.

### `World` stayed in `ark-core`

Considered moving it to a new `ark-world` crate or to `ark-supervisor`
per the build-site plan's Option (B). Both would have introduced new
dep-direction friction:

- `ark-supervisor` already depends on `ark-orchestrators-*`, so moving
  `World` there would force orchestrators to depend on supervisor
  (circular).
- A new `ark-world` crate would need to host both `World` **and** the
  `Orchestrator` trait (since `Orchestrator::run` takes `World`), and
  `ark-core::Config` is part of `World`, so `ark-world` would depend on
  `ark-core`. Consumers (supervisor + orchestrators) would then depend
  on both. Workable, but adds one crate boundary for no win.

The flip to **have `ark-core` depend on `ark-mux-zellij`** was simpler
and safe (verified non-circular once the trait was removed). `World`
stays put; `World.mux: Arc<ZellijMux>` is concrete.

## What was deferred, with exit criteria

### MuxOp functional-core factoring

**Trigger**: the next orchestrator kit revision that needs richer
test ergonomics, OR a fourth test that becomes awkward under the
"assert on recorded argv" pattern. Per the build-site plan, each
orchestrator kit revision introduces its own pure-function layer — this
revision only removed the trait and migrated tests to concrete
`ZellijMux(StubExecutor)`.

**Exit criteria**: when triggered, introduce a `MuxOp` enum
(`MuxOp::CreateTab { session, name, layout } | MuxOp::CloseTab { handle
} | MuxOp::Pipe { target, payload } | ...`) and an applier that runs
on concrete `ZellijMux`. Pure-function orchestrator logic returns
`Vec<MuxOp>`; tests assert on the emitted op log. Re-enables the three
`#[ignore]`'d tests listed above by letting them assert on the op log
rather than on (unreachable) mux error paths.

### Zellij-native capability additions

`open_floating(session, layout_path)`, `swap_layout(handle, layout_path)`,
`pipe_with_source(source_pane, target_name, payload)`, `set_pane_title`,
plugin-permission declarations.

**Trigger**: a plugin kit that needs them (e.g. the picker gaining
floating-pane UX, or custom-pane plugins demanding typed pipe source
metadata).

**Exit criteria**: each addition lands as new `pub async fn` on
`ZellijMux` with its own stubbed-argv unit tests in `mux.rs::tests`.

### MCP plugin gateway / `PluginTool` trait

Orthogonal design, pending. **Trigger**: first MCP-exposed plugin or
tool. **Exit criteria**: gateway spec kit + `schemars`-derived schemas.

### Surface enum + plugin registry

Sits on top of concrete `ZellijMux`. **Trigger**: needs MCP gateway
decision first.

## Follow-ups for other revisions

- **`MUX_V1` constant in `ark-types/src/scope.rs`** now effectively
  has a single meaningful entry (`"zellij"`). The slug-validation
  surface in `crates/supervisor/src/factory.rs::build_multiplexer` is
  preserved for error-message consistency but is architecturally
  vestigial. A future revision may fold it into the hard-coded
  match arm. Flagged here — **do not** change in this revision.

- `crates/supervisor/src/lib.rs:23` has `#![cfg(unix)]` at crate level,
  which propagates to the new `consumers/` module automatically.

## Known caveats

Ignored tests (all with pointer-comments):

- `supervisor::consumers::status_pipe::tests::fallback_rename_when_both_pipes_fail`
- `supervisor::consumers::status_pipe::tests::asymmetric_pipe_failure_no_fallback`
- `supervisor::consumers::status_pipe::tests::both_pipes_fail_no_handle_skips_rename`
- `orchestrators_cavekit::watchers::review_tab::tests::close_tab_failure_retries_without_emitting_tabclosed`

All four trace to `ZellijMux`'s fire-and-forget / idempotent-close
contracts making their original failure paths unreachable. Production
code paths they were asserting are still in source; re-enabling them is
tied to the `MuxOp` follow-up.

The `apply_auto_close_policy` "one-fails-continue" test was rewritten
into `every_tab_receives_a_close_attempt` (same adjacent invariant, no
ignore needed).

## Verification

- `cargo check --workspace` — clean (3.46s).
- `cargo test --workspace` — all pass (291 tests in the supervisor
  binary, 83 in mux/zellij, 67 in ark-core, etc.; 4 ignored as
  documented above).
- `cargo clippy --workspace --all-targets -- -D warnings` — does not
  pass, but the failing lints are all pre-existing (in files this pass
  did not touch or in code regions this pass did not modify). The
  specific warnings in files this pass edited (e.g. `review_tab.rs`'s
  `too_many_arguments` on `watch_phase_and_review`, line 59/85) existed
  before the change — the signature only substituted
  `Arc<dyn Multiplexer>` → `Arc<ZellijMux>` with the same 8 parameters.
  Baseline-matching, per the plan's "unchanged from baseline"
  criterion.

## Files touched (summary for `git status`)

- **Deleted** (pre-existing as deleted entries in git status):
  `crates/core/src/multiplexer.rs`, `crates/core/src/mux_contract.rs`,
  `crates/core/src/consumers/status_pipe.rs`,
  `crates/mux/zellij/tests/contract.rs`.
- **Created**: `crates/supervisor/src/consumers/mod.rs`,
  `crates/supervisor/src/consumers/status_pipe.rs`,
  `context/impl/impl-mux-tight-coupling.md` (this file).
- **Modified**: `crates/mux/zellij/{Cargo.toml, src/{lib.rs, mux.rs}}`,
  `crates/core/{Cargo.toml, src/{lib.rs, orchestrator.rs,
  orchestrator_contract.rs, consumers/mod.rs}}`,
  `crates/supervisor/{Cargo.toml, src/{lib.rs, orchestration.rs,
  factory.rs, kill.rs, auto_close.rs}}`,
  `crates/orchestrators/cavekit/{Cargo.toml, src/{lib.rs,
  watchers/review_tab.rs}}`,
  `crates/orchestrators/claude-code/{Cargo.toml, src/lib.rs}`.
