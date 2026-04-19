---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-15T00:00:00Z"
---

# Spec: Testing Strategy

## Scope
Test layers for ark. Covers: trait contract tests (every Engine / Orchestrator impl shares a common suite), per-crate unit tests (including `ark-mux-zellij` stub-executor assertions), integration tests for state dir + hooks, e2e tests against real zellij + delta, wasm plugin tests.

## Requirements

### R1: Contract tests per trait — and when NOT to introduce a trait
**Description:** Every trait with multiple real impls (Engine, Orchestrator) passes a common behavioral suite. `ZellijMux` is a concrete type (no mux trait) and is covered by unit tests in R3 rather than a cross-impl contract suite.

**Rule of trait introduction** (applies to all ark crates): a trait exists because either (a) a second production impl exists or is concretely planned in a kit, or (b) a downstream caller outside ark's control needs to swap the impl. Tests are not a justification — test-only traits with a single production impl are explicitly rejected. This matches matklad's "Concrete Abstraction" guidance and the sans-IO pattern practiced by `quinn`, `gitoxide`, `cargo-nextest`, and `zero2prod`. When the current consumer seems to "want a trait for tests," prefer in order: (1) factor a pure function returning data (e.g. a `MuxOp` enum the caller applies), (2) use a stubbed command executor at the subprocess boundary, (3) relocate the consumer to a crate where the concrete type is reachable.

**Acceptance Criteria:**
- [ ] Engine contract suite lives in `ark-core/engine_contract.rs` with a factory closure `impl Fn() -> Box<dyn Engine>`; fires fake hook payloads; asserts emitted event timelines
- [ ] Orchestrator contract suite lives in `ark-core/orchestrator_contract.rs` with a factory closure; runs against a fixture cwd + `StubExecutor`-backed `ZellijMux`; asserts orchestrator-level events + recorded zellij CLI argv
- [ ] No `TabOps`, `PluginPipe`, or other single-impl mux-facing trait exists in the workspace. Grep rule: `rg 'trait (Mux|TabOps|TabGraph|PluginPipe|StatusChannel|PipeSender)'` returns no matches. (The core `Multiplexer` trait was deleted in the 2026-04-18 mux tight-coupling pass; `ZellijMux` is now the concrete type consumers hold.)
- [ ] Every new trait impl of Engine or Orchestrator passes the relevant suite before merge
- [ ] Suites run in CI as part of `cargo test --workspace`
**Dependencies:** cavekit-soul (supersedes cavekit-architecture; Phase 5 deletes Engine/Orchestrator traits — R1's contract suites retarget to per-extension tests), cavekit-overview (principle 9)

### R2: Fixtures
**Description:** Reproducible test data.
**Acceptance Criteria:**
- [ ] `tests/fixtures/cavekit-project/` — minimal cwd satisfying cavekit detect (sites, impl, ralph-loop)
- [ ] `tests/fixtures/claude-transcripts/` — golden JSONL session transcripts for engine tests
- [ ] `tests/fixtures/hook-payloads/` — example claude hook JSON for each supported event
- [ ] Fixtures are small, committed, documented in a README
- [ ] Helper crate `ark-test-fixtures` re-exports path constants
**Dependencies:** cavekit-claude-code.md (R13 — mock-claude fixture; supersedes deleted cavekit-engine-claude-code.md). Cavekit orchestrator kit deleted; its fixture needs either drop or rehome in a follow-up pass.

### R3: Unit tests per crate
**Description:** Ordinary `#[test]` functions covering module-level logic.
**Acceptance Criteria:**
- [ ] `ark-types`: AgentId generation, serde round-trips for every AgentEvent variant
- [ ] `ark-core` / state: state dir creation, atomic status.json writes, events.jsonl append/read, crash recovery helpers
- [ ] `ark-config`: figment layering tests (defaults → user → project → env → flag), hook matching
- [ ] `ark-engines-claude-code`: settings.local.json injection (idempotent, deep-merge, backup/restore)
- [ ] `ark-orchestrators-cavekit`: impl-tracking parser, ralph-loop parser, findings parser
- [ ] `ark-mux-zellij`: layout templating, command argument construction (no real zellij calls)
- [ ] `ark-pane`: pane command arg parsing, rendering helpers
- [ ] `ark-plugin-*`: fuzzy search, chip rendering width logic
- [ ] Coverage target: 70%+ on core crates (`ark-types`, `ark-core`, `ark-engines-claude-code`)
**Dependencies:** all domains

### R4: End-to-end tests
**Description:** Exercise ark against real zellij + real delta in CI.
**Acceptance Criteria:**
- [ ] CI matrix: macOS (arm64, x64), Linux (x64, arm64)
- [ ] E2E tests in `tests/e2e/` using a real zellij binary + `Command` spawning
- [ ] Scenarios:
  - `spawn → list` — verify agent appears with expected phase
  - `spawn → kill` — verify tab closes and status.json = killed
  - `spawn → stall → detect` — mock a long-silent session
  - `spawn → done` — fake a claude Stop hook, verify Done emitted and auto-close
  - `crashed supervisor` — simulate OOM, verify `ark list` shows Crashed and `ark doctor --fix` archives
- [ ] E2E uses a mock `claude` shim binary that emits scripted hook events on demand
- [ ] Gated on `ARK_E2E=1`; default cargo test skips these
- [ ] All e2e tests clean up state dirs + zellij sessions on teardown (even on failure)
**Dependencies:** cavekit-cli, cavekit-supervisor, cavekit-mux-zellij

### R5: Plugin tests
**Description:** Wasm plugin tests without a real zellij runtime.
**Acceptance Criteria:**
- [ ] Unit tests in plugin crates use `zellij-tile`'s offline test helpers (if any); otherwise unit-test the pure-Rust rendering + state logic
- [ ] Render tests: given a cache of AgentSummary, assert Text output contains expected chips/ordering
- [ ] Pipe handling tests: feed JSON payloads, assert cache updates + re-render triggered
- [ ] Fuzzy search tests: given known agents + query string, assert ranked order
- [ ] Picker control protocol tests: serialize/deserialize requests+responses
**Dependencies:** cavekit-plugin-status, cavekit-plugin-picker

## CI configuration
```yaml
name: CI
on: [push, pull_request]
jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    env:
      ZELLIJ_VERSION: "0.44.1"
      DELTA_VERSION: "0.18.2"
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install zellij (prebuilt tarball, ~10s)
        shell: bash
        run: |
          case "${{ runner.os }}-${{ runner.arch }}" in
            Linux-X64)   T=zellij-x86_64-unknown-linux-musl.tar.gz ;;
            Linux-ARM64) T=zellij-aarch64-unknown-linux-musl.tar.gz ;;
            macOS-X64)   T=zellij-x86_64-apple-darwin.tar.gz ;;
            macOS-ARM64) T=zellij-aarch64-apple-darwin.tar.gz ;;
          esac
          curl -fsSL "https://github.com/zellij-org/zellij/releases/download/v${ZELLIJ_VERSION}/${T}" \
            | sudo tar -xz -C /usr/local/bin
          zellij --version
      - name: Install delta (prebuilt)
        shell: bash
        run: |
          case "${{ runner.os }}-${{ runner.arch }}" in
            Linux-X64)   T=git-delta-${DELTA_VERSION}-x86_64-unknown-linux-gnu.tar.gz ;;
            Linux-ARM64) T=git-delta-${DELTA_VERSION}-aarch64-unknown-linux-gnu.tar.gz ;;
            macOS-X64)   T=git-delta-${DELTA_VERSION}-x86_64-apple-darwin.tar.gz ;;
            macOS-ARM64) T=git-delta-${DELTA_VERSION}-aarch64-apple-darwin.tar.gz ;;
          esac
          curl -fsSL "https://github.com/dandavison/delta/releases/download/${DELTA_VERSION}/${T}" \
            | sudo tar -xz -C /tmp
          sudo mv "/tmp/git-delta-${DELTA_VERSION}-"*/delta /usr/local/bin/
          delta --version
      - run: cargo test --all-features
      - run: ARK_E2E=1 cargo test --all-features -- --test-threads=1
```

**Why prebuilt tarballs:** `cargo install zellij` is ~5 min cold; `brew install` is 30-90s cold; tarball is ~10s and same on macOS + Linux. Avoid `apt install zellij` (Ubuntu universe lags badly). No maintained `setup-zellij` action exists.

**Linux musl tarball is statically linked** — runs on any `ubuntu-*` runner regardless of glibc.

## Out of Scope
- Property-based tests beyond serde round-tripping — nice-to-have, not required
- Mutation testing / code coverage quality gates — v2
- Fuzzing hook payloads — v2
- Load / stress tests (many agents concurrent) — not v1 priority

## Cross-References
- cavekit-soul.md — Engine/Orchestrator traits deleted in Phase 5 (R1's trait-contract suites retarget to per-extension tests: cavekit-claude-code R13, cavekit-pi R22)
- all other kits — each references its contract test subset
