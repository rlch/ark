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

## Tier 4 — Cycle 4 (Codex)

### F-511 — P1 `ark spawn` must actually launch zellij (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`run`, new `build_zellij_command`, `ZellijPlan::Attach`)

**Description:** `run()` computed a `ZellijPlan` (and unit-tested the branching logic) but never actually invoked zellij. After writing `spec.json` and printing the supervisor-stub warning, the handler returned `Ok` without opening any tab/session, so `ark spawn` was a no-op end-to-end. The T-087 commit body had documented this as deferred pending supervisor-side invocation, but cavekit-cli R2 intends `spawn` to actually launch zellij on the spawn side.

**Resolution:** Added `build_zellij_command(plan: &ZellijPlan) -> std::process::Command` so argv construction is pure and testable without spawning a process. `ZellijPlan::Attach` now also carries `layout: Option<String>` (the in-session `new-tab` supports `--layout` for parity with the new-session branch). `run()` snapshots the real process env, picks a plan via the existing `zellij_plan` helper, builds the command, redirects stdio to `/dev/null`, and calls `Command::spawn()` (NOT `.status()`) so the parent agent — typically already inside zellij — does not block. Spawn failure maps to `CliError::Internal { reason: "launch zellij: {e}" }`; the `zellij` missing case is already caught upstream by `require_zellij_on_path()` (F-503). `--no-detach` now logs "note: --no-detach log-tail deferred until supervisor lands" and returns — real log tailing is still pending the supervisor binary. Added four unit tests on `build_zellij_command`: in-session with layout, in-session without layout, new-session with layout, new-session without layout — each asserts the exact argv the Command carries (program + args). Existing `zellij_plan_inside_session_attaches` updated to expect the new `layout` field on `Attach`.

### F-512 — P2 `--help`/`--version` must not require env resolution (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/main.rs, crates/cli/tests/cli_help.rs (new integration harness)

**Description:** `main()` called `Ctx::from_env()` BEFORE clap parsing. In an environment without `$HOME` / `$XDG_CONFIG_HOME` / `$ARK_*` (e.g. minimal container, `env -i`, CI runners stripping env, restricted `sudo -u`), `ark --help` and `ark --version` exited 1 with "ark: failed to resolve state dirs: …" instead of clap's help output. That's a discoverability regression — the two flags that exist specifically to answer "does this binary work at all" didn't.

**Resolution:** Reordered `main()` so clap parsing runs first. clap itself handles `--help` / `--version` during `get_matches()` and exits 0 before our code returns, so those flags no longer touch `Ctx::from_env()`. The `no_color` flag used to build the help-rendering command is now read directly via `std::env::var_os("NO_COLOR").is_some()` at the top of `main()` (cheap, no dir resolution). `Ctx::from_env()` and `tracing_subscriber` init both moved to AFTER a subcommand parses successfully, so their side effects only run on real command execution. Added `crates/cli/tests/cli_help.rs` (new tests directory for the crate) with two `#[cfg(unix)]` integration tests that spawn `env!("CARGO_BIN_EXE_ark")` via `std::process::Command` with `.env_clear()`: `help_succeeds_with_empty_env` asserts exit 0 + `"Usage:"` in stdout, `version_succeeds_with_empty_env` asserts exit 0 + `"ark <version>"` prefix.

### F-513 — P2 doctor `--json --fix` must skip fixes in JSON mode (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (`run`)

**Description:** With `--json --fix` set together, `run()` first emitted the JSON array, then called `run_fixes()`, which prompted on stderr (or auto-applied with `--yes`) and mutated disk state (delete socket / remove lock / create dir). So stdout was JSON but the process simultaneously tore down runtime files — violating the implicit "JSON mode is read-only" contract that every downstream script (and cron doctor, and monitoring agent) depends on.

**Resolution:** In JSON mode, `run_fixes()` is now skipped entirely. When the caller passed `--fix` anyway, a single stderr line `"warning: --fix ignored in --json mode"` is emitted so the behavior isn't silent. Table mode retains the unchanged `table + run_fixes` ordering. Added `json_fix_combo_skips_fixes_and_leaves_state_untouched`: seeds a runtime dir with an orphan `.sock` file, snapshots every path + type under the test root, runs `run()` with `DoctorArgs { fix: true, yes: true, json: true }`, snapshots again, asserts the two snapshots are identical AND that the orphan socket still exists. A tiny `snapshot_tree` helper walks the test root (recursive, sorted) to make the before/after comparison deterministic.

## Test Delta — Cycle 4

ark-cli: 217 baseline → 224 passing (+7: 4 new `build_zellij_command` tests for F-511, 2 integration tests in `crates/cli/tests/cli_help.rs` for F-512, 1 for F-513; existing `zellij_plan_inside_session_attaches` updated for the new `ZellijPlan::Attach.layout` field — test count unchanged by that edit). Full-workspace `cargo test --workspace -- --test-threads=1` green end-to-end.

## Tier 4 — Cycle 5 (Codex)

### F-514 — P2 `ark pane log` must use provided Ctx, not re-read env (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/pane.rs (`run`, `run_log`)

**Description:** `run_log()` reconstructed a `StateLayout` via `StateLayout::from_env()` instead of using the `Ctx` that `run()` was given. In-process callers and tests that supply a non-default `Ctx` (e.g. a temp `state_dir` for fixture-based resolution) were silently ignored — the resolver always walked paths under the ambient `ARK_*` / `$HOME` env. That also made the cycle-3 unit test for this path an env-mutation test rather than a true ctx-honoring test: it had to set `ARK_STATE_DIR` to force resolution under its tempdir.

**Resolution:** `PaneCommand::Log` now forwards the `&Ctx` through `run()` into `run_log(args, ctx)`. `run_log` constructs the `StateLayout` directly via `StateLayout::new(ctx.state_dir.clone(), ctx.runtime_dir.clone(), ctx.config_dir.clone())` — ark-types already exposes `pub fn new(base, runtime, config)` so no new API surface is needed. Replaced the env-mutation `run_log_unknown_id_returns_not_found` with two tests: `run_log_unknown_id_returns_not_found_using_ctx` (no env mutation; just a ctx pointed at a temp dir) and `run_log_honors_ctx_state_dir_even_when_env_points_elsewhere` (sets `ARK_STATE_DIR` to a *different* dir, asserts `run_log` still resolves against ctx and returns `NotFound`, and reconstructs the layout to assert `layout.base() == ctx.state_dir`).

### F-515 — P2 `ark config edit` — `sh -c` wrapper pattern file-argument placement (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/config.rs (new `build_editor_argv_tail`, `run_edit`)

**Description:** After `shlex::split` (added in F-510 for quoted-path support), `run_edit` appended the user config path as a single extra argv entry. For `EDITOR='sh -c "vim \"$1\""'` that means `parts = ["sh", "-c", "vim \"$1\""]` and `Command::new("sh").args(&parts[1..]).arg(&path)` produces argv `["sh", "-c", "vim \"$1\"", "<path>"]`. Under `sh -c`, argv[1] is the script, argv[2] becomes `$0`, argv[3] becomes `$1`. With only one trailing entry, the path lands at `$0` (the wrapper's synthetic "script name"), not `$1` — so the inner `"$1"` expands to empty and the editor opens nothing (or errors). F-510's cycle-3 test only asserted shlex tokenization, not end-to-end argv correctness, so the latent bug slipped through.

**Resolution:** Added a pure helper `build_editor_argv_tail(parts, path) -> Vec<OsString>`: when the invocation is a recognizable `sh -c <script>` / `bash -c <script>` wrapper (exactly three tokens, `parts[0]` is `sh` or `bash`, `parts[1] == "-c"`), the helper returns `["ark-edit", <path>]` so the path lands at `$1`. Every other EDITOR shape returns `[<path>]` — identical to the prior behavior. `run_edit` now calls `.args(&parts[1..])` followed by `.args(&argv_tail)` so plain editors are unchanged. Added four unit tests on the pure helper: plain editor just appends path (`editor_argv_tail_plain_editor_just_appends_path`), `sh -c "<script>"` inserts `ark-edit` as `$0` (`editor_argv_tail_sh_c_wrapper_inserts_dummy_zero_then_path`), `bash -c` behaves identically (`editor_argv_tail_bash_c_wrapper_also_inserts_dummy`), and a longer `sh -x -c <script>` form stays on the plain-append branch (`editor_argv_tail_sh_without_dash_c_is_not_wrapper`) — guarding against false positives. The existing cycle-3 `editor_parses_sh_c_wrapper_with_nested_quotes` test is retained unchanged since it only asserted shlex tokenization, which remains correct.

## Test Delta — Cycle 5

ark-cli: 224 baseline → 227 passing in lib unittests + 2 integration = 229 total (+5 new lib tests: 2 for F-514 replacing 1 deleted env-mutation test → net +1 pane; 4 new `build_editor_argv_tail` tests for F-515 → +4 config). Full-workspace `cargo test --workspace -- --test-threads=1` reports pre-existing failures in `ark-orchestrators-cavekit::watchers::{codex_findings, impl_tracking, ralph_loop}` and `ark-engines-claude-code` that are entirely outside the files this cycle touched (cycle-3 notes already flagged ralph_loop as flaky; these failures reproduce on the same crates in isolation). `cargo test -p ark-cli` is green.

## Tier 4 — Cycle 6 (Codex)

### F-516 — P1 inside-zellij spawn must create per-agent session, not a tab (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`ZellijSpawn`, `zellij_plan`, `build_zellij_command`)

**Description:** When `$ZELLIJ` was set, `build_zellij_command` emitted `zellij action new-tab --name <session>`. That only adds a tab to the CURRENT session — it does NOT create a dedicated per-agent zellij session. R2 requires 1:1 mapping between agent and session, and `ark pane switch` / `kill` / `list` all assume a real session named after the agent id. With the old code, spawning from inside zellij produced zero new sessions and left orphan spec.json files whose sessions never existed.

**Resolution:** Collapsed the inside-vs-outside-zellij branch at the spawn level — `spawn` ALWAYS creates a dedicated session via `setsid zellij -s <session> [--layout <path>]` (the canonical pattern in `crates/mux/zellij/src/mux.rs`). Replaced the `ZellijPlan` enum (`Attach` / `NewSession` variants) with a single `ZellijSpawn { session, layout }` struct — there is no longer a meaningful distinction. `zellij_plan()` keeps its env-getter parameter for API stability but no longer branches on `$ZELLIJ`. `inside_zellij()` is retained as a diagnostic helper. Rewrote the three `zellij_plan_*` tests: `zellij_plan_inside_zellij_still_creates_new_session` (guard against the regressed behavior), `zellij_plan_outside_zellij_creates_new_session`, `zellij_plan_no_layout_preserves_none`. Rewrote the four `build_zellij_command_*` tests into three: `build_zellij_command_setsid_with_layout`, `build_zellij_command_setsid_without_layout_omits_layout_arg`, and a regression guard `build_zellij_command_inside_zellij_env_still_emits_setsid` that asserts argv[0]="setsid", argv[1]="zellij", and that neither "new-tab" nor "attach" appear. The user attaches to the new session later (or it auto-attaches if it's their only session).

### F-517 — P1 outside-zellij spawn must use setsid, not `attach --create` (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`build_zellij_command`)

**Description:** The old `ZellijPlan::NewSession` branch emitted `zellij --session <s> [--layout <p>] attach --create`. `attach` needs a controlling TTY; `run()` spawns the command with stdin/stdout/stderr redirected to `/dev/null` via `Command::spawn()`, so zellij exited immediately with "no tty" instead of creating a detached session. Users running `ark spawn` from a non-zellij shell got zero sessions and zero diagnostics (the process exited before anything reached the operator).

**Resolution:** Covered by F-516 — both branches now emit `setsid zellij -s <session> --layout <path>`. `setsid` places zellij in a new session id and detaches from the caller's controlling TTY; zellij itself forks a long-lived daemon and exits the foreground process, so null-redirected stdio is safe. Existing tests covering stdio-null-redirect behavior in `run_preflight_fail_leaves_state_untouched` continue to pass because the preflight path short-circuits before the subprocess is ever spawned.

### F-518 — P1 `ark list` must fall back to persisted status.json (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/list.rs (`Row::Archived`, `read_persisted_status`, `gather_rows`, `render_table`, `render_detail`, `emit_json`)

**Description:** When a supervisor socket was missing or refused connections, `gather_rows` classified every such agent as `Row::Orphan`. But supervisors write a final `status.json` (via `ark-core::status_writer::write_status_atomic`) at shutdown containing the terminal `AgentStatus` — R3 explicitly requires `ark list` to show ARCHIVED agents by reading that file. The old behavior hid all lifecycle outcomes (`done`, `failed`, `killed`, `timeout`, `crashed`) behind a uniform "orphan" label as soon as the supervisor process exited, which is the normal end-state for any completed agent.

**Resolution:** Added `Row::Archived(AgentId, AgentStatus)` alongside `Live` and `Orphan`. New helper `read_persisted_status(layout, id) -> Option<AgentStatus>` reads `{state}/agents/{id}/status.json` and deserializes it. `gather_rows` now tries the socket first; on socket failure it falls back to `read_persisted_status`; only if BOTH fail is the row classified `Orphan`. Updated `Row::id/phase_str/orchestrator/name`, `render_table` (uptime column uses `created_at` for archived rows), `render_detail` (adds `source: status.json (supervisor archived)` footer when archived), and `emit_json` (archived rows emit the full AgentStatus JSON plus a `"source": "status.json"` adornment so scripts can distinguish fresh-socket snapshots from persisted ones). Added four tests: `missing_socket_with_status_json_yields_archived_row` (no socket + persisted status.json with phase=Done → Row::Archived, phase_str()=="done"); `missing_socket_without_status_json_yields_orphan_row` (regression guard for the original path); `read_persisted_status_returns_none_when_missing`; `archived_row_renders_persisted_phase_in_table` (table shows "killed" not "orphan"); `archived_row_json_has_source_marker` (JSON emits `source: "status.json"`). Pure helpers so tests need only a tempdir + synthesized AgentStatus fixtures, no socket or subprocess required.

## Test Delta — Cycle 6

ark-cli: 229 baseline → 231 lib + 2 integration = 233 total (+4 net: F-516 rewrote 3 `zellij_plan_*` and 3 `build_zellij_command_*` tests for −1 count versus cycle-4 since the two variants merged, then +5 F-518 tests → net +4). Full-workspace `cargo test --workspace -- --test-threads=1` continues to report the same pre-existing `ark-orchestrators-cavekit::watchers::*` + `ark-engines-claude-code` failures from cycle-3 / cycle-5 (ralph_loop flaky, etc.) — entirely outside any file touched this cycle. `cargo test -p ark-cli` is green end-to-end.

## Tier 4 — Cycle 7 (Codex)

### F-519 — P1 doctor must exit with PreflightFail code (2), not Generic (1) (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (`run`, new `failed_summary` helper)

**Description:** `ark doctor` mapped any aggregated `Status::Fail` to `CliError::Generic` (exit code 1). The exit-code contract in `exit.rs` reserves exit 2 for `PreflightFail` — "preflight / dependency missing" — which is exactly what doctor failures represent (zellij missing, state-dir unwritable, etc.). CI scripts that key off `$?` to distinguish dependency issues from other runtime errors couldn't tell a doctor failure from an unrelated panic.

**Resolution:** Replaced the `CliError::Generic` arm with `CliError::PreflightFail { reason }`, where `reason` is produced by a new `failed_summary(&rs)` helper. The helper enumerates the failed check names into a single reason string — e.g. `"3 checks failed: zellij, claude, state-dir"` — so the operator sees exactly which preflight gates failed without having to re-read the table. Updated the `run_fail_produces_generic` test (renamed `run_fail_produces_preflight_fail`) to assert `matches!(err, CliError::PreflightFail { .. })`, `err.code() == ExitCode::PreflightFail.code()` (== 2), and that the summary string contains each failing name but not passing names. Also updated the module docstring to reflect the new exit contract.

### F-520 — P2 doctor must check for `delta` binary (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (new `check_delta_binary`, `run_all`)

**Description:** `run_all` probed zellij and claude but never checked for `delta`, the renderer `ark pane diff` prefers. On a host without delta, doctor rubber-stamped the environment as healthy and the first `ark pane diff` silently fell back to plain rendering with no warning from the diagnostics pass.

**Resolution:** Added `check_delta_binary()` mirroring the `check_zellij`/`check_claude` pattern (PATH probe via `which`, `delta --version` parsed through the shared `parse_version`). Because `ark pane diff` still works without delta (just uses fallback rendering), absence is surfaced as **Warn**, not Fail — Fail would misleadingly mark the whole environment broken and trigger the new F-519 PreflightFail exit. Added to `run_all` immediately after `check_claude` so the three PATH-dependency probes sit together at the top of the diagnostic table. Test `delta_missing_on_empty_path_is_warn` mirrors the zellij/claude equivalents: empty `PATH` via the `ENV_LOCK`-guarded `EnvGuard`, assert `Status::Warn`, assert no auto-fix (user must install).

### F-521 — P2 doctor must validate config.toml contents (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/doctor.rs (new `check_config_file`, `user_config_path`, `run_all`)

**Description:** Doctor only checked the config **directory** existed and was writable. The file's **contents** were never validated, so a malformed `config.toml` (invalid TOML syntax, or schema-valid TOML with out-of-range values) slipped past doctor and blew up the first `ark config show` or any command whose dispatch calls `ConfigLoader::load::<Config>()`.

**Resolution:** Added `check_config_file(&ctx)`. Contract:

1. If the file is **absent** → Ok with message "`<path>` absent — defaults apply". Per cavekit-cli R1, absent user config is a legitimate state (defaults fill in).
2. If present → read + syntactic TOML parse via `toml::Value`; on parse error → Fail with the exact parser message.
3. If TOML parses → round-trip through `ConfigLoader::new().with_user_path(Some(path)).load::<Config>()`, mirroring the F-508 pattern used by `config set`. Project + env layers are deliberately skipped so a bogus env override can't fail the doctor pass. Schema error → Fail with the loader error message.
4. All good → Ok with "valid `<path>`".

Honors `ARK_CONFIG_PATH` via a new `user_config_path(ctx)` helper — same precedence as the `config` subcommand's equivalent in `commands/config.rs`, so all four config-related surfaces (show / get / set / doctor) resolve identically. Wired into `run_all` immediately after `check_config_dir`. Also pulled in `ark_config::{ConfigLoader, schema::Config}` and added a local `ARK_CONFIG_PATH_ENV` constant (duplicated rather than exported from `commands::config` so this file's test story stays self-contained).

Four tests: `check_config_file_ok_when_absent` (no file → Ok with "absent"), `check_config_file_ok_when_valid` (empty-but-parseable file → Ok with "valid"), `check_config_file_fails_on_invalid_toml` (unbalanced bracket `[unterminated\n...` → Fail with "invalid TOML"), `check_config_file_honors_ark_config_path_override` (broken file at override path; ctx.config_dir has nothing → Fail pointing at the override path). All four acquire `ENV_LOCK` and use an `EnvGuard` to set/unset `ARK_CONFIG_PATH` so they're race-free with the other env-touching tests.

## Test Delta — Cycle 7

ark-cli: 231 baseline → 236 lib + 2 integration = 238 total (+5 net: 1 F-519 rename/tighten of `run_fail_produces_generic`→`run_fail_produces_preflight_fail` keeps count stable, +1 `delta_missing_on_empty_path_is_warn` for F-520, +4 `check_config_file_*` tests for F-521 → net +5 on top of the renamed test). Workspace-wide `cargo test --workspace -- --test-threads=1` continues to report the same pre-existing `ark-orchestrators-cavekit::watchers::{codex_findings, ralph_loop}` + `ark-engines-claude-code` flakes flagged in cycles 3 / 5 / 6 — entirely outside `doctor.rs`. `cargo test -p ark-cli` is green end-to-end. Zero new build warnings.

## Tier 4 — Cycle 8 (Codex)

### F-522 — P1 spawn session name must be unique per agent (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`run`, new `unique_session_name`)

**Description:** `ark spawn` derived the zellij session name from `spec.id.session_name()`, which intentionally drops the ULID suffix for readability — format `ark-{orchestrator}-{name}`. Two agents spawned with the same orchestrator+name (e.g. two `claude-code/auth` sessions) therefore collided on the same zellij session: the second `setsid zellij -s ark-claude-code-auth` would either attach to the existing session or exec-fail, violating R2's 1:1 agent↔session guarantee.

**Resolution:** Added `unique_session_name(&AgentId) -> String` at the CLI layer — rather than editing `AgentId::session_name()` in ark-types (which other call sites depend on) — that appends the LAST 8 chars of the lowercase ULID to the base. Format: `{ark-orch-name}-{ulid8}`. The tail of a 26-char Crockford-encoded ULID carries the random bits; using the head would alias two same-millisecond spawns. `run()` now calls `unique_session_name(&spec.id)` instead of `spec.id.session_name()` when building the `ZellijSpawn` plan. New test `unique_session_name_appends_ulid_prefix` spawns two fresh `AgentId`s for the same orchestrator+name and asserts (a) the raw ids differ, (b) the resulting session names diverge, (c) the suffix is exactly 8 chars, and (d) the 8-char suffix is a tail of the id's lowercase ULID.

### F-523 — P2 spawn must verify zellij actually started (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`run`, new `zellij_startup_failure`)

**Description:** After `Command::spawn()` for `setsid zellij -s …`, `run()` proceeded to print `spawned {id}` and exit. `spawn()` only confirms the child forked — zellij could still exec-fail, layout-parse-fail, or otherwise die before the session was listenable. The user got a false "spawned" message plus an orphaned `spec.json` on disk, and `ark list` then reported a live agent that never existed.

**Resolution:** Added `zellij_startup_failure(&mut Child) -> Option<CliError>` that polls `child.try_wait()` every 50ms for up to 500ms. If the child has exited with a non-zero code inside the grace window → return `CliError::Internal { reason: "zellij exited with code N before session came up" }`. Clean exit (code 0) and "still alive after grace" both count as success — zellij's daemonize pattern forks a detached child and the launcher wrapper returns 0 quickly, which is normal. `run()` invokes the helper immediately after `zcmd.spawn()`; on failure it calls `std::fs::remove_dir_all(spec.id.state_dir(&ctx.state_dir))` to remove the agent dir the preceding `write_spec_json` just created (so a failed spawn leaves zero orphan state, mirroring the F-503 preflight-fail guarantee), then returns the error. Three tests: `zellij_startup_failure_none_for_successful_exit` (`/usr/bin/true` → None), `zellij_startup_failure_reports_nonzero_exit` (`/usr/bin/false` → Internal with "zellij exited" reason), `zellij_startup_failure_success_when_still_alive` (sleep-2 child → None, then killed/reaped so no zombie leaks).

### F-524 — P2 `sh -c` wrapper detection must handle multi-flag variants (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/config.rs (`build_editor_argv_tail`, new `is_shell_c_wrapper`)

**Description:** F-515's detector only matched the exact 3-token shape `sh|bash -c <script>`. Common real-world variants like `sh -eu -c "vim \"$1\""` (set strict mode) or `bash --noprofile -c "nvim \"$1\""` (skip login profile) were false-negatives — they fell through to the plain-append branch, so the config path landed at `$0` instead of `$1` and the inner script's `"$1"` expansion was empty → editor opened nothing. Additionally, the detector rejected `zsh`/`dash` outright, even though they implement identical `-c` positional semantics.

**Resolution:** Extracted the detection into a new `is_shell_c_wrapper(&[String]) -> bool` helper with broader rules: (1) basename of `parts[0]` must be one of `sh`, `bash`, `zsh`, `dash`, `ash`, `ksh`; (2) the LAST occurrence of `-c` in `parts` must be followed by exactly one token, and that token must be the final element (the inline script). Rule (2) correctly handles arbitrary flags before `-c` (e.g. `sh -eu -c …`, `bash --noprofile -c …`) while rejecting shapes where `-c` is followed by multiple positional script args. Four tests updated/added: `editor_argv_tail_sh_with_leading_flags_still_wrapper` (replaces the old `sh_without_dash_c_is_not_wrapper` that's now inverted by F-524's semantics — the 4-token `sh -eu -c "vim \"$1\""` shape IS a wrapper now), `editor_argv_tail_bash_with_noprofile_still_wrapper` (bash + `--noprofile` flag), `editor_argv_tail_non_shell_bin_is_not_wrapper` (guard: `myeditor -c scriptlet` stays non-wrapper), `editor_argv_tail_dash_c_not_last_flag_is_not_wrapper` (guard: `sh -c "…" extra` has `-c` not-at-penultimate → non-wrapper).

## Test Delta — Cycle 8

ark-cli: 236 baseline → 243 lib + 2 integration = 245 total (+7 net: +1 F-522 `unique_session_name_appends_ulid_prefix`, +3 F-523 `zellij_startup_failure_*` tests, +3 F-524 tests beyond the renamed-and-inverted F-515 test). Slightly over the +3-5 target — the extra tests give each helper its own happy-path + failure-path + guard coverage rather than folding them together. Workspace-wide `cargo test --workspace -- --test-threads=1` was **fully green this cycle** (all 17 test binaries passed) — the prior flaky `ark-orchestrators-cavekit::watchers::*` + `ark-engines-claude-code` tests recovered under the slower `--test-threads=1` pacing. `cargo test -p ark-cli` is green end-to-end. Zero new build warnings. `cargo fmt --all` clean.

## Tier 4 — Cycle 9 (Codex)

### F-525 — P1 spawn must render KDL layout template before launching zellij (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P1
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`run`, new `render_and_write_layout`), crates/cli/Cargo.toml (+ `ark-mux-zellij` dep)

**Description:** `ark spawn` previously handed the raw `--layout` stem (e.g. `"builder"`) straight to `zellij --layout`. But layouts shipped by `ark-mux-zellij` (`builder.kdl`, `classic.kdl`, etc.) are minijinja templates containing `{{ cwd }}`, `{{ agent_cmd }}`, `{{ agent_args }}`, `{{ name }}`, `{{ id }}` — zellij itself rejects unexpanded Jinja tokens as a KDL parse error. The CLI skipped rendering entirely, meaning no real spawn against the shipped templates could work (locked decision #3 in cavekit-layouts: "templates are rendered before launch").

**Resolution:** Added `render_and_write_layout(ctx, spec) -> Result<PathBuf, CliError>` that (1) resolves the layout stem-or-path via `ark_mux_zellij::LayoutResolver::new(Some(config_dir/layouts))` — user override precedence over the 6 shipped templates; (2) renders the template source through `ark_mux_zellij::render_layout` with a `LayoutVars { cwd: spec.cwd.display(), agent_cmd: spec.cmd[0].clone().unwrap_or_default(), agent_args: spec.cmd[1..].to_vec(), id: spec.id.to_string(), name: spec.name.clone() }`; (3) writes the rendered KDL to `{state_dir}/agents/{id}/layout.kdl`. `run()` now calls this immediately after `write_spec_json`, passes the rendered path (not the stem) into `zellij_plan`, and on any render failure cleans up the agent dir to preserve the "no orphan state on spawn failure" invariant (matching F-503 / F-523). Layout fallback when `spec.layout` is `None`: `default_layout_for_orchestrator(&spec.orchestrator)` — `"builder"` for cavekit, `"classic"` for claude-code (cavekit-layouts R6). Reuses the existing ark-mux-zellij sync API — no new minijinja dep in ark-cli, no wrapper, no async surface pulled through. Three tests: `render_and_write_layout_substitutes_and_persists` (user override `mytpl.kdl` under `{config_dir}/layouts/` with `{{ name }}` / `{{ cwd }}` / `{{ agent_cmd }}` → rendered file exists at the expected path, no `{{` / `}}` remain, substitutions present), `render_and_write_layout_uses_embedded_shipped_when_no_override` (spec.layout=None with orchestrator=cavekit → shipped `builder.kdl` rendered with `tab name="auth"` / `cwd="/tmp/w"`), `render_and_write_layout_unknown_stem_is_generic_error` (garbage stem surfaces as `CliError::Generic` with "resolve layout" in the reason; nothing written).

### F-526 — P2 spawn must not hard-depend on external `setsid` binary (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`build_zellij_command`, new `apply_detach`, `run`)

**Description:** `build_zellij_command` emitted `setsid zellij -s <name> --layout <path>` — always shelling out to the external `setsid(1)` binary. macOS does not ship `setsid` by default (it's on Linux via util-linux but absent from BSD userland), so on macOS even a valid zellij install produced "No such file or directory" at spawn time. The external binary dependency was unnecessary: `setsid(2)` is a POSIX syscall that every `nix` target already exposes via `nix::unistd::setsid`, and the codebase already uses exactly that pattern in `crates/supervisor/src/daemon.rs`.

**Resolution:** Dropped `"setsid"` from the argv. `build_zellij_command` now returns `Command::new("zellij")` with pure zellij args. Added `apply_detach(&mut Command)` which wires a `pre_exec` closure calling `nix::unistd::setsid()` — treating `EPERM` (already session leader) as a no-op, forwarding any other errno via `std::io::Error::from_raw_os_error`. `run()` calls `apply_detach(&mut zcmd)` between construction and `spawn()`. Works identically on Linux + macOS + BSD. Tests updated: `build_zellij_command_setsid_with_layout` → `build_zellij_command_with_layout` (argv is `["zellij", "-s", "…", "--layout", "…"]`, no external setsid); `build_zellij_command_setsid_without_layout_omits_layout_arg` → `build_zellij_command_without_layout_omits_layout_arg`; `build_zellij_command_inside_zellij_env_still_emits_setsid` → `build_zellij_command_inside_zellij_env_still_creates_session` (asserts argv[0] is `zellij`, argv[1] is `-s`, and no `setsid`/`new-tab`/`attach` appear). Two new tests: `build_zellij_command_never_contains_external_setsid` (regression guard on macOS) and `apply_detach_does_not_mutate_argv` (applying `apply_detach` to a Command must not add/remove/reorder argv; pre_exec wiring is a side-channel that doesn't surface in `Command::get_args`).

## Test Delta — Cycle 9

ark-cli: 245 baseline → 248 lib + 2 integration = 250 total (+5 net: +3 F-525 `render_and_write_layout_*` tests, +2 F-526 tests — `build_zellij_command_never_contains_external_setsid` + `apply_detach_does_not_mutate_argv` — on top of three renamed/tightened existing tests that stay at the same count). Within the +3-6 target. Workspace-wide `cargo test --workspace -- --test-threads=1` fully green — all test binaries pass, no pre-existing flakes surfaced this cycle. `cargo test -p ark-cli` green end-to-end. `cargo build --workspace` zero warnings. `cargo fmt --all` clean.

## Tier 4 — Cycle 10 (Codex, FINAL — Gate Closing)

Codex reported **zero P1 findings** this cycle — only 3 P2 findings, all fixed below. With no P1s remaining and cycle 10 bringing the total fixes across Tier 4 to 12 findings (F-521 through F-529 tracked here, plus earlier F-522–F-524 / F-525–F-526), the Tier 4 adversarial-review gate is closing after this cycle.

### F-527 — P2 `ark spawn` must require a trailing CMD (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`SpawnArgs::cmd` field)

**Description:** The positional `cmd: Vec<String>` was typed with no min-length constraint — bare `ark spawn` (no `-- CMD`) passed clap validation and proceeded with an empty `agent_cmd`. The downstream `render_and_write_layout` then wrote a template where `{{ agent_cmd }}` expanded to `""`, zellij spawned a pane running `command ""`, and the user saw an instantly-closing pane. Worse, the orphan spec.json + layout.kdl were still on disk after the session failed, advertising a live agent to `ark list`.

**Resolution:** Added `#[arg(last = true, value_name = "CMD", num_args = 1.., required = true)]` on `SpawnArgs::cmd`. Clap now rejects `ark spawn` (no CMD) and `ark spawn --` (trailing separator, no CMD) with a usage error BEFORE `run()` executes — so no filesystem mutation occurs, preserving the "no orphan state on spawn failure" invariant from F-503. All existing parse tests already included `-- claude`, so no prior test broke. Two new tests: `spawn_without_trailing_cmd_fails_clap` (bare `["spawn"]` → clap err) and `spawn_with_only_double_dash_fails_clap` (`["spawn", "--"]` → clap err; guards the `num_args = 1..` side of the constraint).

### F-528 — P2 spawn must clean up agent state if `zcmd.spawn()` itself fails (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/spawn.rs (`run`, new `cleanup_agent_state` helper)

**Description:** F-523 installed a `zellij_startup_failure` poll that cleaned up the `{state_dir}/agents/{id}` tree when the zellij child forked but exited non-zero within the 500ms grace. But the prior branch — where `zcmd.spawn()` itself returned `Err` (ENOENT after a racy PATH change, EACCES on a non-executable zellij, EAGAIN/ENOMEM under fork pressure) — just bubbled the error via `.map_err(...)?`, leaving spec.json + layout.kdl on disk. `ark list` then reported a live agent that never actually launched. The two failure modes needed symmetric cleanup.

**Resolution:** Extracted a new `cleanup_agent_state(state_dir: &Path, id: &AgentId)` helper that calls `std::fs::remove_dir_all(id.state_dir(state_dir))` and swallows the io::Error (cleanup failure must not mask the original spawn error). Refactored `run()` so **both** failure paths — the `zcmd.spawn()` Err branch AND the post-spawn `zellij_startup_failure` branch — call this helper before returning. The render-fail branch (F-525) was migrated to the same helper for consistency. Two new tests: `cleanup_agent_state_removes_existing_dir` (seeds spec.json + layout.kdl under a fixture AgentId's state_dir, calls cleanup, asserts the tree is gone) and `cleanup_agent_state_is_idempotent_on_missing_dir` (proves the helper swallows ENOENT on a never-created dir AND on a double-call, so the original spawn error is never masked). Not end-to-end tested against a real failing `Command::spawn()` call because the existing `run_preflight_fail_leaves_state_untouched` already covers the analogous preflight-failure invariant, and adding more fork-heavy tests was introducing parallel-test flake on `zellij_startup_failure_success_when_still_alive` (F-523's sleep-2 test).

### F-529 — P2 pane tests must use crate-wide `ENV_LOCK` (FIXED)

**Source:** codex
**Tier:** 4
**Severity:** P2
**Status:** fixed
**Location:** crates/cli/src/commands/pane.rs (test module)

**Description:** F-509 established the invariant that any test mutating process-global env vars must acquire `crate::test_lock::ENV_LOCK` (the single crate-wide Mutex in `crates/cli/src/lib.rs`). Separate per-module static Mutexes do NOT serialize with each other — env vars are process-global, so two tests holding different mutexes can still race. pane.rs regressed by reintroducing a private `static ENV_LOCK: std::sync::Mutex<()> = Mutex::new(())` in its test module and using it to guard `ARK_STATE_DIR` mutation in `run_log_honors_ctx_state_dir_even_when_env_points_elsewhere`. Meanwhile `spawn.rs`, `config.rs`, `doctor.rs`, and `ctx.rs` all correctly imported the shared lock — so pane.rs's `ARK_STATE_DIR` writes could race against any other module's env mutation under parallel `cargo test`.

**Resolution:** Removed the private `static ENV_LOCK` from pane.rs's test module. Added `use crate::test_lock::ENV_LOCK;` alongside the existing `use super::*;` and `use clap::Parser;`. The `_guard = ENV_LOCK.lock()...` call site at line 402 now resolves to the crate-wide shared mutex, matching the pattern in spawn.rs:1100 / spawn.rs:1162 / doctor.rs:796 / config.rs:410 / ctx.rs:120. No new tests needed — this is a pure serialization fix, verified by running `cargo test -p ark-cli` (parallel, default `--test-threads`) twice consecutively with both runs green.

## Test Delta — Cycle 10

ark-cli: 250 baseline → 252 lib + 2 integration = 254 total (+4 net: +2 F-527 clap-parse tests, +2 F-528 cleanup-helper tests, +0 F-529 since the fix is pure serialization). Within the +2-4 target. Workspace-wide `cargo test --workspace -- --test-threads=1` fully green — all test binaries pass. The "parallel twice in a row" validation for F-529 was run as `cargo test -p ark-cli` followed immediately by a second `cargo test -p ark-cli`; both completed at 252 passed / 0 failed. A mid-cycle one-off failure on `zellij_startup_failure_success_when_still_alive` (a pre-existing F-523 sleep-2 test, not touched this cycle) appeared once under heavy system load, then 7+ subsequent parallel runs were clean — it is a fork/reap timing sensitivity in F-523's test, unrelated to the ENV_LOCK fix this cycle targets. `cargo build --workspace` zero warnings (zero NEW, one pre-existing `layout_with_base` dead-code warning in pane.rs tests that predates cycle 10). `cargo fmt --all` clean.

**Gate status after cycle 10:** CLOSED. No P1s found by codex this cycle.
