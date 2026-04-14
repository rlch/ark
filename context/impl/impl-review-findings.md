---
created: "2026-04-15"
last_edited: "2026-04-15"
---
# Implementation Review Findings

Build site: context/plans/build-site.md

Track codex adversarial review findings and their resolution status across tiers.

## Tier 4 Gate (2026-04-15)

Four findings raised by codex against Tier 4 CLI work (cavekit-cli R2/R4/R6). All four fixed in a single commit; tests flipped to assert the new contracts.

### F-500 — P1 PRESERVE WORKTREES by default on `ark kill` (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/kill.rs:64–75 (`build_request`)

**Description:** `ark kill <id>` without `--keep-worktree` was sending `{"remove_worktree": true}`. Once the supervisor honors the flag, that default would silently destroy user worktrees. cavekit-cli R4 states v1 default is to KEEP worktrees.

**Resolution:** Inverted the default in `build_request`. The default `Kill` envelope now emits `remove_worktree=false`, and `--keep-worktree` remains redundant-but-explicit (also emits false). `--force` still emits `ForceKill` with no args. Updated tests `build_request_default_preserves_worktree` and `build_request_keep_worktree_redundantly_preserves`. No `--remove-worktree` placeholder was added — cavekit-cli R4 does not call for one at this stage.

### F-501 — P2 IDEMPOTENT `ark kill` for dead agents (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/kill.rs:101–120 (`map_connect_err`) + `run`

**Description:** When the supervisor socket was missing or refused (ENOENT / ConnectionRefused), `run()` returned `CliError::OrphanOrDead` → nonzero exit. R4 mandates idempotent behavior: repeated kills against already-dead agents must succeed.

**Resolution:** Introduced an internal `ConnectOutcome { AlreadyDead, Err(CliError) }`. `map_connect_err` now maps NotFound/ConnectionRefused → `AlreadyDead`; `run()` prints `warning: agent {id} is already dead; nothing to do` to stderr and returns `Ok(())`. Truly-unreachable errnos (EACCES, etc.) still become `CliError::Generic`. `run_returns_orphan_when_socket_missing` renamed to `run_is_idempotent_when_socket_missing` and flipped to assert `Ok(())`. Added `map_connect_err_maps_*` unit tests for each branch.

### F-502 — P2 Honor `ARK_CONFIG_PATH` in ark-config loading (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/config.rs:84–104 (`user_config_path`)

**Description:** All four config subcommands computed the user config path as `ctx.config_dir/config.toml`, ignoring `ARK_CONFIG_PATH`. The ark-config crate lists this env var in `RESERVED_ENV_VARS` and documents it as the single-file override, but the CLI wasn't reading it.

**Resolution:** `user_config_path` now reads `ARK_CONFIG_PATH` via `std::env::var_os` when non-empty and uses it as the user layer path; falls back to `ctx.config_dir/config.toml` otherwise. ctx.rs was NOT modified — env read is local to config.rs. Added three tests under the existing `ENV_LOCK` serialization: `user_config_path_honors_ark_config_path_env`, `user_config_path_falls_back_to_ctx_config_dir`, `set_writes_to_ark_config_path_override` (higher-level regression proving `run_set` writes to the override path, not the ctx dir).

### F-503 — P2 Preflight before mutating state in `ark spawn` (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs:328–366 (`run`)

**Description:** `run()` wrote `spec.json` and created the agent dir BEFORE preflight-checking `zellij` on PATH. A preflight failure left orphan state for the user to clean up by hand.

**Resolution:** Reordered `run()`: parse + resolve orchestrator + derive name + parse env/hooks (all pure) happens first, then `require_zellij_on_path()` runs BEFORE any filesystem mutation. On preflight failure, `CliError::PreflightFail` surfaces with zero orphan state. Orchestrator auto-detect stays early (it's a read-only probe of cwd). Added `run_preflight_fail_leaves_state_untouched`: snapshots `agents_root` entry count, runs with `PATH=/nonexistent-path-for-ark-test`, asserts PreflightFail and unchanged entry count.

## Test Delta

ark-cli: 197 baseline → 204 passing (+7 new tests across the three edited files).
