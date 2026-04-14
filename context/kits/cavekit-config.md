---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Configuration

## Scope
Unified configuration system using `figment`. Layered sources (defaults → user → project → env → flags), full TOML schema for v1, validation, editor workflow.

## Requirements

### R1: Layering precedence
**Description:** Config resolved by merging sources in a deterministic order.
**Acceptance Criteria:**
- [ ] Sources, lowest-to-highest precedence:
  1. Compiled-in defaults (`Config::defaults()`)
  2. User config: `$XDG_CONFIG_HOME/ark/config.toml` (if present)
  3. Project config: `./.ark/config.toml` (if present in cwd at spawn time)
  4. Env vars: `ARK_*` keys (see R5)
  5. CLI flags (highest)
- [ ] Higher layers only override keys they set; unset keys fall through
- [ ] Arrays (e.g., `[[hooks]]`) are concatenated across layers, not replaced
- [ ] Figment `Figment::new().merge(...).merge(...).merge(...)` pattern
- [ ] Missing files silently skipped; malformed files error out with clear line/column
**Dependencies:** cavekit-types-state-events (for typed schema)

### R2: TOML schema (top-level)
**Description:** The canonical structure of `config.toml`.
**Acceptance Criteria:**
- [ ] Sections:
  - `[defaults]` — cross-cutting defaults
  - `[diff]` — diff pane rendering
  - `[engine.claude_code]` — engine-specific
  - `[orchestrator.cavekit]` — orchestrator-specific
  - `[orchestrator.claude_code]` — orchestrator-specific
  - `[mux.zellij]` — multiplexer-specific
  - `[[hooks]]` — repeatable hook definitions
- [ ] Every section's keys deserialize into a typed struct via serde
- [ ] Unknown keys rejected with warning (not silent drop) — typos surface early
**Dependencies:** R1

### R3: Default values
**Description:** Exact shipped defaults (used when user provides no config).
**Acceptance Criteria:**
- [ ] `[defaults]`:
  - `orchestrator = "auto"`
  - `engine = "claude-code"`
  - `session_prefix = "ark"`
  - `auto_close_on_done = true`
  - `auto_close_on_fail = false`
  - `auto_close_on_kill = true`
  - `stall_timeout_secs = 120`
- [ ] `[diff]`:
  - `command = "delta --paging=never --side-by-side --line-numbers"`
  - `debounce_ms = 300`
- [ ] `[engine.claude_code]`:
  - `transcript_tail = true`
  - `permission_policy = "auto_approve_all"`  # configurable; values: `ask | auto_approve_read | auto_approve_all`
  - `hook_transport = "state_file"`  # v1 fixed; `socket` reserved
  - `inject_hooks = ["PostToolUse", "Stop", "PermissionRequest", "Notification", "TaskCompleted", "SessionEnd"]`
- [ ] `[orchestrator.cavekit]`:
  - `watch_ralph_loop = true`
  - `watch_impl_tracking = true`
  - `spawn_review_tab = true`
  - `default_layout = "builder"`
  - `review_layout = "review"`
  - `review_on_phase = "check"`
- [ ] `[orchestrator.claude_code]`:
  - `default_layout = "classic"`
- [ ] `[mux.zellij]`:
  - `status_plugin_path = "~/.config/zellij/plugins/ark-status.wasm"`
  - `picker_plugin_path = "~/.config/zellij/plugins/ark-picker.wasm"`
  - `default_layout_dir = "~/.config/ark/layouts"`
- [ ] `[[hooks]]` default: empty (users opt in; commented example ships in template config)
**Dependencies:** R2

### R4: Hooks
**Description:** User-defined commands fired on AgentEvent matches.
**Acceptance Criteria:**
- [ ] Each `[[hooks]]` table entry:
  - `event` (required) — event kind slug (e.g., `done`, `stall`, `review_comment`)
  - `cmd` (required) — array: argv to exec
  - `match` (optional) — TOML table with additional filters (e.g., `{ severity = "P0" }` on `review_comment`)
- [ ] Hook cmd invoked via `tokio::process::Command`, inherits no env unless `env` table set
- [ ] Cmd args support `{{name}}` template substitution for fields present on the event (e.g., `{{id}}`, `{{name}}`, `{{outcome}}`)
- [ ] Hooks run async, don't block supervisor; failures logged but non-fatal
- [ ] Shipped template config contains at least 2 commented examples: `notify-send` on `done`, `say` on `stall`
**Dependencies:** cavekit-types-state-events, R3

### R5: Env variable mapping
**Description:** `ARK_*` env vars that override config keys.
**Acceptance Criteria:**
- [ ] Mapping pattern: nested keys flatten with double-underscore (e.g., `ARK_DEFAULTS__AUTO_CLOSE_ON_DONE=false`)
- [ ] Array values unsupported via env (single scalars only)
- [ ] Special-case shortcuts for common toggles:
  - `ARK_ORCHESTRATOR` → `defaults.orchestrator`
  - `ARK_ENGINE` → `defaults.engine`
  - `ARK_LOG` — tracing filter (not a config key; consumed by logging subsystem)
  - `ARK_CONFIG_PATH`, `ARK_STATE_DIR`, `ARK_RUNTIME_DIR` — path overrides (not a config key; consumed by path resolver)
- [ ] Documented in `ark config show --help` and README
**Dependencies:** R1

## Shipped template config (doctor offers to install)
```toml
# ark config — https://github.com/rlch/ark
# Layered: defaults → this file → ./ark/config.toml → ARK_* env → CLI flags

[defaults]
orchestrator      = "auto"
engine            = "claude-code"
auto_close_on_done = true
auto_close_on_fail = false
stall_timeout_secs = 120

[diff]
command     = "delta --paging=never --side-by-side --line-numbers"
debounce_ms = 300

[engine.claude_code]
transcript_tail   = true
permission_policy = "auto_approve_all"   # ask | auto_approve_read | auto_approve_all
inject_hooks = [
    "PostToolUse", "Stop", "PermissionRequest",
    "Notification", "TaskCompleted", "SessionEnd",
]

[orchestrator.cavekit]
watch_ralph_loop    = true
watch_impl_tracking = true
spawn_review_tab    = true
default_layout      = "builder"
review_layout       = "review"

[orchestrator.claude_code]
default_layout = "classic"

[mux.zellij]
status_plugin_path = "~/.config/zellij/plugins/ark-status.wasm"
picker_plugin_path = "~/.config/zellij/plugins/ark-picker.wasm"

# Hooks fire on AgentEvent kinds. Uncomment to enable.
# [[hooks]]
# event = "done"
# cmd   = ["notify-send", "ark: {{name}} done"]

# [[hooks]]
# event = "stall"
# cmd   = ["say", "agent stalled"]

# [[hooks]]
# event = "review_comment"
# match = { severity = "P0" }
# cmd   = ["notify-send", "ark: P0 finding on {{name}}"]
```

## Out of Scope
- Encrypted or remote config
- Live reload (config picks up changes only on next spawn/list invocation)
- Per-orchestrator config files (everything lives in one TOML)
- Schema versioning / migrations — breaking changes bump major version, migration docs ship separately

## Cross-References
- cavekit-cli.md — `ark config` subcommand
- cavekit-engine-claude-code.md — consumes `[engine.claude_code]`
- cavekit-orchestrator-cavekit.md — consumes `[orchestrator.cavekit]`
- cavekit-orchestrator-claude-code.md — consumes `[orchestrator.claude_code]`
- cavekit-mux-zellij.md — consumes `[mux.zellij]`
