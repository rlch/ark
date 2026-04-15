---
created: "2026-04-15"
last_edited: "2026-04-15"
---

# Build Site: Mux Tight-Coupling Revision

## Why this exists

v1 shipped the `Multiplexer` trait in `crates/core/src/multiplexer.rs` with a stated goal (`cavekit-architecture.md` R4) that it be "tmux-compatible in principle" so a future `TmuxMux` could slot in. In practice the portability promise is theatrical:

- Picker + status plugins compile to `wasm32-wasi` and run inside zellij's `wasmtime` plugin host via `zellij-tile`. Tmux has no wasm host; a tmux impl would have to rewrite every plugin from scratch.
- The host never links zellij crates — integration is CLI shell-out + `zellij pipe` + wasm layout declarations. There is no rich API we're portably abstracting over.
- `TmuxMux` appears only in deferred-scope lists (`cavekit-overview.md:99`). Nothing is planned.

The trait costs real coupling surface: `async_trait` indirection, `Box<dyn Multiplexer>` dispatch, `MockMux` duplicated alongside `StubExecutor`, a mux-contract test suite, and a `dyn`-safety constraint that prevents `ZellijMux` from exposing zellij-native capabilities (floating panes, swap layouts, typed pipe source metadata) that upcoming custom-pane plugin work will want.

This site deletes the trait, concretizes `ZellijMux`, and updates four kits + the code to match. Zellij-native capability additions (floating panes etc.) are **deferred** to a follow-up revision tied to a specific plugin kit — do not conflate.

## Design decision: no replacement trait

An intermediate proposal considered splitting `Multiplexer` into narrow consumer-facing traits (`TabOps`, `PluginPipe`, `StatusChannel`) for test mockability. **Rejected** after external research (matklad "Concrete Abstraction," sans-IO practice in quinn/gitoxide, `faux` README, `zero2prod`). Rust community consensus: a single-impl trait that exists only for tests is an anti-pattern — it leaks `dyn`/generic noise into every call site, lies about extensibility, and is dominated by two alternatives:

1. **Functional core / command-bus.** Pure functions produce a `MuxOp` (or `Vec<MuxOp>`) enum; an applier crate applies them to the concrete `ZellijMux`. Unit tests assert on the emitted data; integration tests exercise the applier against `ZellijMux(StubExecutor)`.
2. **Concrete type + stubbed command executor at the subprocess boundary.** What `ark-mux-zellij` already does for its own unit tests. Extend this pattern to downstream consumers.

Plus a dep-direction tool:

3. **Relocate consumers** that previously needed a trait into a crate that already depends on `ark-mux-zellij`. `status_pipe` → supervisor. Eliminates the inverted-dep pressure that tempts trait introduction.

This site adopts (2) + (3) for consumer migration. (1) is **deferred** to follow-up revisions tied to orchestrator kits — introducing `MuxOp` is a bigger refactor than the mux-coupling delete.

## Cavekit traceability

Revisions to approved kits, no new kits. All changes trace back to the design decision above:

- `cavekit-architecture.md` R3, R4, R6 — drop `dyn Multiplexer`; R4 restated as concrete `ZellijMux` + explicit "no narrow injection traits" AC; `TmuxMux` dropped from deferred scope.
- `cavekit-mux-zellij.md` — reframe from "trait impl" to "the mux"; interaction snippet updated.
- `cavekit-testing.md` R1 — new rule-of-trait-introduction language: traits justified by second impl or external caller, never by tests alone. Explicit rejection of `TabOps` / `PluginPipe` / `StatusChannel`.
- `cavekit-overview.md` — principle 9 added ("Concrete over trait-with-one-impl"); v1 scope line updated; `TmuxMux` removed from deferred.

## Scope boundary

**In scope**: trait deletion, spec revisions, `Arc<dyn Multiplexer>` → `Arc<ZellijMux>`, contract-suite removal, `status_pipe` relocation from `ark-core` to `ark-supervisor`, test migration to `ZellijMux(StubExecutor)` constructor.

**Out of scope** (separate revision when needed):
- `MuxOp` enum / functional-core factoring for orchestrator test ergonomics — follow-up tied to each orchestrator kit.
- New zellij-native methods (`open_floating`, `swap_layout`, `pipe_with_source`) — add when a plugin kit demands them.
- MCP plugin gateway / `PluginTool` trait — orthogonal, design pending.
- Any wasm-plugin-side rework.

## Wave A — spec revisions (mechanical kit edits)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| M-1 | `cavekit-architecture.md` R3: change `mux: Arc<dyn Multiplexer>` to `mux: Arc<ZellijMux>` (line 48). Update supervisor-flow sketch line 132 to match. | architecture | R3 | none | S |
| M-2 | `cavekit-architecture.md` R4: rewrite section header from "Multiplexer trait" to "Zellij host integration". Replace AC — drop "trait is `Send + Sync`", drop "Trait is tmux-compatible in principle", drop "contract suite uses a stub executor" (migrates to unit-tests R3 in testing kit). Keep method enumeration, rebind as inherent methods on concrete `ZellijMux`. Note: no new zellij-native methods in this pass — deferred. | architecture | R4 | none | S |
| M-3 | `cavekit-architecture.md` R6 v1 scope: line 82 "Multiplexers: `ZellijMux` only" → "Zellij integration (`ZellijMux`, concrete type, no mux trait)". | architecture | R6 | none | S |
| M-4 | `cavekit-mux-zellij.md`: title (line 6) "Spec: Multiplexer — Zellij" → "Spec: Zellij Integration". Scope (line 9) drop "implementation of the `Multiplexer` trait"; reword as "`ZellijMux` — ark's concrete integration with zellij." Interaction snippet lines 83–89 `Arc<dyn Multiplexer>` → `Arc<ZellijMux>`. Cross-ref line 99 from "Multiplexer trait definition (R4)" to "ZellijMux ownership + World (R3/R4)". | mux-zellij | (scope + all R) | none | S |
| M-5 | `cavekit-testing.md` R1: line 9 scope drop "Multiplexer" from trait-contract list; line 14 remove "Multiplexer" from trait enumeration; delete line 20 AC "Multiplexer contract uses a stub executor...". Unit-test coverage is already mandated at line 43 under R3. | testing | R1 | none | S |
| M-6 | `cavekit-overview.md`: line 90 v1 scope "1 mux: `ZellijMux`" → "Zellij integration (`ZellijMux`)". Line 99 deferred list: delete `TmuxMux` entry entirely (no longer deferred — removed from roadmap). Domain table (line 35) requirement count stays at 6 for this pass; bump only when R7+ capabilities land in a future revision. | overview | (index) | none | S |

## Wave B — code alignment (compiler-guided)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| M-7 | Delete trait + contract modules: `crates/core/src/multiplexer.rs` (whole file incl. inline `MockMux` + trait-dispatch tests), `crates/core/src/mux_contract.rs` (whole file). Remove `pub mod` + re-exports from `crates/core/src/lib.rs` (`Multiplexer`, `MuxHarness`, `RecordedCall`, `mux_contract_suite`). | architecture | R4 | M-2 | S |
| M-8 | `crates/mux/zellij/src/mux.rs`: remove `#[async_trait] impl Multiplexer for ZellijMux { ... }` block. Move method bodies verbatim into an inherent `impl ZellijMux { ... }` block — same signatures (async fns in inherent impls need no attribute in modern Rust). Drop `use ark_core::Multiplexer`. Delete `crates/mux/zellij/tests/contract.rs` (driven by deleted `mux_contract_suite`). | mux-zellij | R1–R6 | M-7 | M |
| M-8b | Add a public test-support constructor on `ZellijMux` so downstream test migration stays one-liner-friendly: `pub fn for_test(scripted: Vec<(Vec<&str>, StubResponse)>) -> Self` or equivalent, gated behind `#[cfg(any(test, feature = "test-support"))]` with `test-support` as an opt-in feature so dev-deps can pull it. Document in mux-zellij crate docs. | mux-zellij | R1–R6, testing R1 | M-8 | S |
| M-9 | **Relocate** `crates/core/src/consumers/status_pipe.rs` → `crates/supervisor/src/consumers/status_pipe.rs`. Move its tests alongside. Keep `hook_dispatcher` and `state_writer` in `ark-core` (they don't touch mux). Adjust imports: `status_pipe` now takes `Arc<ZellijMux>` concrete; tests use `ZellijMux::for_test(...)`. Update supervisor lib to re-export the moved consumer where needed. Update `ark-core/lib.rs` `pub use consumers::{hook_dispatcher, state_writer};` (drop `status_pipe`). | architecture | R3, testing R1 | M-7, M-8, M-8b | M |
| M-10 | Rewrite remaining dyn-dispatch call sites: `crates/supervisor/` (orchestration, kill, auto_close, factory), `crates/orchestrators/cavekit` (lib, watchers/review_tab), `crates/orchestrators/claude-code` (lib), `crates/cli/` (if any). `Arc<dyn Multiplexer>` → `Arc<ZellijMux>`. Remove `use ark_core::Multiplexer` imports; swap to `use ark_mux_zellij::ZellijMux`. `World.mux: Arc<ZellijMux>`. | architecture | R3 | M-7, M-8 | M |
| M-11 | Test-side cleanup: delete `MockMux` / `StubMux` / `NoopMux` helpers in `crates/core/src/orchestrator.rs`, `crates/core/src/orchestrator_contract.rs`, and any `crates/orchestrators/**/tests` or `#[cfg(test)]` modules. Replace with `ZellijMux::for_test(...)` per M-8b. Orchestrator contract suite keeps its factory pattern but yields a `ZellijMux` not a `Box<dyn Multiplexer>`. Grep-check: `rg 'MockMux|StubMux|NoopMux|dyn Multiplexer'` clean after this task. | testing | R1, R3 | M-8, M-8b, M-9, M-10 | M |
| M-12 | `cargo check --workspace` clean, `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` unchanged from baseline. Fix residual warnings (unused imports from removed trait). | testing | R1, R3 | M-10, M-11 | S |
| M-13 | Tracking doc: write `context/impl/impl-mux-tight-coupling.md` summarizing the revision — decision rationale (portability was theatrical; trait-for-testing rejected per matklad/sans-IO), research citations, deleted surfaces (trait + contract + MockMux/StubMux/NoopMux), status_pipe relocation + reasoning, `ZellijMux::for_test` constructor, capabilities this unblocks for future revisions (floating panes, swap layouts, typed pipe source), `MuxOp` functional-core follow-up flag. | impl-tracking | n/a (Tier 4) | M-12 | S |

## Deferred follow-ups (new revisions when triggered)

These are **not tasks in this site** — flag them as future work tied to whichever kit motivates them:

- `MuxOp` enum + functional-core factoring for orchestrator logic. Each orchestrator kit revision introduces its own pure-function layer; this site just deletes the trait and migrates tests to concrete `ZellijMux`.
- Zellij-native capability expansion: `open_floating(session, layout_path)`, `swap_layout(handle, layout_path)`, `pipe_with_source(source_pane, target_name, payload)`, `set_pane_title`, plugin-permission declarations. Each pairs with a plugin kit that needs it.
- MCP plugin gateway + `PluginTool` trait with `schemars`-derived schemas — separate design (earlier in this thread).
- `Surface` enum + plugin registry — sits on top of concrete `ZellijMux`; needs MCP gateway decision first.

## Success criteria

- Grep clean: `rg 'trait Multiplexer|dyn Multiplexer|TmuxMux|MockMux|StubMux|NoopMux|TabOps|PluginPipe|StatusChannel'` returns no matches outside historical impl-tracking docs.
- `status_pipe` lives under `crates/supervisor/src/consumers/`; `ark-core` consumers are mux-free.
- `ZellijMux::for_test` exists as the canonical downstream test seam.
- All five revised kits (architecture, mux-zellij, testing, overview, + implicit traceability to supervisor/orchestrator kits) re-read coherently; no dangling "trait" / "portable" / "contract suite" language.
- `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings` all green.
- `ck:scan` run against revised kits reports no drift.
