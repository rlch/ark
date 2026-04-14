---
created: "2026-04-14"
last_edited: "2026-04-14"
---
# Dead Ends & Deferred Findings

Build site: context/plans/build-site.md

Track failed approaches and deliberately-deferred codex findings so future iterations don't re-litigate them.

## Deferred Codex Findings (Tier 2 gate)

### F-058 / F-061 — P1 Command Injection in hook_dispatcher (deferred by user 2026-04-14)

**Location:** crates/ark-core/src/consumers/hook_dispatcher.rs (approx L88, HookEntry::render → sh -c)

**Finding:** HookEntry::render interpolates untrusted AgentEvent fields (tool name, file path, agent name, permission decision) into a command string that is then executed via `sh -c`. A malicious filename or tool name containing shell metacharacters (`;`, `$()`, backticks, etc.) would execute arbitrary shell syntax inside the configured hook.

**Sweep plan:** Tier 3 hook_dispatcher revisit. Two viable fixes:
1. Replace `sh -c <template>` execution path with direct argv execution from ark-config's already-parsed `cmd_argv` form (HookEntry already parses both shell-style `cmd` and argv-style `cmd_argv`; switch the default to argv, emit deprecation warning for `cmd` templates that reference event vars).
2. Shell-quote every interpolated value before substitution (shlex::quote).

**Why deferred:** Non-trivial design call (drop shell-style templates entirely vs keep + escape); attack surface is local-only (hook commands are user-authored in local config), but still a latent P1.

### F-059 / F-062 — P1 Data Loss in restore_settings (deferred by user 2026-04-14)

**Location:** crates/ark-engines-claude-code/src/settings.rs (approx L250, restore_settings)

**Finding:** `restore_settings()` deletes `settings.local.json` when no `.ark-backup` exists. If EngineHandle::teardown runs on a worktree with a user-managed `.claude/settings.local.json` that was never backed up (edge case: inject failed mid-write, or the user manually removed the backup), the user's config is silently deleted.

**Sweep plan:** Simple fix — teardown's "no backup" path should preserve the live file. Only restore when backup exists. No-op otherwise. Write a regression test that exercises `inject → manually remove backup → teardown → confirm live file intact`.

**Why deferred:** Small fix, low probability in happy path, but real data-loss scenario. Will address in Tier 3 supervisor wiring when EngineHandle::teardown is actually invoked by supervisor.

### F-060 / F-063 — P2 Missing PermissionAsked on stdin-error + empty-stdin (deferred by user 2026-04-14)

**Location:** crates/ark-hook/src/run.rs (approx L121, fail-open stdin branches)

**Finding:** Regression from F-053 fix. The stdin-read-error and empty-stdin fail-open branches now emit PermissionResolved but skip PermissionAsked. Breaks the "always emit the trace pair" invariant that F-053 itself introduced.

**Sweep plan:** At each fail-open branch that constructs a synthetic Resolved, also synthesize an Asked with tool="unknown" first. Consolidate into a single helper `emit_permission_pair_synthetic(reason: &str, stdout, ...)` so both events fire in lock-step.

**Why deferred:** Minor, paired with F-058/F-059 sweep in Tier 3.
