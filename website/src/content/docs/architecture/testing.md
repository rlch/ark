---
title: "Testing"
description: "Contract tests, fixtures, and e2e"
---

Ark's testing strategy has four layers: trait contract tests, per-crate unit tests, integration tests, and end-to-end tests against real zellij. The unifying principle is **concrete over trait-with-one-impl**: tests are not a justification for introducing a trait.

## The concrete abstraction rule

A trait exists in ark only when either:

1. A second production implementation exists or is concretely planned, or
2. A downstream caller outside ark's control needs to swap the implementation.

Test-only traits with a single production implementation are explicitly rejected. This matches matklad's "Concrete Abstraction" guidance and the sans-IO patterns practiced by `quinn`, `gitoxide`, `cargo-nextest`, and `zero2prod`.

When a test seems to "want a trait for mockability," the preferred alternatives are (in order):

1. **Factor a pure function returning data.** For example, instead of mocking a mux trait, return a `MuxOp` enum that the caller applies. The pure function is trivially testable.
2. **Stub at the subprocess boundary.** `ZellijMux` uses a `StubExecutor` that records zellij CLI commands rather than running them. The stub operates below the mux, not instead of it.
3. **Relocate the consumer** to a crate where the concrete type is reachable.

As a concrete enforcement:

```
rg 'trait (Mux|TabOps|TabGraph|PluginPipe|StatusChannel|PipeSender)'
```

returns no matches in the ark workspace.

## Contract tests

Every trait with multiple real implementations shares a common behavioral suite:

### Engine contract suite

Lives in `ark-core/engine_contract.rs`. Takes a factory closure `impl Fn() -> Box<dyn Engine>`:

```rust
pub fn engine_contract_suite(factory: impl Fn() -> Box<dyn Engine>) {
    test_name_returns_non_empty(&factory);
    test_install_observability_emits_events(&factory);
    test_install_observability_is_idempotent(&factory);
    test_teardown_after_install(&factory);
    test_default_pane_cmd_non_empty(&factory);
    // ...
}
```

The suite fires fake hook payloads into a fixture cwd and asserts the emitted event timeline on the `EventSink`. Every new Engine implementation passes this suite before merge.

### Orchestrator contract suite

Lives in `ark-core/orchestrator_contract.rs`. Runs each orchestrator against a fixture cwd with a `StubExecutor`-backed `ZellijMux`:

```rust
pub fn orchestrator_contract_suite(factory: impl Fn() -> Box<dyn Orchestrator>) {
    test_name_returns_non_empty(&factory);
    test_detect_on_fixture_cwd(&factory);
    test_run_emits_started_and_done(&factory);
    test_run_creates_expected_tabs(&factory);
    test_cancel_honored_within_5s(&factory);
    // ...
}
```

The suite injects fixture events onto the bus and asserts:
- Orchestrator-level events (Progress, PhaseTransition, Iteration)
- Recorded zellij CLI argv from the StubExecutor
- Outcome type on completion

## StubExecutor

The key testing primitive for `ZellijMux`. Instead of introducing a mux trait, `ZellijMux` accepts an executor parameter:

```rust
pub struct StubExecutor {
    commands: Arc<Mutex<Vec<RecordedCommand>>>,
    responses: Arc<Mutex<VecDeque<CommandResponse>>>,
}

pub struct RecordedCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}
```

In production, the executor dispatches to `tokio::process::Command`. In tests, `StubExecutor` records every command and returns pre-configured responses:

```rust
#[tokio::test]
async fn ensure_session_checks_existing_sessions() {
    let stub = StubExecutor::new();
    // Simulate "zellij list-sessions" returning empty
    stub.push_response(CommandResponse::ok(""));

    let mux = ZellijMux::with_executor(stub.clone());
    mux.ensure_session("ark-cavekit-auth").await.unwrap();

    let cmds = stub.recorded_commands();
    assert!(cmds.iter().any(|c| c.args.contains(&"list-sessions".to_string())));
}
```

This approach tests the real `ZellijMux` code -- argument construction, error handling, response parsing -- without running a zellij binary. No mocking framework needed.

## Fixtures

Reproducible test data lives in `tests/fixtures/` and is re-exported by the `ark-test-fixtures` crate:

| Directory | Contents |
|---|---|
| `tests/fixtures/cavekit-project/` | Minimal cwd satisfying CavekitOrchestrator's `detect` (sites/, impl/, ralph-loop/) |
| `tests/fixtures/claude-transcripts/` | Golden JSONL session transcripts for engine tests |
| `tests/fixtures/hook-payloads/` | Example Claude Code hook JSON for each supported event |

Fixtures are small, committed to the repo, and documented with a README. Path constants are available via `ark_test_fixtures::paths::*`.

## Per-crate unit tests

Ordinary `#[test]` and `#[tokio::test]` functions cover module-level logic:

| Crate | Coverage focus |
|---|---|
| `ark-types` | AgentId generation, serde round-trips for every `AgentEvent` variant |
| `ark-core` | StateDir creation, atomic `status.json` writes, `events.jsonl` append/read, crash recovery helpers |
| `ark-config` | Figment layering (defaults -> user -> project -> env -> flag), hook matching |
| `ark-engines-claude-code` | `settings.local.json` injection (idempotent, deep-merge, backup/restore) |
| `ark-orchestrators-cavekit` | impl-tracking parser, ralph-loop parser, findings parser |
| `ark-mux-zellij` | Layout templating, command argument construction (via StubExecutor, no real zellij) |
| `ark-plugin-*` | Fuzzy search ranking, chip rendering width logic |

Coverage target: 70%+ on core crates (`ark-types`, `ark-core`, `ark-engines-claude-code`).

## Integration tests

Integration tests validate cross-crate interactions without a real zellij:

- **State directory round-trips:** create a StateDir, write events, read back `status.json` and `events.jsonl`, verify consistency.
- **Hook payload processing:** feed fixture hook JSON into `ark-hook` logic, verify emitted `AgentEvent` variants.
- **Config layering:** verify that project config overrides user config, env overrides project, flags override env.

## End-to-end tests

E2E tests exercise ark against a real zellij binary and real delta in CI. They live in `tests/e2e/` and are gated behind `ARK_E2E=1`:

```bash
# Unit + integration tests (fast, no zellij needed)
cargo test --all-features

# E2E tests (requires zellij + delta on PATH)
ARK_E2E=1 cargo test --all-features -- --test-threads=1
```

E2E tests run single-threaded to avoid zellij session name collisions.

### Scenarios

| Scenario | Verifies |
|---|---|
| `spawn -> list` | Agent appears with expected phase |
| `spawn -> kill` | Tab closes, `status.json` shows Killed |
| `spawn -> stall -> detect` | Long-silent session triggers Stall event |
| `spawn -> done` | Fake Stop hook fires, Done emitted, auto-close works |
| `crashed supervisor` | Simulated OOM, `ark list` shows Crashed, `ark doctor --fix` archives |

### Mock claude shim

E2E tests use a mock `claude` shim binary that emits scripted hook events on demand. The shim replaces the real `claude` on PATH during test execution, providing deterministic behavior without an API key or network access.

### Cleanup

All E2E tests clean up state dirs and zellij sessions on teardown, even on failure. Tests use a `Drop` guard on a cleanup struct to ensure session removal.

## Plugin tests

Wasm plugin tests run without a real zellij runtime:

- **Render tests:** Given a cache of `AgentSummary`, assert the text output contains expected chips and ordering.
- **Pipe handling tests:** Feed JSON payloads, assert cache updates and re-render triggers.
- **Fuzzy search tests:** Given known agents and a query string, assert ranked order.
- **Control protocol tests:** Serialize and deserialize request/response pairs.

Pure-Rust rendering and state logic is extracted from wasm-specific code so it can run in ordinary `#[test]` functions.

## CI matrix

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
      - name: Install zellij
        run: |
          # Prebuilt tarball (~10s, vs 5min for cargo install)
          curl -fsSL "https://github.com/zellij-org/zellij/releases/..." \
            | sudo tar -xz -C /usr/local/bin
      - name: Install delta
        run: |
          curl -fsSL "https://github.com/dandavison/delta/releases/..." \
            | sudo tar -xz -C /tmp
          sudo mv /tmp/git-delta-*/delta /usr/local/bin/
      - run: cargo test --all-features
      - run: ARK_E2E=1 cargo test --all-features -- --test-threads=1
```

**Platform coverage:** macOS (arm64, x64) and Linux (x64, arm64). The CI installs zellij and delta from prebuilt tarballs (~10 seconds) rather than `cargo install` (~5 minutes) or `brew install` (~30-90 seconds). Linux uses musl-linked static binaries that run on any `ubuntu-*` runner regardless of glibc version.
