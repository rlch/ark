---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Cleanup Packet A — Loop Log

Append-only. Newest wave on top.

## Wave 1 — Tier 0 (T-001 + T-002)

- Precondition audit: READ_ONLY_TOOLS hit in extensions/claude-code/src/lib.rs (1); cc-hook/main.rs present (471 LOC); salvage map in cleanup-preconditions.md.
- cli Cargo.toml diff: removed `[[bin]] ark-hook` stanza, `required-features = ["_binstall_shim"]`, `[features] _binstall_shim` block, F-706/F-710 prose; rewrote binstall bin-dir comment for single-binary layout.
- `grep -c 'ark-hook\|_binstall_shim' crates/cli/Cargo.toml` = 0.
- `cargo check -p ark-cli` → 0 errors, 9 warnings (pre-existing wasm-build placeholder warnings).
