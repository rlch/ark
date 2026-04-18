---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Cleanup Packet A — Loop Log

Append-only. Newest wave on top.

## Wave 2 — Tier 1 (T-003 + T-004 + T-005)

- `crates/orchestrators/cavekit/` + `crates/orchestrators/claude-code/` + `crates/hook/` deleted (`rm -rf`).
- `Cargo.toml` workspace members: `"crates/hook"` line removed; `"crates/orchestrators/*"` glob replaced with a deletion-note comment.
- `crates/cli/Cargo.toml` + `crates/supervisor/Cargo.toml`: `ark-orchestrators-cavekit` + `ark-orchestrators-claude-code` path deps dropped.
- `crates/supervisor/src/factory.rs`: neutralised — removed both orchestrator-crate `use` imports; `build_orchestrator` now errors for every slug with a TODO(cleanup-T-008) marker; per-slug positive tests + `cavekit_orchestrator_name_is_cavekit` deleted, replaced with `build_orchestrator_always_errors`. Doc comments rewritten without the concrete type names so T-007's grep gate stays clean.
- `justfile` install/uninstall targets pruned of the `ark-hook` lines.
- Test-fixtures markdown prose (README.md + ralph-loop.md + cavekit-project README + build-site.md + .claude/ralph-loop.local.md) retroactively annotated "historical — crate removed".
- Grep audit (crates/): `ark_hook` = 0; `ark_orchestrators` = 0; `CavekitOrchestrator` = 0; `ClaudeCodeOrchestrator` = 0. `ark-hook` (kebab prose/binary-name) still surfaces in ark-bus plugin runtime spawns + hook-ipc doc-comments — OUT OF SCOPE for Packet A (runtime external-binary callsite; documentation of the historical IPC surface).
- `cargo check --workspace --tests` → 0 errors.

## Wave 1 — Tier 0 (T-001 + T-002)

- Precondition audit: READ_ONLY_TOOLS hit in extensions/claude-code/src/lib.rs (1); cc-hook/main.rs present (471 LOC); salvage map in cleanup-preconditions.md.
- cli Cargo.toml diff: removed `[[bin]] ark-hook` stanza, `required-features = ["_binstall_shim"]`, `[features] _binstall_shim` block, F-706/F-710 prose; rewrote binstall bin-dir comment for single-binary layout.
- `grep -c 'ark-hook\|_binstall_shim' crates/cli/Cargo.toml` = 0.
- `cargo check -p ark-cli` → 0 errors, 9 warnings (pre-existing wasm-build placeholder warnings).
