---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Testing Strategy

## Scope
Test layers for ark. Covers: trait contract tests (every Engine / Orchestrator / Multiplexer impl shares a common suite), per-crate unit tests, integration tests for state dir + hooks, e2e tests against real zellij + delta, wasm plugin tests.

## Requirements

### R1: Contract tests per trait
**Description:** Every trait impl (Engine, Orchestrator, Multiplexer) passes the same behavioral suite.
**Acceptance Criteria:**
- [ ] Contract suites live in the respective trait's crate under `tests/contract.rs`
- [ ] Each suite takes a factory closure `impl Fn() -> Box<dyn Trait>` and runs a fixed scenario set
- [ ] Engine contract fires fake hook payloads, asserts events emitted match expected timeline
- [ ] Orchestrator contract runs with a mock Mux + fixture cwd, asserts orchestrator-level events + tab-graph calls
- [ ] Multiplexer contract uses a stub executor that records command sequences, asserts create/close/pipe/rename patterns
- [ ] Every new trait impl must pass the suite before being merged
- [ ] Suite runs in CI as part of `cargo test`
**Dependencies:** cavekit-architecture

### R2: Fixtures
**Description:** Reproducible test data.
**Acceptance Criteria:**
- [ ] `tests/fixtures/cavekit-project/` — minimal cwd satisfying cavekit detect (sites, impl, ralph-loop)
- [ ] `tests/fixtures/claude-transcripts/` — golden JSONL session transcripts for engine tests
- [ ] `tests/fixtures/hook-payloads/` — example claude hook JSON for each supported event
- [ ] Fixtures are small, committed, documented in a README
- [ ] Helper crate `ark-test-fixtures` re-exports path constants
**Dependencies:** cavekit-engine-claude-code, cavekit-orchestrator-cavekit

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
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install dependencies (macOS)
        if: runner.os == 'macOS'
        run: brew install zellij git-delta
      - name: Install dependencies (Linux)
        if: runner.os == 'Linux'
        run: |
          cargo install zellij
          cargo install git-delta
      - run: cargo test --all-features
      - run: ARK_E2E=1 cargo test --all-features -- --test-threads=1
```

## Out of Scope
- Property-based tests beyond serde round-tripping — nice-to-have, not required
- Mutation testing / code coverage quality gates — v2
- Fuzzing hook payloads — v2
- Load / stress tests (many agents concurrent) — not v1 priority

## Cross-References
- cavekit-architecture.md — trait definitions
- all other kits — each references its contract test subset
