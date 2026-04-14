---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14"
---

# Cavekit Overview — ark

## Project
**ark** is a zellij-native agent orchestration layer written in Rust. It spawns AI coding agents (Claude Code CLI, Aider, Codex, future) into dedicated zellij sessions, each with a live diff pane, a git-status pane, and a rich claude TUI pane. Observability across all agents is surfaced via a zellij status-bar plugin and a zellij session-picker plugin. Users never open a custom TUI — zellij itself is the UI.

Pluggability is two-layer:
- **Engines** extract structured signal from agent CLIs (e.g., `ClaudeCodeEngine` injects hooks into `.claude/settings.local.json` and tails the transcript JSONL)
- **Orchestrators** encode workflow methodology (e.g., `CavekitOrchestrator` watches impl-tracking + ralph-loop + spawns codex review tabs; `ClaudeCodeOrchestrator` is methodology-free passthrough)

One engine can be used by many orchestrators. Third-party extension via a subprocess NDJSON protocol is planned (v2) but out-of-scope for v1.

## Principles
1. **Zellij-native UX** — no custom TUI wrapper. Sessions, tabs, panes, plugins.
2. **Session-per-run** — every `ark spawn` creates a new zellij session, never nests or joins existing. Users switch via zellij's session picker.
3. **Observers, not orchestrators** — engines and orchestrator adapters observe upstream tools; they never fork or rewrite them. Cavekit stays external.
4. **Filesystem-first state** — all state lives under `$XDG_STATE_HOME/ark/`. Restart-safe. `ark list` reads the directory.
5. **No daemon, ever** — ephemeral per-agent supervisors. Per-supervisor control sockets (kakoune model: one socket per `kak -s`, picker enumerates via `read_dir`, dead sockets GC'd by reachability). Picker spawns new agents by `exec`ing `ark spawn` subprocess (wezterm "connect-or-spawn" coarsened) — no shared listener, no bootstrap dead zone.
6. **Compile-in default, subprocess escape hatch later** — blessed engines + orchestrators in-binary; third-party comes in v2.
7. **XDG compliant** — state in `$XDG_STATE_HOME`, sockets in `$XDG_RUNTIME_DIR`, config in `$XDG_CONFIG_HOME`.
8. **Textual aesthetic** — `delta` for diff rendering, syntect-backed. No ratatui for wasm plugins (zellij-tile instead). Ratatui reserved for native pane commands.

## Domain Index

| Domain | Cavekit File | Requirements | Status | Description |
|--------|--------------|--------------|--------|-------------|
| Core architecture | cavekit-architecture.md | 6 | APPROVED | Two-layer Engine + Orchestrator abstraction, trait surfaces, ownership rules |
| Types, state, events | cavekit-types-state-events.md | 7 | APPROVED | AgentId, AgentSpec, AgentEvent exhaustive, state-dir XDG schema, events.jsonl |
| CLI surface | cavekit-cli.md | 8 | APPROVED | 6 top-level subcommands with arg schema, exit codes, env vars |
| Configuration | cavekit-config.md | 5 | APPROVED | Figment-layered TOML, defaults → user → project → env → flag |
| Zellij multiplexer | cavekit-mux-zellij.md | 6 | APPROVED | Session-per-run, tabs, pipe integration, `$ZELLIJ` detection |
| Claude Code engine | cavekit-engine-claude-code.md | 7 | APPROVED | Hook injection, transcript tailing, permission auto-approve, done/stall detection |
| Cavekit orchestrator | cavekit-orchestrator-cavekit.md | 9 | APPROVED | Impl-tracking, ralph-loop, site phases, review tab trigger, codex integration |
| Claude-code orchestrator | cavekit-orchestrator-claude-code.md | 3 | APPROVED | Passthrough mode, minimal observation |
| Supervisor lifecycle | cavekit-supervisor.md | 7 | APPROVED | Fork/detach, event bus wiring, control socket, crash recovery, kill semantics |
| KDL layouts | cavekit-layouts.md | 6 | APPROVED | Shipped tab KDLs, templating, user authoring |
| Pane commands (ratatui) | cavekit-pane-commands.md | 4 | APPROVED | `ark pane diff/git/log` ratatui widgets |
| Status plugin (wasm) | cavekit-plugin-status.md | 5 | APPROVED | Zellij-tile renderer, pipe ingestion, graceful degradation |
| Picker plugin (wasm) | cavekit-plugin-picker.md | 7 | APPROVED | Session-manager-alike UI, W1-W5 wireframes, fuzzy search, host control |
| Hook sidecar + IPC | cavekit-hook-ipc.md | 5 | APPROVED | `ark-hook` binary, control socket protocol for picker→host |
| Testing strategy | cavekit-testing.md | 5 | APPROVED | Contract tests, fixtures, e2e, CI matrix |
| Distribution | cavekit-distribution.md | 4 | APPROVED | cargo-dist, homebrew, install flow, wasm embedding |

## Cross-Reference Map

| Domain A | Interacts With | Interaction Type |
|----------|----------------|------------------|
| Supervisor | Engine, Orchestrator, Mux, State dir, Event bus | Owns lifecycle, dispatches traits |
| Engine (claude-code) | State dir, Hook sidecar, Event bus | Writes events from hook callbacks |
| Orchestrator (cavekit) | Engine (claude-code), Mux, State dir, Pane cmd (log) | Observes FS + consumes engine events |
| Orchestrator (claude-code) | Engine (claude-code), Mux | Pure passthrough |
| Mux (zellij) | Layouts, Plugins (status, picker) | Creates tabs from KDL, pipes events to plugins |
| Layouts | Pane commands | KDL references `ark pane diff/git/log` |
| Pane commands | State dir (log only) | `ark pane log` tails events.jsonl |
| Status plugin | Mux pipe, State dir (fallback) | Consumes progress events |
| Picker plugin | State dir, Host control socket | Reads agents, sends commands to host |
| Hook sidecar | State dir, Mux pipe | Writes hook events to per-agent JSONL + pipes to plugin |
| CLI | Supervisor, State dir, Config, Mux | Orchestrates spawn/list/kill, reads/writes state |
| Config | All consumers | Figment-layered, each component reads its section |
| Testing | All | Contract tests per trait, fixtures for engines and orchestrators |
| Distribution | All binaries + wasm | Package and ship |

## Dependency Graph
Ordered by what must be defined before what can be built:

1. **Types + state dir + event bus** — foundational; no deps
2. **Config** — depends on types (schema)
3. **Mux (zellij)** — depends on types
4. **Engine (claude-code)** — depends on types, state dir; provides the engine trait impl
5. **Supervisor** — depends on types, state, mux, engine trait; ties components together
6. **Orchestrator (claude-code)** — depends on engine trait + mux + supervisor; simplest orchestrator first
7. **Orchestrator (cavekit)** — depends on same + additional FS watchers; more complex
8. **Pane commands** — depends on types (for `log` command reading events); mostly standalone
9. **Hook sidecar** — depends on state dir; standalone binary
10. **CLI** — depends on all of the above; top-level entry point
11. **Status plugin (wasm)** — depends on types (event shape), mux pipe protocol
12. **Picker plugin (wasm)** — depends on state dir schema + host control protocol
13. **Layouts (KDL)** — depends on pane commands + plugin binaries being shipped
14. **Testing** — cross-cuts; contract tests land with each trait
15. **Distribution** — final integration; cargo-dist + wasm embedding

## v1 Scope Boundary
v1 ships:
- 1 engine: `ClaudeCodeEngine`
- 2 orchestrators: `CavekitOrchestrator`, `ClaudeCodeOrchestrator`
- 1 mux: `ZellijMux`
- 2 plugins: status bar, picker
- 3 pane commands: `ark pane diff`, `ark pane git`, `ark pane log`
- 6 CLI subcommands: `spawn`, `list`, `kill`, `doctor`, `config`, `pane`

Explicitly deferred to v2+:
- AiderEngine, CodexEngine (as first-class), CursorEngine
- RalphOrchestrator, AiderOrchestrator, ShellOrchestrator
- SubprocessOrchestrator NDJSON protocol for third-party plugins
- TmuxMux
- Agent SDK (headless, no pane) mode
- Remote agents (ssh)
- Multi-user / team features
- Windows support
