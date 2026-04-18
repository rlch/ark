---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: Cavekit Soul Cleanup (Packet A)

Build site: `context/plans/build-site-cleanup.md`

Ledger append-only. Newest entries at top.

## Task Status

| Task | Tier | Phase | Status | SHA | Notes |
|------|------|-------|--------|-----|-------|
| T-005 | 1 | 4 | DONE | `<pending-tier1>` | `crates/hook/` deleted; `"crates/hook"` removed from workspace `members`; `justfile` install/uninstall dropped ark-hook lines (runtime spawn in ark-bus plugin left as external-binary callsite — out-of-scope doc) |
| T-004 | 1 | 4 | DONE | `<pending-tier1>` | `crates/orchestrators/claude-code/` deleted; dep removed from cli + supervisor Cargo.toml; `crates/orchestrators/*` workspace-glob dropped |
| T-003 | 1 | 4 | DONE | `<pending-tier1>` | `crates/orchestrators/cavekit/` deleted; dep removed from cli + supervisor Cargo.toml; factory.rs refs no-op'd (see "Factory.rs patches" below) |
| T-002 | 0 | 4 | DONE | `e2fffcd` | ark-hook binstall shim deleted from `crates/cli/Cargo.toml` (F-706/F-710 stanzas + `_binstall_shim` feature gone); bin-dir comment reworded for single-binary reality; `cargo check -p ark-cli` green |
| T-001 | 0 | 4 | DONE | `e2fffcd` | salvage precondition audit → `context/impl/cleanup-preconditions.md`; `READ_ONLY_TOOLS` hit in `extensions/claude-code/src/lib.rs`; cc-hook `main.rs` (471 LOC) present |

## Factory.rs patches (temporary, until Packet B T-008)

- Removed `use ark_orchestrators_cavekit::CavekitOrchestrator` +
  `use ark_orchestrators_claude_code::ClaudeCodeOrchestrator`.
- `build_orchestrator` now returns `Err(anyhow!("orchestrator slug `{slug}` unresolvable — … cleanup Packet A"))` for every slug. No production caller invokes this path (`run_supervisor` hard-codes `orchestrator = None`).
- Deleted per-slug positive tests (`build_orchestrator_cavekit_returns_ok`, `build_orchestrator_claude_code_returns_ok`, `cavekit_orchestrator_name_is_cavekit`); added `build_orchestrator_always_errors` which asserts the new negative contract across four slugs (including the historical positives).
- Doc comments rewritten to describe by role so the T-007 grep gate (`CavekitOrchestrator|ClaudeCodeOrchestrator`) comes back clean.
- TODO(cleanup-T-008) markers left on both the prod fn and the test cluster so Packet B deletes the whole file cleanly.
