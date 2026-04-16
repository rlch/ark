---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-16"
---

# Cavekit Overview — ark

## Project
**ark** is a zellij-native agent orchestration layer written in Rust. It spawns AI coding agents (Claude Code CLI, Aider, Codex, future) into dedicated zellij sessions, each with a live diff pane, a git-status pane, and a rich claude TUI pane. Observability across all agents is surfaced via a zellij status-bar plugin and a zellij session-picker plugin. Users never open a custom TUI — zellij itself is the UI.

Pluggability is **three-layer** (locked 2026-04-16):
- **Scene** — user-facing KDL config (preprocessed superset of zellij layout) declaring layout + reactions + keybinds + plugin lifecycle + extension composition. Author-side artifact. See `cavekit-scene.md`.
- **Extension protocol** — ark-native JSON-RPC 2.0 contract between ark core and running extensions. Three delivery modes share one protocol: compiled-in (workspace crate), subprocess (any language, NDJSON stdio), wasm-component (WASI p2). Replaces the earlier "v2 subprocess NDJSON" plan — extensions ship in v0.3. See `cavekit-scene.md` R10 + R16.
- **ACP** (Agent Client Protocol, external open standard, [agentclientprotocol.com](https://agentclientprotocol.com)) — editor↔coding-agent JSON-RPC. Ark is a first-class ACP client; engines are ACP agents (Claude Code, Codex, Gemini CLI speak ACP natively; aider via an adapter extension). Ark does NOT invent an agent protocol. See `cavekit-scene.md` R17.

Legacy vocabulary for reference:
- **Engines** extract structured signal from agent CLIs (e.g., `ClaudeCodeEngine` injects hooks + tails transcript). After R17 ACP adoption, the engine abstraction collapses to a launch spec: just `command` + `args` + `env`. `ClaudeCodeEngine` hook-injection + transcript-tailing retires at v0.3 (plan T-ACP.7).
- **Orchestrators** encode workflow methodology (e.g., `CavekitOrchestrator` watches impl-tracking + ralph-loop + spawns codex review tabs; `ClaudeCodeOrchestrator` is methodology-free passthrough). Orchestrator abstraction survives; scene reactions are additive.

## Principles
1. **Zellij-native UX** — no custom TUI wrapper. Sessions, tabs, panes, plugins.
2. **Session-per-run** — every `ark spawn` creates a new zellij session, never nests or joins existing. Users switch via zellij's session picker.
3. **Observers, not orchestrators** — engines and orchestrator adapters observe upstream tools; they never fork or rewrite them. Cavekit stays external.
4. **Filesystem-first state** — all state lives under `$XDG_STATE_HOME/ark/`. Restart-safe. `ark list` reads the directory.
5. **No daemon, ever** — ephemeral per-agent supervisors. Per-supervisor control sockets (kakoune model: one socket per `kak -s`, picker enumerates via `read_dir`, dead sockets GC'd by reachability). Picker spawns new agents by `exec`ing `ark spawn` subprocess (wezterm "connect-or-spawn" coarsened) — no shared listener, no bootstrap dead zone.
6. **Compile-in default, subprocess escape hatch later** — blessed engines + orchestrators in-binary; third-party comes in v2.
7. **XDG compliant** — state in `$XDG_STATE_HOME`, sockets in `$XDG_RUNTIME_DIR`, config in `$XDG_CONFIG_HOME`.
8. **Textual aesthetic** — `delta` for diff rendering, syntect-backed. No ratatui for wasm plugins (zellij-tile instead). Ratatui reserved for native pane commands.
9. **Concrete over trait-with-one-impl** — when ark wraps a single external tool (e.g. zellij), the wrapper is a concrete type, not a trait. Test seams come from (a) pure functions returning data (`Vec<MuxOp>`-style command-bus), (b) stubbed command executors at the subprocess boundary, or (c) relocating consumers to a crate where the concrete type is already reachable. Traits-for-mocking with a single production impl are explicitly rejected — see `cavekit-testing.md` R1 for rationale (matklad "Concrete Abstraction," sans-IO).

## Domain Index

| Domain | Cavekit File | Requirements | Status | Description |
|--------|--------------|--------------|--------|-------------|
| Core architecture | cavekit-architecture.md | 6 | APPROVED | Two-layer Engine + Orchestrator abstraction, trait surfaces, ownership rules |
| Types, state, events | cavekit-types-state-events.md | 7 | APPROVED | AgentId, AgentSpec, AgentEvent exhaustive, state-dir XDG schema, events.jsonl |
| CLI surface | cavekit-cli.md | 8 | APPROVED | 6 top-level subcommands with arg schema, exit codes, env vars |
| Configuration | cavekit-config.md | 5 | APPROVED | Figment-layered TOML, defaults → user → project → env → flag |
| Zellij multiplexer | cavekit-mux-zellij.md | 6 | APPROVED | Session-per-run, tabs, pipe integration, `$ZELLIJ` detection |
| Claude Code engine | cavekit-engine-claude-code.md | 7 | LEGACY | Hook injection, transcript tailing, permission auto-approve, done/stall detection. Retires at v0.3 per scene R17 (engines become ACP launch specs). |
| Cavekit orchestrator | cavekit-orchestrator-cavekit.md | 9 | APPROVED | Impl-tracking, ralph-loop, site phases, review tab trigger, codex integration |
| Claude-code orchestrator | cavekit-orchestrator-claude-code.md | 3 | APPROVED | Passthrough mode, minimal observation |
| Supervisor lifecycle | cavekit-supervisor.md | 7 | APPROVED | Fork/detach, event bus wiring, control socket, crash recovery, kill semantics |
| KDL layouts | cavekit-layouts.md | 6 | APPROVED | Shipped tab KDLs, templating, user authoring |
| Pane commands (ratatui) | cavekit-pane-commands.md | 4 | APPROVED | `ark pane diff/git/log` ratatui widgets |
| Status plugin (wasm) | cavekit-plugin-status.md | 5 | APPROVED | Zellij-tile renderer, pipe ingestion, graceful degradation. v0.1: shipped wasm plugin (inline in default scene). v0.3: ported to ark-native extension; default scene `use`s it. |
| Picker plugin (wasm) | cavekit-plugin-picker.md | 7 | APPROVED | Session-manager-alike UI, W1-W5 wireframes, fuzzy search, host control. v0.3: ported to ark-native extension; also renders ACP permission-request modals (scene R17 5-tier fallback). |
| Hook sidecar + IPC | cavekit-hook-ipc.md | 5 | APPROVED | `ark-hook` binary, control socket protocol for picker→host. Expanded for scene ark-bus: `ark-hook intent` + `ark-hook emit` subcommands route keybind/event dispatch through the existing socket. |
| Testing strategy | cavekit-testing.md | 5 | APPROVED | Contract tests, fixtures, e2e, CI matrix |
| Distribution | cavekit-distribution.md | 4 | APPROVED | cargo-dist, homebrew, install flow, wasm embedding. Ark ships its own zellij — the release tarballs, brew formula, and binstall payload each carry a pinned zellij binary alongside `ark`, so `$PATH` lookup is only a dev-mode fallback (`ARK_USE_SYSTEM_ZELLIJ=1` opts into the system copy). Pins `agent-client-protocol` crate version. |
| Scene (reactive KDL + ACP) | cavekit-scene.md | 17 (R1-R17) | DRAFT → APPROVED pending T-11.2+ and T-12.4+ | KDL 2.0 superset of zellij layout; reactions, keybinds, plugin lifecycle, extension composition via `use`, ACP client integration (engines as ACP agents). CEL predicates + minijinja templates. Nvim-class extensibility. Flips to APPROVED once R1-R15 are fully implemented (R14 hot-reload tiers T-11.2 through T-11.8 + R13 CLI tiers T-12.4 through T-12.11 remain). R16-R17 tracked separately under the Extension entry below + the ACP client row. |
| Extension protocol (runtime RPC) | cavekit-scene.md R10 + R16 | 2 (R10 + R16) | DRAFT | ark-native JSON-RPC 2.0 contract between ark core and running extensions. Three delivery modes share one protocol: compiled-in (workspace crate, `register_extension!` macro), subprocess (any language, NDJSON stdio), wasm-component (WASI p2). Picker + status will port from inline-plugins to ark-native extensions at v0.3. Replaces the earlier "v2 subprocess NDJSON" plan — extensions ship in v0.3. |

## Cross-Reference Map

| Domain A | Interacts With | Interaction Type |
|----------|----------------|------------------|
| Scene | Supervisor, Mux, Event bus, Intent registry, Extension protocol, ACP client, Hook-IPC (via ark-bus) | Compiled at spawn; registers reactions + keybinds + plugin lifecycle + intent registry; gates hot-reload on ACP turn state |
| Extension protocol | Scene (R10, R16), Supervisor (supervision tree), Event bus | ark↔ext JSON-RPC 2.0; bidirectional; three delivery modes share one protocol |
| ACP client | Supervisor, Event bus, Scene reactions, Picker plugin | Per-engine-session JSON-RPC; surfaces `session/update` as `UserEvent:ark.acp.*`; permission dispatch routes via Zed 5-tier precedence |
| Supervisor | Scene, Engine→ACP client, Orchestrator, Mux, State dir, Event bus | Owns lifecycle, compiles scene, spawns ACP client, tracks turn-inflight per session |
| Engine (claude-code, LEGACY) | State dir, Hook sidecar, Event bus | Writes events from hook callbacks. Retires at v0.3 — replaced by ACP launch spec. |
| Orchestrator (cavekit) | Engine/ACP, Mux, State dir, Pane cmd (log) | Observes FS + consumes engine events |
| Orchestrator (claude-code) | Engine/ACP, Mux | Pure passthrough |
| Mux (zellij) | Layouts (rendered from scene), Plugins (status, picker, ark-bus) | Creates tabs from KDL, pipes events to plugins |
| Layouts | Pane commands | KDL references `ark pane diff/git/log` (scene's `layout { }` block passes through) |
| Pane commands | State dir (log only) | `ark pane log` tails events.jsonl |
| Status plugin | Mux pipe, State dir (fallback) | Consumes progress events. v0.3: ark-native extension. |
| Picker plugin | State dir, Host control socket, ACP permission requests | Reads agents, sends commands to host, renders ACP permission modals (5-tier fallback). v0.3: ark-native extension. |
| Hook sidecar | State dir, Mux pipe, Scene intent registry | Writes hook events to per-agent JSONL + pipes to plugin. v0.1 scene: adds `ark-hook intent` + `ark-hook emit` for ark-bus dispatch. |
| CLI | Supervisor, State dir, Config, Mux, Scene, Extensions | Orchestrates spawn/list/kill + `ark scene *` + `ark ext *` subcommands |
| Config | All consumers; `config.toml` at `$XDG_CONFIG_HOME/ark/` | Figment-layered, each component reads its section; `[acp]`, `[scene]`, `[engines]` sections added by scene |
| Testing | All | Contract tests per trait, fixtures for engines and orchestrators |
| Distribution | All binaries + wasm + bundled zellij + pinned `agent-client-protocol` crate | Package and ship |

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

v1 ships in milestones — see `plans/build-site-scene.md` for tier-by-tier coverage.

- **v0.1 — Scene**: scene grammar, reactions, keybinds, intent registry, ark-bus plugin, inline shipped plugins (picker + status). No extensions. No ACP (uses legacy engine-claude-code hooks).
- **v0.2 — Composition**: `extends`, `include`, scene-search-path.
- **v0.3 — Extensions + ACP**: ark extension protocol (compiled-in / subprocess / wasm-component), ACP client, `use`, wasm metadata. Shipped ACP engines: `claude`, `codex`, `gemini-cli`. Picker + status ported to ark-native extensions. Legacy `ClaudeCodeEngine` hook-injection retires.
- **v0.4 — Declared capabilities**: capability declarations in `ExtensionMetadata`, install-time disclosure.
- **v0.5 — Hot reload + package mgr + trust**: `reload_scene`, file-watcher, `ark ext add github:…`, publisher-trust prompt.
- **v1.0 — Freeze**: `ark.core.*` intents frozen; extension protocol v1 locked for 1.x.

Zellij integration: `ZellijMux` (concrete type, no mux trait). Ark ships its own zellij (see cavekit-distribution).
2 orchestrators: `CavekitOrchestrator`, `ClaudeCodeOrchestrator`.
3 pane commands: `ark pane diff`, `ark pane git`, `ark pane log`.
CLI subcommands: `spawn`, `list`, `kill`, `doctor`, `config`, `pane`, `scene {check|fmt|dry-run|graph|explain|reload}`, `ext {add|remove|list|update|info|inspect}`.

Explicitly deferred to v2+:
- AiderEngine adapter extension, non-ACP engines (as first-class), CursorEngine
- RalphOrchestrator, AiderOrchestrator, ShellOrchestrator
- Agent SDK (headless, no pane) mode
- Remote agents (ssh)
- Multi-user / team features
- Windows support
- User-defined CEL functions, Rhai/Lua scripting
- Chord sequences (vim-style `<leader>ff`)
- Multi-version same-ext loading
- Intents-as-ACP-tools (ark does not expose its intent surface as callable tools to the agent; revisit post-v1)
