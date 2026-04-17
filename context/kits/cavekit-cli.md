---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: CLI Surface

## Scope
The `ark` binary's command-line interface — 6 user-facing top-level subcommands plus a `pane` subcommand namespace for layout composability. Argument schemas, exit codes, environment variables, `--help` content expectations.

## Requirements

### R1: Top-level command surface
**Description:** The user-facing commands, minimal and memorable.
**Acceptance Criteria:**
- [ ] Binary name: `ark`
- [ ] Top-level subcommands: `list`, `kill`, `doctor`, `config`, `pane` (bare `ark` is the default session launcher; no `spawn` subcommand)
- [ ] Global flags: `--version | -V`, `--help | -h`
- [ ] No `status` subcommand — folded into `list [ID]`
- [ ] No `logs` subcommand — folded into `ark pane log --id <ID>`
- [ ] No `gc` subcommand — folded into `ark doctor --fix`
- [ ] No `plugin install` — folded into `ark doctor --fix` (prompts user)
- [ ] No `attach` subcommand — users switch sessions via zellij's native picker
- [ ] Clap-derived (`clap` crate with derive feature)
- [ ] All subcommand output respects `$NO_COLOR` env
- [ ] `--help` text is <80 columns, groups examples per subcommand
**Dependencies:** none

### R2: ~~`ark spawn`~~ — REMOVED in v3
**Description:** Removed per scene v3 (R10). Bare `ark` with `--scene`/`--session` flags replaces `ark spawn`. See R1 for the new launch interface.

> **Archived for cross-reference.** Other kits may reference R2 by number. The original acceptance criteria described a `spawn` subcommand that created a new agent in a dedicated zellij session. That surface has been folded into bare `ark` invocation with `--scene <SCENE>`, `--session <NAME>`, `--cwd <DIR>`, `--engine <E>`, `--env KEY=VAL`, `--detach / --no-detach` flags. The orchestrator-selection flag (`--orchestrator`) is replaced by scene activation (`use "claude-code"` etc.).

**Dependencies:** (archived) cavekit-supervisor, cavekit-mux-zellij, cavekit-scene

### R3: `ark list`
**Description:** Show active and archived agents. Doubles as single-agent status.
**Acceptance Criteria:**
- [ ] Signature: `ark list [ID] [OPTIONS]`
- [ ] Options:
  - `--orchestrator <R>` — filter
  - `--status <S>` — `running | stalled | done | failed | crashed`
  - `--json` — emit JSON array (same schema as AgentStatus)
  - `--watch` — re-render every 2s; clears screen between
- [ ] Positional `ID` (optional) — if given, show that agent's detail view (single-record rich output)
- [ ] Default (no ID): table with columns `ID · NAME · ORCH · PHASE · PROGRESS · LAST`
- [ ] Detail view: spec + status + last 10 events, decorated
- [ ] `ID` accepts full AgentId, name prefix, or unique substring (with ambiguity error if multiple match)
- [ ] Reads `$STATE/agents/*/status.json`; never locks or blocks on supervisors
**Dependencies:** cavekit-types-state-events

### R4: `ark kill`
**Description:** Terminate an agent — SIGTERM supervisor, cascade-close tabs, optionally remove worktree.
**Acceptance Criteria:**
- [ ] Signature: `ark kill <ID> [--force] [--keep-worktree]`
- [ ] Default: SIGTERM supervisor; supervisor has 10s grace to close tabs, flush events, write final status
- [ ] `--force`: SIGKILL supervisor; orphan tab cleanup deferred to `ark doctor`
- [ ] `--keep-worktree`: default behavior for v1 (worktree untouched); flag reserved for future when ark manages worktrees
- [ ] Kill of a run-level id cascades to all child tab ids in that run
- [ ] Emits `Done { outcome: Killed }` to events.jsonl before exit
- [ ] Idempotent: killing an already-dead agent prints warning, returns 0
- [ ] `ID` resolution same as `ark list`
**Dependencies:** cavekit-supervisor

### R5: `ark doctor`
**Description:** Diagnose environment + optionally fix.
**Acceptance Criteria:**
- [ ] Signature: `ark doctor [--fix]`
- [ ] Checks (report each as ✓ ✗ ⚠):
  - zellij installed + version ≥ 0.44
  - delta installed (for `ark pane diff`)
  - `claude` on PATH
  - config file valid TOML; warn if absent (defaults apply)
  - state dir permissions (0700); offer to fix
  - orphan state dirs (pid dead); offer to archive
  - stale locks; offer to remove
  - status plugin installed at `~/.config/zellij/plugins/ark-status.wasm`; offer to install
  - picker plugin installed at `~/.config/zellij/plugins/ark-picker.wasm`; offer to install
  - zombie zellij sessions named `ark-*` with no state; offer to kill
- [ ] `--fix` prompts for each remediable issue (`y/n`) unless `--yes` also passed
- [ ] Exit code: 0 if all ✓, 2 if any ✗, 0 with warnings ⚠
- [ ] Plugin install = writes embedded wasm to `~/.config/zellij/plugins/`, prints KDL snippet for user's zellij config
**Dependencies:** cavekit-plugin-status, cavekit-plugin-picker, cavekit-distribution

### R6: `ark config`
**Description:** Show/edit/get/set configuration values.
**Acceptance Criteria:**
- [ ] Signature: `ark config <show|edit|get|set>`
- [ ] `ark config show` — prints effective config (after figment layering) in TOML
- [ ] `ark config edit` — opens `$EDITOR` on `$XDG_CONFIG_HOME/ark/config.toml`; creates from template if missing
- [ ] `ark config get <KEY>` — dot-path lookup (e.g., `ark config get orchestrator.cavekit.default_layout`)
- [ ] `ark config set <KEY> <VAL>` — sets in user config file, preserves comments where possible, validates value
- [ ] Validates on write; refuses invalid values with explanatory message
**Dependencies:** cavekit-config

### R7: `ark pane` namespace
**Description:** Pane composability primitives — subcommands invoked by KDL layouts.
**Acceptance Criteria:**
- [ ] `ark pane diff --cwd <DIR>` — watchexec on `.git/index` + tracked files; renders git diff via delta (ratatui wrapper)
- [ ] `ark pane git  --cwd <DIR>` — branch / staged / unstaged / untracked / last commit; ratatui widget
- [ ] `ark pane log  --id  <ID>` — tails events.jsonl, pretty-prints via ratatui
- [ ] Visible in `--help`; intended for KDL authoring + occasional shell use
- [ ] Each pane command honors `SIGWINCH`, redraws on resize
- [ ] Each exits cleanly on `q`, `Esc`, `Ctrl+C`
- [ ] Each gracefully degrades if repo/state missing (empty cwd, no events): shows placeholder text, not error
**Dependencies:** cavekit-pane-commands

### R8: Exit codes and env
**Description:** Clear exit semantics; minimal env knobs.
**Acceptance Criteria:**
- [ ] Exit codes: `0` ok, `1` generic error, `2` preflight/dep missing, `3` id not found, `4` orphan or already dead, `5` config parse error
- [ ] Env vars recognized:
  - `ARK_CONFIG_PATH` — override config file location
  - `ARK_STATE_DIR` — override state dir base
  - `ARK_RUNTIME_DIR` — override runtime dir base
  - `ARK_LOG` — tracing-subscriber env filter (e.g., `ark=debug`)
  - `NO_COLOR` — disable ANSI
- [ ] Env overrides take precedence over config, lower than CLI flags (figment layering; see cavekit-config)
**Dependencies:** cavekit-config

## Example invocations
```bash
# Typical cavekit session from a worktree
ark --scene cavekit --cwd .

# Quick passthrough (scene activates claude-code extension)
ark --cwd .

# See everything
ark list
ark list --watch
ark list myfeat

# Kill a specific agent
ark kill myfeat
ark kill myfeat --force

# Preflight
ark doctor --fix

# Config
ark config show
ark config set orchestrator.cavekit.default_layout triple-stack
ark config edit

# Pane commands (usually invoked from KDL, occasionally from shell)
ark pane diff --cwd .
ark pane log --id myfeat
```

## Out of Scope
- Shell completion generation — deferred to v1.x (easy via clap's built-in)
- `ark attach` — dropped; zellij native session picker covers it
- `ark version` subcommand — `--version` flag only
- Interactive fuzzy picker from CLI — the zellij picker plugin fills this role

## Cross-References
- cavekit-config.md — config file schema
- cavekit-supervisor.md — supervisor lifecycle around spawn/kill
- cavekit-mux-zellij.md — zellij interaction for spawn
- cavekit-pane-commands.md — `ark pane` subcommand details
- cavekit-plugin-status.md / cavekit-plugin-picker.md — doctor installs these
