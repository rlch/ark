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

## Tier 4 Gate — Cycle 2 (2026-04-15)

Four additional findings raised by codex against Tier 4 CLI work (cavekit-cli R5/R7 plus list/pane IO hygiene). All four fixed in a single commit.

### F-504 — P2 `ark doctor` must NOT mutate state during diagnosis (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (`check_runtime_dir`, `check_state_dir`)

**Description:** Both checks called `fs::create_dir_all` when the dir was missing and then reported the result as healthy. This silently mutated the filesystem on plain `ark doctor` (no `--fix`) and hid missing-dir conditions behind a success status.

**Resolution:** Introduced a shared non-mutating helper `check_dir_writable(name, dir)`:
- missing → `Fail` + `FixAction::CreateDir` (the fix only runs under `--fix`);
- present but unwritable → `Fail` with no auto-fix (permission issue, user decides);
- present + writable → `Ok`.
Writability is still probed with a tempfile inside the existing dir. `check_runtime_dir` and `check_state_dir` now just delegate to the helper. Added four tests: `check_runtime_dir_missing_does_not_create_dir`, `check_state_dir_missing_does_not_create_dir`, `check_state_dir_fix_creates_missing_dir`, `check_state_dir_writable_when_present`, `check_state_dir_fails_without_auto_fix_when_unwritable` (unix-only, skipped under root).

### F-505 — P2 `ark list` must surface real IO errors, not silence them (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/list.rs (`gather_rows`, callers in `run_once`)

**Description:** `gather_rows` called `list_agent_ids(layout).unwrap_or_default()`, so a `PermissionDenied` on `$STATE/agents` became an empty list and the user saw `(no agents)` with a zero exit. `list_agent_ids` already treats a missing dir as empty, so any remaining `Err` is a genuine environment failure that must be surfaced.

**Resolution:** Changed `gather_rows` signature to `Result<Vec<Row>, CliError>` and propagated `list_agent_ids` errors as `CliError::Generic { reason: format!("read agents_root: {err}") }`. Updated both call-sites in `run_once` to `?`. Added `gather_rows_surfaces_io_failure_when_agents_root_unreadable`: chmod-000s `agents_root`, asserts `Err(CliError::Generic)` with the expected reason; skipped under root where mode 000 is bypassed. Existing `missing_socket_yields_orphan_row_in_table_mode` updated to unwrap the new `Result`.

### F-506 — P2 `ark pane` must preserve resolver IO failures (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/pane.rs (`map_resolve_err`)

**Description:** `ResolveError::Io` mapped to `CliError::NotFound`, so a permission-denied or corruption failure during id resolution appeared as "agent not found" — misleading and inconsistent with `kill.rs` which already maps IO errors to `CliError::Generic`.

**Resolution:** `ResolveError::Io(err)` now maps to `CliError::Generic { reason: format!("resolve: {err}") }`, matching the pattern elsewhere. `Ambiguous*` and `NotFound` are unchanged. Existing test `resolve_err_io_maps_to_cli_not_found` renamed to `resolve_err_io_maps_to_cli_generic` and flipped to assert `Generic` with a reason containing both "resolve" and the underlying IO message.

### F-507 — P3 `ark doctor` config_dir must check writability (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P3
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (`check_config_dir`)

**Description:** Any existing `config_dir` was reported as `Ok`, even if it was read-only. A read-only config_dir silently breaks `ark config edit`/`set`, but doctor claimed the env was healthy.

**Resolution:** After confirming existence, `check_config_dir` now probes writability via the same tempfile method used for state/runtime dirs. Writable → `Ok` ("writable {path}"); unwritable → `Warn` ("config_dir not writable: {path}"). Kept as `Warn` (not `Fail`) because reading config still works — only `edit`/`set` paths are impacted. Missing-dir branch still emits `Warn` + `FixAction::CreateDir` as before. Added `check_config_dir_ok_when_writable` and `check_config_dir_warns_when_not_writable` (unix-only, skipped under root).

## Test Delta — Cycle 2

ark-cli: 204 baseline → 212 passing (+8 new tests: 5 for F-504, 1 for F-505, 2 for F-507; F-506 flipped an existing assertion).

Pre-existing flaky failure in `ark-engines-claude-code::transcript::tests::append_path_emits_initial_then_appended` observed on this commit; unrelated to the files touched here.

## Tier 4 — Cycle 3 (Codex)

### F-508 — P1 `ark config set` must validate before persisting (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/config.rs (`run_set`)

**Description:** `run_set` serialized the edited in-memory `toml::Value` table and wrote it directly over the user config file, then returned `Ok`. Schema-invalid values (e.g. `ark config set diff.debounce_ms "nope"`) were persisted unchecked, leaving `config.toml` unreadable for later commands that call `ConfigLoader::load::<Config>()`.

**Resolution:** `run_set` now writes the rendered TOML to a sibling temp file (`.config.toml.tmp.<pid>`), then validates it by building a fresh `ConfigLoader::new().with_user_path(Some(tmp))` and calling `.load::<Config>()`. On validation failure the temp is removed and the original file is left on disk untouched; a `CliError::ConfigError { reason: "invalid value for {key}: {figment err}" }` is returned. On success the temp is renamed atomically over the real file (`fs::rename`). The project/env layers are intentionally skipped during validation so an unrelated env override can't reject a legitimate edit. Added `set_rejects_schema_invalid_value_and_preserves_original_file` (confirms the original TOML is byte-identical after rejection and no temp leaks) and `set_accepts_schema_valid_value_via_validation_path` (regression for the happy path through the new validation pipeline).

### F-509 — P1 consolidate process-env test lock across ark-cli (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/lib.rs (new `test_lock` module), crates/cli/src/ctx.rs, crates/cli/src/commands/{config,doctor,spawn}.rs

**Description:** Every module that exercised process-global env vars (`ctx.rs`, `config.rs`, `doctor.rs`, `spawn.rs`) declared its own private `static LOCK: Mutex<()>`. Since env vars are genuinely process-global, separate mutexes do not serialize with each other — parallel `cargo test -p ark-cli` could flake when one test flipped `PATH` while another was reading it (or similarly for `HOME`, `EDITOR`, `ARK_*`).

**Resolution:** Added a single `#[cfg(test)] pub(crate) mod test_lock { pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(()); }` block in `crates/cli/src/lib.rs`. All four env-touching modules now `use crate::test_lock::ENV_LOCK;` and their private mutexes were deleted. The module is `#[cfg(test)]`-gated so the shared lock does not ship in release binaries. Verified by running `cargo test -p ark-cli` (parallel, no `--test-threads=1`) twice consecutively: both runs report 217 passed / 0 failed, confirming the flake window is closed.

### F-510 — P2 `$EDITOR` parsing must handle quoted args (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/config.rs (`run_edit`)

**Description:** The local `shlex_split` helper used `split_whitespace`, which mangles `EDITOR="/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl --wait"` (splits the path in half at the space) and `EDITOR='sh -c "vim \$1"'` (drops quoting entirely). The original comment said shlex was avoided to keep the dep surface small, but T-087 already pulled the `shlex` crate into the workspace for hook argv parsing.

**Resolution:** Replaced the local helper with `shlex::split(&editor)`, which follows POSIX shell word-splitting rules. `None` (unparseable input, e.g. unterminated quote) surfaces as `CliError::ConfigError { reason: "invalid $EDITOR syntax: <editor>" }` instead of silently degrading. Empty-argv remains a `ConfigError` ("$EDITOR is empty"). Added `editor_parses_quoted_path_with_spaces` and `editor_parses_sh_c_wrapper_with_nested_quotes` as positive regressions, plus `editor_invalid_syntax_returns_config_error` to cover the unparseable-input path via `run_edit`.

## Test Delta — Cycle 3

ark-cli: 212 baseline → 217 passing (+5 new tests: 2 for F-508, 3 for F-510; F-509 is purely test-infra plumbing and adds no new assertions — verified instead by clean parallel runs). Full-workspace serial run (`cargo test --workspace -- --test-threads=1`) green end-to-end. One unrelated flaky failure in `ark-orchestrators-cavekit::watchers::ralph_loop` was observed once under full-workspace load and cleared on rerun; not in any file touched this cycle.
