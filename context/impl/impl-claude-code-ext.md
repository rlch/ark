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
| T-008a | 1 | R1 | PARTIAL | `4253771` | `CC_HOOK_BYTES` STUB in `src/lib.rs` = `&[]`; real `include_bytes!` via `crates/cli/build.rs` deferred — risk of cargo-in-cargo deadlock (F-709 pathology) with native binary sharing outer target dir. Downstream T-019/T-023 consume `CC_HOOK_BYTES` symbol cleanly against empty slice. TODO comment names the real-embedding task. |
| T-008 | 1 | R9 | DONE | `4253771` | Non-goal marker expanded in `src/lib.rs` module doc — `READ_ONLY_TOOLS` / `PermissionPolicy` / `POLICY_FILE_NAME` NOT restored (git-history-only, v0.2-stretch MCP surface). |
| T-007 | 1 | R8 | DONE | `4253771` | `src/transcript.rs` — `notify`-based recursive watcher + `TailCursor` byte-offset JSONL tail. Survives truncation (length shrink) + rotation (inode flip). Pull model per R8. `TranscriptEvent::FileAppeared` fires for new files under `<dir>/subagents/` but emits NO ExtEvent (R3 lifecycle is cc-hook hook). Zero orchestrator-trait surface salvaged (kit §Non-goals). |
| T-006 | 1 | R1+R2 | DONE | `4253771` | `bin/cc-hook/main.rs` — clap CLI (`--session <sid> --socket <path> --event <HookEventName> [--first-post]`), stdin→`HookPayload` parse (placeholder-fallback on junk), NDJSON envelope via `NdjsonLine`, unix-socket POST write-line-exit-0. Fail-open on every error (R2). `BRIDGE_VERSION = env!("CARGO_PKG_VERSION")` — drives R4 handshake. DROPPED from salvage: FIFO writer / zellij pipe / allow payload / bridge subcommands / policy plumbing — all pre-R2 surface belonging to v0.2-stretch MCP or to ark-bus. Added deps: clap, chrono, anyhow, tracing-subscriber (workspace entries). |
| T-005 | 1 | R2+R3 | DONE | `4253771` | `src/hook_payload.rs` — preserved serde shape of legacy `HookPayload` (session_id, cwd, hook_event_name, tool_name?, tool_input?, `#[serde(flatten)] extra`). New `NdjsonLine` envelope wrapping payload per R2 wire (`kind`, `session_id`, `payload`, `emitted_at`, `bridge_version?`). `payload_to_ext_event` emits `ExtEvent { ext: "claude-code", kind: <R3 kind>, payload: <verbatim> }`; no per-kind restructuring. `flat_event_name(ev)` convenience for `on "claude-code.<kind>"` matching. |
| T-004 | 1 | R3 | DONE | `4253771` | `src/hook_event.rs` — 10-variant `HookEvent` enum covering Claude Code's full hook surface (expanded from legacy 6). `as_str()` → Claude's PascalCase wire name, `ext_kind()` → R3 dotted strings (`session.start`, `subagent.stop`, …). `HookEvent::ALL` drives settings.json reconciliation (T-019/T-020). |
| T-001 | 0 | R1 | DONE | `829fa3c` | `extensions/` workspace dir + registered in top-level `Cargo.toml` members |
| T-002 | 0 | R1 | DONE | `829fa3c` | `extensions/claude-code/Cargo.toml` with R1 dep budget + `[[bin]] cc-hook` target at `bin/cc-hook/main.rs` |
| T-003 | 0 | R1 | DONE | `829fa3c` | `src/lib.rs` with `ClaudeCodeExtension` unit struct + empty `impl ArkExtension` (trait defaults inherited); `bin/cc-hook/main.rs` empty-body stub with T-006 TODO |

## Tier 1 notes (2026-04-18)

- Salvage serial on main tree (memory: parallel agents on main git-collide). Scope strict: `extensions/claude-code/` + workspace dep table only. NOT touching `crates/hook/` or `crates/orchestrators/claude-code/` — Phase 4 owns those deletions.
- T-004 EXPANSION: legacy enum had 6 variants (PostToolUse, Stop, PermissionRequest, Notification, SessionEnd, TaskCompleted). Extension needs 10 (all Claude Code hooks per R1 + R3). PermissionRequest + TaskCompleted DROPPED (legacy artefacts of the pre-soul permission surface); added SessionStart, UserPromptSubmit, PreToolUse, SubagentStart, SubagentStop, PreCompact. `as_str` → PascalCase wire (clap + hook_event_name validation), `ext_kind` → R3 dotted kinds.
- T-005 DIVERGENCE from legacy: the legacy `payload_to_events` produced multiple synthetic events per hook (tool.use + file.edited for edits, permission.asked/resolved pair, task.stopped, etc.). v0.1 is single-event-per-hook per R3 "verbatim payload": no restructuring, no truncation (scene author drives downstream via Rhai). Legacy `FILE_EDIT_TOOLS` / `SUMMARY_MAX_CHARS` not salvaged.
- T-006 SIGNIFICANT REDUCTION: legacy binary was ~800 LOC across 8 modules (allow/bridge/cli/event/lib/main/payload/pipe/run/writer). R2 needs ~200 LOC total — just CLI parse, stdin read, NDJSON envelope, unix-socket POST, exit 0. Dropped: zellij pipe, FIFO/JSONL writer, PermissionRequest allow-payload stdout writer, bridge subcommands (intent/emit/permit). All five dropped surfaces either moved ark-side (pipe distribution, JSONL persistence) or deferred to v0.2-stretch MCP (permission handling).
- T-007 SHAPE-ONLY SALVAGE: pre-deletion `crates/orchestrators/claude-code/src/lib.rs` never actually had `notify`-based tail code — it was an orchestrator stub mid-rewrite. Salvaged the *shape* R8 needs from scratch: `TailCursor` (byte-offset, truncation-by-length + rotation-by-inode), `TranscriptWatcher` (recursive `notify` watch), `TranscriptEvent` (Changed | FileAppeared). Pull model. NO orchestrator trait, NO `detect()`, NO `ClaudeCodeConfig` — those stay in the legacy crate for Phase 4 to delete.
- T-008a STUB: real `include_bytes!` embedding deferred. Rationale: the wasm plugins work via `cargo build --target wasm32-wasip1` in an isolated `CARGO_TARGET_DIR` under $OUT_DIR, so the inner build lives in a completely separate target-triple tree and can't deadlock the outer `cargo build --workspace`. A native `cc-hook` build inside `ark-cli`'s build.rs would share the host target triple and the same `CARGO_TARGET_DIR`, reintroducing the F-709 deadlock pathology the wasm stack explicitly works around. Flagging PARTIAL so T-019/T-023 can compile against `CC_HOOK_BYTES == &[]` (they just surface a helpful "not embedded" message). Real embedding TODO: revisit with a packaging-time approach (cargo-dist + `cargo install --bin cc-hook`) rather than build-time embedding. Safer for distribution; drops one indirection.
- Deps added to `extensions/claude-code/Cargo.toml`: `clap`, `chrono`, `anyhow`, `tracing-subscriber`. All already in workspace dep table — crate-level table uses `{ workspace = true }` per constraint. `tempfile` added under `[dev-dependencies]` for cc-hook unit tests.
- Validation:
  - `cargo build -p ark-ext-claude-code` ✓ (0 errors)
  - `cargo build -p ark-ext-claude-code --bin cc-hook` ✓
  - `cargo build -p ark-cli` ✓ (exercises `crates/cli/build.rs` untouched)
  - `cargo build --workspace` ✓ (0 errors, only pre-existing wasm-plugin build fallback warnings)
  - `cargo test -p ark-ext-claude-code` ✓ (23 unit tests pass across 3 modules + cc-hook bin)
  - `cargo test --workspace --tests` ✓ (2080 pass / 4 ignored / 0 fail — no regressions)
  - `cargo fmt --all --check` ✓
  - `cargo clippy -p ark-ext-claude-code --all-targets` ✓ (no claude-code warnings; pre-existing `ark-ext-proto` lint drift unrelated to this tier)
- Next: Tier 2 (T-009..T-013) — hook IPC foundation. cc-hook already POSTs NDJSON (T-006 delivered on T-009 behaviour as a side effect); Tier 2 adds the ark-side socket reader + bridge-version handshake plumbing.

## Tier 0 notes (2026-04-18)

- Crate name: `ark-ext-claude-code` (mirrors `ark-ext-proto` / `ark-ext-metadata` / `ark-ext-test-support` convention).
- Directory: `extensions/claude-code/` at repo root — NEW top-level dir, NOT under `crates/`. Explicit member line added to workspace `Cargo.toml` (the existing `crates/*` globs don't match).
- Dep budget landed: `ark-types`, `ark-ext-proto`, `ark-view`, `ark-scene`, `notify`, `tokio`, `serde`, `serde_json`, `tracing`, `async-trait`. `async-trait` is REQUIRED (not in the build-site list but needed because upstream `ArkExtension` is `#[async_trait]`-based; an `impl` block without it fails to compile). `ark-ext-metadata-types` deliberately skipped — no manifest work in scaffolding, T-019+ can add it on demand.
- Every `ArkExtension` method has a trait-default returning either `method_not_found` or `Ok(Default)`, so the scaffolding `impl` block is legitimately empty. T-020..T-045+ override methods as they land.
- `cc-hook` binary body is a single empty `fn main()` + T-006 TODO. Validated that the target builds (cargo build -p ark-ext-claude-code --bin cc-hook succeeded).
- Workspace build green: `cargo build --workspace` → 0 errors. (Pre-existing unrelated warnings about wasm plugin build fallback — not introduced by this tier.)
