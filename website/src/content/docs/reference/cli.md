---
title: CLI Reference
description: All ark subcommands, flags, environment variables, and exit codes
---

## `ark` (bare)

Launch a session. Running `ark` with no subcommand starts the default session; flags pick a scene and session name.

```
ark [--scene <NAME_OR_PATH>] [--session <NAME>]
```

| Flag | Description | Default |
|------|-------------|---------|
| `--scene <NAME_OR_PATH>` | Scene to launch. A bare name resolves to `$ARK_CONFIG_DIR/scenes/<name>.kdl`; a path containing `/` or `.kdl` is used verbatim. | Built-in default scene |
| `--session <NAME>` | Zellij session name (attach-or-create). Inside zellij (`$ZELLIJ` set) switches to the named session; outside, creates a new one. | Derived by the supervisor |

Both flags are global and propagate to subcommands.

Examples:

```sh
ark                               # launch default session
ark --scene myproject             # launch with a named scene
ark --scene ./scenes/dev.kdl      # launch with a scene at a path
ark --session work                # attach-or-create the `work` session
ark --scene myproject --session work
```

## `ark list`

Show active agents.

```
ark list [ID] [OPTIONS]
```

| Flag | Description |
|------|-------------|
| `--orchestrator <NAME>` | Filter by orchestrator |
| `--status <S>` | Filter by phase: `starting`, `running`, `idle`, `prompting`, `reviewing`, `done`, `failed`, `crashed`, `killed`, `timeout` |
| `--json` | Emit JSON (array in list mode, object in detail mode) |
| `--watch` | Re-render every 2s |

Without `ID`: table view (ID prefix, name, orchestrator, phase, uptime). With `ID`: detail view (spec fields + phase + last event).

## `ark kill`

Terminate an agent.

```
ark kill <ID> [--force] [--keep-worktree]
```

| Flag | Description |
|------|-------------|
| `--force` | SIGKILL instead of SIGTERM (orphan cleanup via `ark doctor`) |
| `--keep-worktree` | Explicitly preserve the worktree (redundant with the default) |

Default: SIGTERM with a 10s grace window. Worktrees are preserved unless you opt in elsewhere.

## `ark doctor`

Diagnose environment.

```
ark doctor [--fix] [--yes]
```

Checks: zellij, delta, claude on PATH, config validity, plugin installation, orphan state, stale locks.

`--fix` prompts for each fixable issue. `--yes` auto-approves all fixes.

## `ark config` subcommands

| Command | Description |
|---------|-------------|
| `ark config show` | Print the effective config (after layering) as TOML |
| `ark config edit` | Open `$EDITOR` on the user config file |
| `ark config get <KEY>` | Print one value by dot-path |
| `ark config set <KEY> <VAL>` | Set one value by dot-path (writes to the user config) |

## `ark scene` subcommands

| Command | Description |
|---------|-------------|
| `ark scene check <FILE>` | Parse and validate without applying |
| `ark scene fmt <FILE>` | Format scene KDL in place |
| `ark scene dry-run <FILE>` | Simulate event flow through reactions |
| `ark scene graph <FILE>` | Print dependency graph (panes, extensions, reactions) |
| `ark scene explain <FILE>` | Human-readable summary of what the scene does |
| `ark scene explain-merge <A> <B>` | Show how two scenes would merge |
| `ark scene reload --session <ID>` | Hot-reload the active session's scene via the supervisor control socket |
| `ark scene schema-dump [--format <json\|text>]` | Dump the scene KDL schema (default: json) |

> **Note:** CLI subcommand names use hyphens (`dry-run`, `explain-merge`, `schema-dump`). The underlying Rust function names use underscores, but the CLI surface always uses hyphens.

## `ark ext` subcommands

| Command | Description |
|---------|-------------|
| `ark ext add <NAME\|PATH>` | Install an extension |
| `ark ext remove <NAME>` | Uninstall an extension |
| `ark ext update [NAME]` | Update one or all extensions |
| `ark ext list` | Show registered extensions |
| `ark ext info <NAME>` | Extension metadata detail |
| `ark ext inspect <PATH>` | Pre-install artifact inspection (wasm metadata) |
| `ark ext trust <NAME>` | Capability approval |

## `ark pane` subcommands

| Command | Description |
|---------|-------------|
| `ark pane diff [--cwd <DIR>]` | Live delta-rendered diff of uncommitted changes |
| `ark pane git [--cwd <DIR>]` | Live git status of the working tree |
| `ark pane log --id <ID>` | Stream events for an agent |

## Environment variables

| Variable | Description |
|----------|-------------|
| `ARK_USE_SYSTEM_ZELLIJ` | Set to `1` to use system zellij instead of the pinned binary |
| `ARK_HANDLE` | Set on every pane process — the pane's `@handle` value |
| `ARK_CONFIG_PATH` | Overrides the user config file location |
| `XDG_STATE_HOME` | State directory root (default: `~/.local/state`) |
| `XDG_CONFIG_HOME` | Config directory root (default: `~/.config`) |
| `XDG_RUNTIME_DIR` | Runtime directory for sockets + rendered layouts |
| `NO_COLOR` | Disables colored output when set |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General error |
| `2` | Environment issue (doctor found problems) |
