---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Implementation Tracking: claude-code-ext

Build site: context/plans/build-site-claude-code-ext.md

Ledger is append-only. Newest entries at top.

## Task Status

| Task | Tier | Kit R | Status | SHA | Notes |
|------|------|-------|--------|-----|-------|
| T-001 | 0 | R1 | DONE | `829fa3c` | `extensions/` workspace dir + registered in top-level `Cargo.toml` members |
| T-002 | 0 | R1 | DONE | `829fa3c` | `extensions/claude-code/Cargo.toml` with R1 dep budget + `[[bin]] cc-hook` target at `bin/cc-hook/main.rs` |
| T-003 | 0 | R1 | DONE | `829fa3c` | `src/lib.rs` with `ClaudeCodeExtension` unit struct + empty `impl ArkExtension` (trait defaults inherited); `bin/cc-hook/main.rs` empty-body stub with T-006 TODO |

## Tier 0 notes (2026-04-18)

- Crate name: `ark-ext-claude-code` (mirrors `ark-ext-proto` / `ark-ext-metadata` / `ark-ext-test-support` convention).
- Directory: `extensions/claude-code/` at repo root — NEW top-level dir, NOT under `crates/`. Explicit member line added to workspace `Cargo.toml` (the existing `crates/*` globs don't match).
- Dep budget landed: `ark-types`, `ark-ext-proto`, `ark-view`, `ark-scene`, `notify`, `tokio`, `serde`, `serde_json`, `tracing`, `async-trait`. `async-trait` is REQUIRED (not in the build-site list but needed because upstream `ArkExtension` is `#[async_trait]`-based; an `impl` block without it fails to compile). `ark-ext-metadata-types` deliberately skipped — no manifest work in scaffolding, T-019+ can add it on demand.
- Every `ArkExtension` method has a trait-default returning either `method_not_found` or `Ok(Default)`, so the scaffolding `impl` block is legitimately empty. T-020..T-045+ override methods as they land.
- `cc-hook` binary body is a single empty `fn main()` + T-006 TODO. Validated that the target builds (cargo build -p ark-ext-claude-code --bin cc-hook succeeded).
- Workspace build green: `cargo build --workspace` → 0 errors. (Pre-existing unrelated warnings about wasm plugin build fallback — not introduced by this tier.)
