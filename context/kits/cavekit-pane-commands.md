---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Pane Commands (ratatui)

## Scope
The `ark pane` subcommand namespace. Native Rust binaries that run inside zellij panes, render UI via ratatui, watch filesystem or event streams, redraw on change. Invoked by KDL layouts (primary use) and occasionally by users from the shell.

## Design principle
Each pane command is a small ratatui app. No shared global state; each is a separate process launched by zellij. Processes exit on `q`, `Esc`, or `Ctrl+C`; zellij closes the pane when the process exits.

## Requirements

### R1: `ark pane diff`
**Description:** Live-refreshing git diff display using `delta` for rendering.
**Acceptance Criteria:**
- [ ] Signature: `ark pane diff --cwd <DIR> [--command <CMD>] [--debounce-ms <N>]`
- [ ] Watches `<DIR>/.git/index` and all tracked files (via `notify` recursive watch on cwd)
- [ ] On change (debounced per `config.diff.debounce_ms`, default 300ms):
  - Runs `git --git-dir {cwd}/.git --work-tree {cwd} diff --color=always`
  - Pipes through `delta --paging=never --side-by-side --line-numbers` (or `config.diff.command`)
  - Writes captured stdout into a scroll buffer
- [ ] ratatui widget: `Paragraph` with styled text, supports up/down/pgup/pgdn scroll
- [ ] On empty diff: placeholder text `Waiting for first edit…`
- [ ] On non-repo cwd: `Not a git repository` placeholder, still runs (pane doesn't crash)
- [ ] Quit: `q`, `Esc`, or `Ctrl+C`
- [ ] SIGWINCH redraw
**Dependencies:** cavekit-config

### R2: `ark pane git`
**Description:** Git status summary — branch, tracking, staged, unstaged, untracked, last commit.
**Acceptance Criteria:**
- [ ] Signature: `ark pane git --cwd <DIR> [--poll-ms <N>]`
- [ ] Watches `<DIR>/.git/index`, `<DIR>/.git/HEAD`, `<DIR>/.git/refs/` + 2s fallback poll
- [ ] Runs `git status --porcelain=v2 --branch` + `git log -1 --format=%h|%s|%cr`
- [ ] Renders ratatui layout:
  - Top line: `branch {name}  ↑{ahead} ↓{behind}`
  - Sections: `staged (N)`, `unstaged (N)`, `untracked (N)`; each with file listing (`M path  +adds -dels` for staged/unstaged, `? path` for untracked)
  - Bottom: `last {hash}  {subject} ({time-ago})`
- [ ] Truncates long file lists with a `(N more)` indicator; up/down scroll
- [ ] Non-repo cwd: `Not a git repository` placeholder
- [ ] Quit: `q`, `Esc`, `Ctrl+C`
- [ ] SIGWINCH redraw
**Dependencies:** cavekit-config

### R3: `ark pane log`
**Description:** Tail `events.jsonl` for a given agent ID, pretty-print via ratatui.
**Acceptance Criteria:**
- [ ] Signature: `ark pane log --id <ID> [--filter <KIND>]`
- [ ] Opens `$STATE/agents/{id}/events.jsonl`, seeks to end, follows via inotify
- [ ] Each line parsed as `{ts, event}`; renders as `HH:MM:SS  KIND  summary`
- [ ] Color-coded by event kind (ToolUse → cyan, TaskDone → green, Stall → yellow, Error → red, etc.)
- [ ] Filter: `--filter tool_use,task_done` hides other kinds
- [ ] Scroll: up/down, end/home; auto-scroll follows tail unless user scrolls up (follow resumes on end key)
- [ ] Missing agent dir: `Agent '{id}' not found` placeholder, exits with 3 after 2s
- [ ] Quit: `q`, `Esc`, `Ctrl+C`
- [ ] SIGWINCH redraw
**Dependencies:** cavekit-types-state-events

### R4: Shared conventions
**Description:** Consistency across pane commands.
**Acceptance Criteria:**
- [ ] All pane commands run with `NO_COLOR` honored (color stripped if env set)
- [ ] All use `tokio` runtime + `crossterm` backend for ratatui
- [ ] All support `Ctrl+C` graceful shutdown (restore terminal, no corruption)
- [ ] All log errors via `tracing` to stderr (captured by zellij pane or shell)
- [ ] Shared chrome: 1-line bottom status bar showing `q quit · j/k scroll · {pane-specific hints}`
- [ ] All are standalone binaries (subcommands of `ark`) — no daemon required; state dir / cwd is the only input
**Dependencies:** none

## Implementation notes
- Use `ratatui = "0.29"` (or latest stable) + `crossterm = "0.29"` backend.
- For `diff` pane: invoking `delta` via `tokio::process::Command` + capture stdout; rendering its ANSI output in ratatui requires converting ANSI to ratatui's `Text` — use `ansi-to-tui` crate.
- For `git` pane: parse `git status --porcelain=v2` with a small hand-rolled parser (format is stable).
- For `log` pane: parse JSONL line-by-line; skip malformed with a warning line in the buffer.

## Out of Scope
- Tree-sitter syntax highlighting inside diff pane — deferred to v2 (Path B from earlier research); delta's syntect is good enough for v1
- Editor integration (jump from pane to editor on Enter) — deferred
- Interactive staging / hunk actions inside git pane — pane is read-only
- Multi-agent log aggregation in one pane — one `--id` per invocation

## Cross-References
- cavekit-cli.md R7 — `ark pane` subcommand surface
- cavekit-layouts.md — layouts invoke these via KDL
- cavekit-config.md — `[diff]` section
- cavekit-types-state-events.md — events.jsonl format consumed by `log`
