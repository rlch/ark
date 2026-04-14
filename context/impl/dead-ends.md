---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Dead Ends & Deferred Findings

Build site: context/plans/build-site.md

Track failed approaches and deliberately-deferred codex findings so future iterations don't re-litigate them.

## Resolved Deferred Codex Findings (Tier 2 gate)

All three Tier 2 deferred findings were swept in one commit ("codex fixes F-058 F-059 F-060 (Tier 2 deferred)"). Kept here as a historical record — do NOT reopen.

### F-058 / F-061 — P1 Command Injection in hook_dispatcher (RESOLVED)

**Location:** crates/core/src/consumers/hook_dispatcher.rs, crates/config/src/hooks.rs

**Finding (original):** HookEntry::render interpolated untrusted AgentEvent fields into a command template executed via `sh -c`. Shell metacharacters (`;`, `$()`, backticks, `&&`) in a filename or tool name executed as separate shell syntax.

**Fix shipped:**
- Added `cmd_argv: Vec<String>` field to `HookEntry` (T-021 schema extension). When populated, the dispatcher splits it and `Command::new(argv[0]).args(&argv[1..])` — no shell involved, injection impossible by construction.
- Legacy `cmd` (shell-string) path: every interpolated `{{var}}` value is now run through `shlex::try_quote` before substitution. `a; rm -rf /tmp/evil` renders as `'a; rm -rf /tmp/evil'` — sh -c treats it as one argument, no command-break.
- `cmd_argv` wins when both are set.
- One-shot tracing warning fires the first time a `cmd` (shell) entry with `{{var}}` interpolation runs, recommending `cmd_argv`.
- Added `shlex = "1"` as a workspace dep.

**Tests (12 new):** `f058_*` in crates/config/src/hooks.rs (render-level unit tests: semicolons, `$()`, backticks, `&&`, safe alphanumeric, NUL byte fallback, argv substitution, cmd_argv precedence, TOML parsing) + 6 `f058_*` in crates/core/src/consumers/hook_dispatcher.rs (integration: argv direct-exec preserves metachar filename, cmd form's canary survives `; rm` injection).

### F-059 / F-062 — P1 Data Loss in restore_settings (RESOLVED)

**Location:** crates/engines/claude-code/src/settings.rs

**Finding (original):** `restore_settings()` deleted `settings.local.json` when no `.ark-backup` existed. If teardown ran on a worktree with a user-managed config that had never been backed up, the user's data was silently removed.

**Fix shipped:**
- Behavior matrix:
  - Backup exists → restore from backup + remove backup (unchanged, correct).
  - Backup absent + live file exists → log `warn!` "no backup to restore from; leaving live settings.local.json untouched" and return Ok(()). **DO NOT DELETE.**
  - Backup absent + live file absent → Ok (no-op).
- Updated the existing `restore_round_trip_with_no_pre_existing_settings` test that had encoded the wrong (buggy) contract — it now asserts the live file is preserved.

**Tests (3 new):** `f059_restore_preserves_live_file_when_backup_deleted_mid_session`, `f059_restore_preserves_user_only_file_when_no_backup_or_inject`, `f059_double_restore_is_noop_on_second_call`.

### F-060 / F-063 — P2 Missing PermissionAsked pair on fail-open branches (RESOLVED)

**Location:** crates/hook/src/run.rs

**Finding (original):** F-053 fix added `PermissionResolved` to the stdin-read-error and empty-stdin fail-open branches but forgot to also synthesize `PermissionAsked`. Broke the "always emit the pair" invariant F-053 itself enforced.

**Fix shipped:**
- New helper `emit_permission_pair_synthetic(id, state_root, tool, summary, decision, reason)` that fires `PermissionAsked` then `PermissionResolved` in JSONL order, both with `tool="unknown"` and the same tool string for correlation.
- Wired into the stdin-read-error and empty-stdin branches (with reasons `"stdin-read-error"` / `"empty-stdin"` for observability).
- Malformed-JSON branch already had Asked from the F-053 work — added a regression test to lock it.

**Tests (6 new):** `f060_stdin_read_error_emits_asked_then_resolved_pair`, `f060_empty_stdin_emits_asked_then_resolved_pair`, `f060_whitespace_only_stdin_emits_asked_then_resolved_pair`, `f060_malformed_json_still_emits_asked_then_resolved_pair`, `f060_valid_payload_still_emits_asked_then_resolved_pair`, `f060_ordering_in_stdin_read_error_branch`.
