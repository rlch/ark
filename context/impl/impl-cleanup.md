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
| T-002 | 0 | 4 | DONE | `<pending-tier0>` | ark-hook binstall shim deleted from `crates/cli/Cargo.toml` (F-706/F-710 stanzas + `_binstall_shim` feature gone); bin-dir comment reworded for single-binary reality; `cargo check -p ark-cli` green |
| T-001 | 0 | 4 | DONE | `<pending-tier0>` | salvage precondition audit → `context/impl/cleanup-preconditions.md`; `READ_ONLY_TOOLS` hit in `extensions/claude-code/src/lib.rs`; cc-hook `main.rs` (471 LOC) present |

## Factory.rs temporary patches

Packet A Tier 1 will neutralise factory.rs references to the deleted orchestrator crates (Packet B T-008 deletes factory.rs outright).
Pending until Tier 1 commit.
