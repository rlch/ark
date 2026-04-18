---
created: "2026-04-18"
last_edited: "2026-04-18"
---
# Cleanup Packet A — Precondition Audit (T-001)

Salvage verification of soon-to-be-deleted crates against the
claude-code extension that absorbed them (per
`context/plans/build-site-cleanup.md` Phase 4 Tier 0).

## Salvage source → destination map

| Deleted file (pre-cleanup) | Salvaged destination |
| --- | --- |
| `crates/hook/src/lib.rs` | `extensions/claude-code/bin/cc-hook/main.rs` (rehomed binary entry; library surface absorbed into `extensions/claude-code/src/{hook_event,hook_payload,socket}.rs`) |
| `crates/hook/src/event.rs` | `extensions/claude-code/src/hook_event.rs` (215 LOC) |
| `crates/hook/src/payload.rs` | `extensions/claude-code/src/hook_payload.rs` (276 LOC) |
| `crates/hook/src/run.rs` | entry flow folded into `extensions/claude-code/bin/cc-hook/main.rs` (471 LOC); permission policy path is gone because the extension rebuilds `READ_ONLY_TOOLS`/`POLICY_FILE_NAME` locally (see `extensions/claude-code/src/lib.rs` docs §35-40) |
| `crates/hook/src/bridge.rs` | socket IPC rehomed to `extensions/claude-code/src/socket.rs` (657 LOC) |
| `crates/hook/src/allow.rs` | auto-allow logic rehomed into the ext's local `READ_ONLY_TOOLS` table (ext lib.rs §35-40) |
| `crates/hook/src/pipe.rs` | zellij pipe-target forwarding removed in v0.1 (hook is now write-only NDJSON per 2026-04-18 pivot) |
| `crates/hook/src/writer.rs` | NDJSON writer folded into `extensions/claude-code/bin/cc-hook/main.rs` |
| `crates/hook/src/cli.rs` | clap surface rebuilt in `extensions/claude-code/bin/cc-hook/main.rs` |
| `crates/orchestrators/claude-code/src/lib.rs` | transcript-watch + detect logic moved to `extensions/claude-code/src/transcript.rs` (913 LOC) — note at line 47 explicitly calls out the "ClaudeCodeOrchestrator struct, Orchestrator impl, detect" absorption |
| `crates/types/src/permission.rs` | `READ_ONLY_TOOLS` / `PermissionPolicy` / `POLICY_FILE_NAME` re-declared as ext-local per ext lib.rs §35-40 — NOT re-exported from `ark_types` per kit §Non-goals |
| `crates/orchestrators/cavekit/` | **NOT salvaged** — Phase-1 stub, deleted outright per cleanup kit P4-R2 (no extension carries these watcher bodies in v0.1) |

## Assertions (per kit P4-R1)

- `git grep -l READ_ONLY_TOOLS extensions/` → 1 hit (`extensions/claude-code/src/lib.rs`)
- `ls extensions/claude-code/bin/cc-hook/` → shows `main.rs` (471 LOC) — salvaged hook surface present

## Gate status

GREEN — salvage verified; cleanup cascade T-003..T-006 cleared to proceed.
