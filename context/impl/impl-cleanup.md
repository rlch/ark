---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: Cavekit Soul Cleanup (Packets A + B)

Build site: `context/plans/build-site-cleanup.md`

Ledger append-only. Newest entries at top.

## Task Status

| Task | Tier | Phase | Status | SHA | Notes |
|------|------|-------|--------|-----|-------|
| T-011 | 3 | 5 | DONE | `<pending-P5-T011>` | Audit-only, no code changes. Greps under `crates/scene/src/` and `crates/core/src/consumers/` for `ark_core::engine`, `ark_core::orchestrator`, `use ark_core::(Engine\|Orchestrator)`, `EngineLaunch\|engine_launch` — all return 0 hits. Phase 1 T-005/T-020/T-026 cleanup held; T-010 did not regress either crate. `cargo check -p ark-scene --tests` = 0 errors (6 crates compiled, 12.36s). |
| T-010 | 3 | 5 | DONE | `75ec431` | DEL 5 files: `crates/supervisor/src/engine_stub.rs` (188 LOC) + `crates/core/src/{engine,engine_contract,orchestrator,orchestrator_contract}.rs` (1038 LOC total). `mod engine_stub;` + `pub use engine_stub::{AcpEngineStub, preflight as engine_preflight};` removed from `crates/supervisor/src/lib.rs`; `pub mod engine; pub mod engine_contract; pub mod orchestrator; pub mod orchestrator_contract;` + `pub use engine::{ApprovalPolicy, Engine, EngineHandle};` + `pub use orchestrator::{Orchestrator, World};` removed from `crates/core/src/lib.rs`; doc-module comments rewritten. `World::new(...)` construction at `orchestration.rs:274` (only surviving callsite) collapsed to `cancel.cancelled().await` — `cancel` was already in local scope, `World` was pure-overhead. `use ark_core::{Config, World};` → `use ark_core::Config;`. `crates/test-fixtures/src/lib.rs:119` doc-comment `[ark_core::engine::contract]` reference rewritten to note the deletion. Grep gate (`crates/`): `ark_core::engine|ark_core::orchestrator|ApprovalPolicy|EngineHandle|pub trait Engine|pub trait Orchestrator` = 1 hit, all in a single doc-comment at `test-fixtures/src/lib.rs:119` describing the removal (acceptable). `cargo check --workspace --tests` = 0 errors. |
| T-009 | 3 | 5 | DONE | `6975f58` | Parent-landed inline after Packet B agent crashed mid-task. `run_supervisor_with` sig cut from 10 args to 7 — dropped `engine: Option<Box<dyn Engine>>`, `orchestrator: Option<Box<dyn Orchestrator>>`, `run_preflight: bool`. R3 step 6 collapsed to mux-only; step 10 skipped; step 13 bare-session park; step 15 skipped. `engine_stub::preflight` no-op was inlined to nothing. Test callers migrated from 10-arg to 7-arg call shape. Workspace green; sets up T-010 to delete the trait surface outright. |
| T-008 | 3 | 5 | DONE | `de73dc6` | `crates/supervisor/src/factory.rs` deleted whole (219 LOC). `build_multiplexer("zellij", &config)` inlined at sole call site (`orchestration.rs:66`) as `Arc::new(ZellijMux::new())` with inline comment noting `MUX_V1 = ["zellij"]` v1-lock. `build_engine`/`build_orchestrator`/`SupervisorError` were dead code — went with file. `pub mod factory;` + `pub use factory::{…}` dropped from lib.rs with a cleanup-T-008 comment. Grep gate (`crates/`): `build_engine|build_orchestrator|build_multiplexer` = 0 live-code hits (3 comment lines remain explaining the removal). `cargo check --workspace --tests` = 0 errors. |
| T-007 | 2 | 4 | DONE | `3169b1d` | Phase 4 green gate: cargo check --workspace --tests = 0 errors; cargo build -p ark-cli = 0 errors; cargo test --workspace --tests = 2164 passed / 4 ignored / 0 fail (69 suites, 34.46s); cargo fmt --all --check = clean; greps (crates/): ark_hook=0, ark_orchestrators=0, CavekitOrchestrator=0, ClaudeCodeOrchestrator=0, ark_types::permission=0, PermissionPolicy/PolicyDecision/READ_ONLY_TOOLS/POLICY_FILE_NAME=0 |
| T-006 | 2 | 4 | DONE | `<pending-tier2>` | `crates/types/src/permission.rs` deleted; `pub mod permission` + the 4-line `pub use permission::{…}` re-export removed from `crates/types/src/lib.rs`; Cargo.toml unchanged (no permission-specific dep gate) |
| T-005 | 1 | 4 | DONE | `df7206f` | `crates/hook/` deleted; `"crates/hook"` removed from workspace `members`; `justfile` install/uninstall dropped ark-hook lines (runtime spawn in ark-bus plugin left as external-binary callsite — out-of-scope doc) |
| T-004 | 1 | 4 | DONE | `df7206f` | `crates/orchestrators/claude-code/` deleted; dep removed from cli + supervisor Cargo.toml; `crates/orchestrators/*` workspace-glob dropped |
| T-003 | 1 | 4 | DONE | `df7206f` | `crates/orchestrators/cavekit/` deleted; dep removed from cli + supervisor Cargo.toml; factory.rs refs no-op'd (see "Factory.rs patches" below) |
| T-002 | 0 | 4 | DONE | `e2fffcd` | ark-hook binstall shim deleted from `crates/cli/Cargo.toml` (F-706/F-710 stanzas + `_binstall_shim` feature gone); bin-dir comment reworded for single-binary reality; `cargo check -p ark-cli` green |
| T-001 | 0 | 4 | DONE | `e2fffcd` | salvage precondition audit → `context/impl/cleanup-preconditions.md`; `READ_ONLY_TOOLS` hit in `extensions/claude-code/src/lib.rs`; cc-hook `main.rs` (471 LOC) present |

## Factory.rs patches (temporary, until Packet B T-008)

- Removed `use ark_orchestrators_cavekit::CavekitOrchestrator` +
  `use ark_orchestrators_claude_code::ClaudeCodeOrchestrator`.
- `build_orchestrator` now returns `Err(anyhow!("orchestrator slug `{slug}` unresolvable — … cleanup Packet A"))` for every slug. No production caller invokes this path (`run_supervisor` hard-codes `orchestrator = None`).
- Deleted per-slug positive tests (`build_orchestrator_cavekit_returns_ok`, `build_orchestrator_claude_code_returns_ok`, `cavekit_orchestrator_name_is_cavekit`); added `build_orchestrator_always_errors` which asserts the new negative contract across four slugs (including the historical positives).
- Doc comments rewritten to describe by role so the T-007 grep gate (`CavekitOrchestrator|ClaudeCodeOrchestrator`) comes back clean.
- TODO(cleanup-T-008) markers left on both the prod fn and the test cluster so Packet B deletes the whole file cleanly.
