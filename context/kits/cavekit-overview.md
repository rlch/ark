---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-18"
note: "Scope cut 2026-04-18 (revised same day) — ACP + cavekit orchestrator deleted outright. Claude Code migrates to extensions/claude-code/ as v0.1's first engine integration via hook-based observability (not ACP). Pi deferred to v0.2. See context/plans/handoff-2026-04-18-claude-code-first-pivot.md."
---

# Cavekit Overview — ark

## Project
**ark** is a **terminal IDE** written in Rust. It uses zellij as its rendering backend, providing extensible sessions with layout, reactions, keybinds, and views. AI coding agents are one extension capability — ark is not an AI terminal; it is a terminal IDE that supports AI via extensions.

Pluggability is **two-layer** (locked 2026-04-16, converged v3; typed handles + stack added 2026-04-18):
- **Scene** — user-facing KDL config declaring layout + reactions + keybinds + extension activation + composition. Ark owns the layout DSL (row/col/stack/span/handle/mode/when=); zellij is a compile target, not a vocabulary source. See `cavekit-scene.md`.
- **Extension** — bundles providing views, intents, events. Three delivery modes share one protocol: compiled-in (workspace crate), subprocess (any language, NDJSON unix socket), zellij-wasm (pane rendering). See `cavekit-scene.md` R10 + R16.

v0.1 engine story: **claude-code is the first integrated engine** (`cavekit-claude-code.md`) via hook-based observability, shipping in `extensions/claude-code/`. Pi (`cavekit-pi.md`) is DEFERRED to v0.2. Cavekit orchestration and ACP have been removed as ark concerns. Future engines integrate via the same extension pattern.

## Principles
1. **Zellij-native UX** — no custom TUI wrapper. Sessions, tabs, panes, stacks.
2. **Session-per-run** — every `ark` invocation creates a new zellij session, never nests or joins existing. Users switch via zellij's session picker.
3. **Extensions own AI** — engines (pi, future) live in `extensions/`, never in core. See `cavekit-soul.md`.
4. **Filesystem-first state** — all state lives under `$XDG_STATE_HOME/ark/`. Restart-safe. `ark list` reads the directory.
5. **No daemon, ever** — ephemeral per-session supervisors. Per-supervisor control sockets (kakoune model: one socket per session, picker enumerates via `read_dir`, dead sockets GC'd by reachability).
6. **Compile-in default, subprocess escape hatch later** — blessed extensions in-binary; third-party subprocess comes post-v0.1.
7. **XDG compliant** — state in `$XDG_STATE_HOME`, sockets in `$XDG_RUNTIME_DIR`, config in `$XDG_CONFIG_HOME`.
8. **Textual aesthetic** — `delta` for diff rendering, syntect-backed. No ratatui for wasm plugins (zellij-tile instead). Ratatui reserved for native pane commands.
9. **Concrete over trait-with-one-impl** — when ark wraps a single external tool (e.g. zellij), the wrapper is a concrete type, not a trait. Test seams come from (a) pure functions returning data, (b) stubbed command executors at the subprocess boundary, or (c) relocating consumers to a crate where the concrete type is already reachable. See `cavekit-testing.md` R1 (matklad "Concrete Abstraction," sans-IO).

## Domain Index

| Domain | Cavekit File | Requirements | Status | Description |
|--------|--------------|--------------|--------|-------------|
| Ark's soul (supersedes architecture) | cavekit-soul.md | — (narrative) | READY | Reactive IDE on zellij; extensions own AI. Supersedes cavekit-architecture.md. Drives Phase 1-6 migration (types/supervisor/ACP-delete/claude+cavekit-delete/trait-delete/picker). |
| Types, state, events | cavekit-types-state-events.md | 7 | APPROVED | SessionSpec/SessionId/SessionStatus + CoreEvent (shrunk AgentEvent, soul-era). state-dir XDG schema, events.jsonl. |
| CLI surface | cavekit-cli.md | 8 | APPROVED | Top-level subcommands with arg schema, exit codes, env vars. Soul Phase 1 drops --orchestrator / PHASE_NAMES / orchestrator+engine columns. |
| Configuration | cavekit-config.md | 5 | APPROVED | Figment-layered TOML, defaults → user → project → env → flag. Soul Phase 4 deletes agent-specific sections. |
| Zellij multiplexer | cavekit-mux-zellij.md | 6 | APPROVED | Session-per-run, tabs, pipe integration, `$ZELLIJ` detection |
| Supervisor lifecycle | cavekit-supervisor.md | 7 | APPROVED | Fork/detach, event bus wiring, control socket, crash recovery, kill semantics. Soul Phase 1 rewrites the main loop; Phase 5 drops Engine/Orchestrator factory calls. |
| KDL layouts | cavekit-layouts.md | 6 | APPROVED | Shipped tab KDLs, templating, user authoring |
| Pane commands (ratatui) | cavekit-pane-commands.md | 4 | APPROVED | `ark pane diff/git/log` ratatui widgets |
| Status plugin (wasm) | cavekit-plugin-status.md | 5 | APPROVED | Zellij-tile renderer, pipe ingestion, graceful degradation. v0.1: shipped wasm plugin (inline in default scene). Port to ark-native ext tracked via scene R17. |
| Picker plugin (wasm) | cavekit-plugin-picker.md | 7 | APPROVED | Session-manager-alike UI, W1-W5 wireframes, fuzzy search, host control. Port to ark-native ext tracked via scene R17. |
| Testing strategy | cavekit-testing.md | 5 | APPROVED | Contract tests, fixtures, e2e, CI matrix |
| Distribution | cavekit-distribution.md | 4 | APPROVED | cargo-dist, homebrew, install flow, wasm embedding. Ark ships its own zellij — release tarballs + brew formula + binstall payload carry a pinned zellij binary (`ARK_USE_SYSTEM_ZELLIJ=1` opts into system copy). |
| claude-code integration | cavekit-claude-code.md | 13 (R1-R13) + R5b | DRAFT | Single-crate extension: `claude-code` CommandView (typed `Stack<ClaudeCodeSubagent>` handle) + `claude-code-subagent` stack-child view + `cc-hook` write-only hook bridge (NDJSON over per-session socket) + transcript fs-watcher + settings.json installer + doctor + list columns. Claude Code's TUI handles permissions natively; no reverse channel, no policy engine. MCP-server control surface deferred. v0.1's first engine integration. Depends on soul Phase 2 + scene 2026-04-18 typed-handle + stack revision. |
| pi integration (deferred) | cavekit-pi.md | 22 (R1-R22) + R6b | DEFERRED (v0.2) | Three-crate extension family: pi-core (TS bridge + `pi` CommandView with typed `Stack<PiSubagentTile>` / `Pane<PiSubagentLog>` / `Pane<PiSceneProposeView>` attrs), pi-subagents (fs watcher + tile+log views + `pi.subagent.focus` intent), pi-control (IntentRegistry exposed as pi LLM tools via `ark_dispatch(kdl)` + `ark_ops()` + `ark_scene_propose(diff)` routed through a `pi-scene-propose` view). Deferred to v0.2; ships after claude-code proves the ext-hook surface + typed-handle revision. All R1-R22 content preserved. |
| Scene + Extensions (v3) | cavekit-scene.md | 17 (R1-R17) | CONVERGED | v3 redesign: ark-native layout DSL (row/col/stack/span/@handle/mode/when=), views replace plugins, typed view-parametric handles `Pane<V>`+`Stack<V>` (2026-04-18 revision), Rhai expression-only mode (CEL + minijinja both dead, 2026-04-16), extensions unified (3 delivery modes: compiled-in/subprocess/zellij-wasm), reconciler via override-layout, composition via include-only, code-generated manifest via Rust derives. See cavekit-scene.md changelogs 2026-04-18 + 2026-04-16. Build site needs regeneration (`/ck:map`). |

## Cross-Reference Map

| Domain A | Interacts With | Interaction Type |
|----------|----------------|------------------|
| Scene + Extensions | Supervisor, Mux, Event bus, Intent registry, Extension protocol | Compiled at launch; registers reactions + keybinds + views; reconciles layout via override-layout; gates hot-reload on ACP turn state |
| Extension protocol | Scene (R10, R16), Supervisor, Event bus | ark↔ext JSON-RPC 2.0; three delivery modes (compiled-in, subprocess, zellij-wasm); views determined by trait impl (CommandView/ZellijView) |
| ACP (extension capability) | Extensions, Event bus, Scene reactions | Extension declares `agent { speaks "acp" }`; events surfaced as `ark.acp.*`; no separate `agent { }` block |
| Supervisor | Scene, Extensions, Mux, State dir, Event bus | Owns session lifecycle, compiles scene, dispatches extension hooks (soul Phase 2) |
| Scene | Supervisor, Mux, Event bus, IntentRegistry, Extension protocol | Compiled at launch; registers reactions/keybinds/views; reconciles via override-layout |
| Mux (zellij) | Layouts (rendered from scene) | Renders tabs/panes/stacks; reconciler drives it |
| Layouts | Pane commands, Views | Rendered from scene; env-wrap panes with `ARK_HANDLE` |
| Pane commands | State dir (log only) | `ark pane log` tails events.jsonl |
| Status plugin | Mux pipe, Event bus | Consumes progress events. Ported to ark-native ext per scene R17. |
| Picker plugin | State dir, Host control socket | Reads sessions, sends commands to host. Ported to ark-native ext per scene R17. |
| CLI | Supervisor, State dir, Config, Mux, Scene, Extensions | Orchestrates launch/list/kill + `ark scene *` + `ark ext *` subcommands |
| Config | All consumers; `config.toml` at `$XDG_CONFIG_HOME/ark/` | Figment-layered; extensions register their own config sections (soul Phase 4) |
| Extensions (claude-code) | Supervisor, Scene, Mux, IntentRegistry, Event bus | v0.1 first-class engine integration via ext-hook surface + Claude Code's native hook system. See cavekit-claude-code.md. |
| Extensions (pi family, deferred) | Supervisor, Scene, Mux, IntentRegistry, Event bus | v0.2 second engine integration. See cavekit-pi.md. |
| Testing | All | Contract tests per trait, mock-claude + mock-pi fixtures per per-engine kit R13 / R22 |
| Distribution | All binaries + wasm + bundled zellij | Package and ship |

## Dependency Graph
Ordered by what must be defined before what can be built:

1. **Types + state dir + event bus** — foundational. Soul Phase 1 replaces AgentSpec/Status/Id with Session-prefixed + `CoreEvent::Ext(ExtEvent)`.
2. **Config** — depends on types (schema). Soul Phase 4 drops agent-specific sections; extensions register their own.
3. **Mux (zellij)** — depends on types.
4. **Supervisor** — depends on types, state, mux; main loop is `world.cancel.cancelled().await` once soul Phase 1 lands.
5. **Scene + Extensions (core framework)** — depends on supervisor; ext-hook surface lands in soul Phase 2.
6. **Pane commands** — depends on types; standalone.
7. **CLI** — depends on all above; top-level entry point. Soul Phase 1 drops --orchestrator / PHASE_NAMES; columns contributed by extensions via Phase 2 hooks.
8. **Status plugin / Picker plugin** — depend on mux pipe + event bus (status) or state dir + host control (picker).
9. **Layouts (KDL)** — depend on pane commands + plugin binaries being shipped.
10. **claude-code extension** — depends on Phase 2 ext-hook surface. v0.1's first engine integration; tier order (R2/R13 → R1/R4 → R3 → R5 → R6/R8 → R7 → R10/R11 → R9/R12) per cavekit-claude-code.md.
11. **pi extension family (deferred v0.2)** — same dependency; ships after claude-code validates the pattern.
11. **Testing** — cross-cuts; contract tests land with each trait.
12. **Distribution** — final integration; cargo-dist + wasm embedding.

## v1 Scope Boundary

v0.1 ships after the 2026-04-18 pivot (claude-code first, pi deferred).

- **v0.1 — Bare ark + scene + claude-code**:
  - Soul Phase 1: types migration, bare `ark` launch works, PTY test green.
  - Soul Phase 2: ext-hook surface (`on_session_start/end`, `scene_compile_hook`, `register_intents`, `doctor_checks`, `list_columns`, `control_verbs`).
  - Soul Phase 3: ACP deleted.
  - Soul Phase 4 (revised): `crates/orchestrators/claude-code/` + `crates/hook/` + `crates/types/src/permission.rs` deleted as ark crates; salvaged content rehomed into `extensions/claude-code/`. `crates/orchestrators/cavekit/` deleted outright (no rehoming).
  - Soul Phase 5: `Engine` / `Orchestrator` traits deleted from core.
  - Scene v3 + 2026-04-18 typed-handle + stack revision.
  - claude-code extension per cavekit-claude-code.md (R1-R13 + R5b).
- **v0.2 — pi integration + MCP control surface**:
  - pi extension family per cavekit-pi.md (on ice since 2026-04-18).
  - MCP server (`ark-mcp` binary) exposing `IntentRegistry` as tools — paired with pi-control R13-R18 and claude-code stretch work.
- **Post-v0.2**: third-party subprocess ext protocol polish, hot-reload completeness, additional engines as extensions.

Zellij integration: `ZellijMux` (concrete type, no mux trait). Ark ships its own zellij (see cavekit-distribution).
3 pane commands: `ark pane diff`, `ark pane git`, `ark pane log`.
CLI subcommands: `list`, `kill`, `doctor`, `config`, `pane`, `scene {check|fmt|dry-run|graph|explain|reload}`, `ext {add|remove|list|update|info|inspect}` (bare `ark` is the default session launcher).

Explicitly deleted or deferred:
- Claude Code *as a core ark engine* (deleted 2026-04-18). **Reinstated same day** as `extensions/claude-code/` — see cavekit-claude-code.md.
- Cavekit orchestrator (deleted 2026-04-18; Cavekit methodology stays external, no extension rehoming).
- ACP client (deleted 2026-04-18).
- `ark-hook` *as an ark crate* (deleted 2026-04-18). **Salvaged** into `extensions/claude-code/bin/cc-hook/`.
- Pi extension family (DEFERRED to v0.2; cavekit-pi.md preserved).
- Aider, Cursor, Gemini, Codex adapters — come as extensions after v0.1 if demand.
- Agent SDK (headless, no pane) mode.
- Remote agents (ssh).
- Multi-user / team features.
- Windows support.
- User-defined helper functions beyond the bundled set.
- Chord sequences (vim-style `<leader>ff`).
- Multi-version same-ext loading.
