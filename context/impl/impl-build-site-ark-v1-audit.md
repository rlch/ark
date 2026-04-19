---
created: "2026-04-18"
last_edited: "2026-04-18"
audit_of: "context/plans/build-site.md"
head_commit: "b7ee714+"
---

# ark v1 Audit — 2026-04-18

Build site: `context/plans/build-site.md` (135 tasks, 7 tiers, dated 2026-04-14).

The site pre-dates the 2026-04-18 pivot and was originally scored 134/134
DONE pre-gate per `impl-overview.md` row. Many of those DONE tasks were
subsequently deleted, replaced, or scope-cut by later build sites
(phase-2 soul, claude-code-ext, cleanup Packets A+B, scene-2026-04-18,
supervisor-wiring, mux-tight-coupling). This audit re-evaluates every
row against the live tree at head `b7ee714+` and classifies each task
as **DONE**, **SUPERSEDED**, **PARTIAL**, **CUT**, or **PENDING**.

Workspace state: 2358 tests pass / 4 ignored / 0 fail. `cargo fmt` clean.
PTY smoke green. v0.1 tag-eligible.

## Scope-cut context (2026-04-18 pivot)

The following systems were deleted from v0.1 scope:

- **`ark-hook` binary** — `crates/hook/` deleted wholesale per cleanup
  T-005 (commit `df7206f`). `ark` has no `hook` subcommand; the
  claude-code extension ships its own `cc-hook` binary under
  `extensions/claude-code/bin/cc-hook/` which replaces the old shared
  hook surface. `ark-hook`-binstall shim in `crates/cli/Cargo.toml`
  gone per T-002 (`e2fffcd`).
- **`ark-orchestrators-cavekit` + `ark-orchestrators-claude-code` crates** —
  both deleted per cleanup T-003 and T-004 (commit `df7206f`); the
  `crates/orchestrators/*` workspace glob was dropped from the root
  Cargo.toml.
- **`Engine` / `Orchestrator` trait + `EngineHandle` + `ApprovalPolicy`** —
  entire trait surface deleted per cleanup T-010 (commit `75ec431`):
  `crates/core/src/{engine,engine_contract,orchestrator,orchestrator_contract}.rs`
  + `crates/supervisor/src/engine_stub.rs` (1226 LOC total).
  `run_supervisor_with` signature cut from 10 args to 7 (T-009, `6975f58`).
  Future engines ship as extensions, not as trait impls.
- **`factory.rs`** (supervisor) — deleted whole per cleanup T-008
  (commit `de73dc6`); `build_multiplexer` inlined; `build_engine` /
  `build_orchestrator` dead-code-eliminated.
- **`Multiplexer` trait** — old core-side trait deleted per mux-tight-coupling
  M-7/M-8 (pre-2026-04-18). `ZellijMux` is now concrete. A tiny test-
  local `Multiplexer` trait remains in
  `crates/cli/src/commands/launch/traits.rs` as a launch-internal
  testing seam (out-of-scope per mux-tight-coupling M-5 audit).
- **`crates/types/src/permission.rs`** — deleted per cleanup T-006;
  `PermissionPolicy` / `PolicyDecision` / `READ_ONLY_TOOLS` /
  `POLICY_FILE_NAME` grep-clean.
- **`ark spawn` CLI verb** — removed per the claude-code-first pivot.
  `cli.rs` test `help should not list 'spawn'` at line 245 enforces
  its absence. Launch flow is now `bare ark` → `supervisor_handoff` →
  `launch/real.rs::spawn_and_wait_for_ready` (internal, not user-visible).
- **ACP** — not in scope this audit (see scene-v3 audit T-101..T-109).
  Relevant here only because cavekit-orchestrator watchers were
  replaced by the scene reaction_dispatcher + extensions model.

## SUPERSEDED-by-post-pivot context

- **Engine pieces (T-052..T-058)** — folded into `extensions/claude-code/`
  under the ArkExtension protocol model. `settings.local.json` injection,
  transcript tailer, permission policy, stall watcher, preflight are all
  present but now live inside the claude-code ext, not inside a core
  `ClaudeCodeEngine` trait impl.
- **Orchestrator pieces (T-073..T-083)** — dissolved. `ClaudeCodeOrchestrator`
  + `CavekitOrchestrator` were simple tab-open-and-wait workflows. Post
  pivot: scene-v3 drives tab/pane lifecycle; reactions + `ark.core.exec`
  ops run the event-triggered workflow; claude-code extension provides
  transcript/hook surface. Build-site fixture `cavekit-project/` is
  preserved in `crates/test-fixtures/` as test input, but there is no
  `CavekitOrchestrator` runtime.
- **Consumer tasks (T-059/T-060/T-061)** — state_writer + status_pipe
  landed in `crates/core/src/consumers/` and `crates/supervisor/src/consumers/`
  respectively. The `hook_dispatcher` consumer was DELETED per cleanup
  T-5.7 (documented in `crates/config/src/hooks.rs:25-29`): legacy
  `[[hooks]]` entries are now compiled by
  `ark_scene::hook_compat::build_hook_registry` into synthetic scene
  reactions; the unified `reaction_dispatcher` runs them via
  `ark.core.exec`. The `[[hooks]]` config schema survives as a legacy
  surface, but dispatch is via scene reactions.
- **ark-hook sidecar (T-046..T-051)** — replaced by `cc-hook` binary in
  `extensions/claude-code/bin/cc-hook/main.rs`. JSONL write +
  PermissionRequest allow payload + malformed-stdin fail-open live there.
  The JSON envelope + AgentEvent translation moved into
  `extensions/claude-code/src/hook_event.rs` + `hook_payload.rs`.
- **AgentEvent renamed to CoreEvent** (phase-1). 17-variant enum still
  exists; `UserEvent` niche filled by `CoreEvent::Ext(ExtEvent{…})`.

## Summary

| Status | Count | % |
|--------|------:|---|
| DONE | 91 | 67.4% |
| SUPERSEDED | 23 | 17.0% |
| PARTIAL | 1 | 0.7% |
| CUT | 20 | 14.8% |
| PENDING | 0 | 0.0% |
| **TOTAL** | **135** | 100% |

## Task-by-task

### Tier 0 — Foundations

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-001 | Workspace scaffold 12 crates | DONE | pre-pivot | workspace has 20+ crates now (expanded for phase-2 ext-proto/ark-view/ext-derive/scene-macros); all original T-001 crates present except deleted ones. |
| T-002 | Pin workspace deps | DONE | pre-pivot | Cargo.toml has tokio/serde/clap/figment/nix/ratatui/notify/tracing/interprocess/signal-hook/minijinja/nucleo-matcher all pinned. |
| T-003 | AgentId ULID + session-name + state-dir | DONE | pre-pivot | `crates/types/src/id.rs` + `types/Cargo.toml` depends on ulid. |
| T-004 | AgentSpec + OrchestratorSpec alias | DONE | pre-pivot | `crates/types/src/spec.rs`. OrchestratorSpec alias retained as legacy shape. |
| T-005 | AgentEvent enum (17 variants) | SUPERSEDED | phase-1 rename | Renamed to `CoreEvent`; 17 variants preserved. `crates/types/src/event.rs`. |
| T-006 | AgentStatus + Phase + Findings | DONE | pre-pivot | `crates/types/src/status.rs`. |
| T-007 | TabHandle + CancellationToken re-export | SUPERSEDED | phase-2 T-008..T-013 | `TabHandle` now comes from `ark_view::TabHandle` (`crates/ark-view/src/typed.rs`), not a core small-types module. |
| T-008 | State-dir schema 0700 + XDG | DONE | pre-pivot | `crates/types/src/state_dir.rs`. |
| T-009 | events.jsonl append writer | DONE | pre-pivot | `crates/core/src/events_log.rs`. |
| T-010 | Atomic status.json writer | DONE | pre-pivot | `crates/core/src/status_writer.rs`. |
| T-011 | EventSink broadcast factory | DONE | pre-pivot | `crates/types/src/event_bus.rs`. |
| T-012 | ARK_* path resolver + runtime_dir | DONE | pre-pivot | `crates/types/src/env_paths.rs`. |
| T-013 | Engine trait + EngineHandle + ApprovalPolicy | CUT | cleanup T-010 (`75ec431`) | `crates/core/src/engine.rs` deleted. Future engines ship as extensions. |
| T-014 | Orchestrator trait + Outcome wiring | CUT | cleanup T-010 (`75ec431`) | `crates/core/src/orchestrator.rs` deleted. |
| T-015 | World struct | CUT | cleanup T-010 (`75ec431`) | Last surviving callsite collapsed to a bare `cancel.cancelled().await` in `orchestration.rs`. |
| T-016 | Multiplexer trait | CUT | mux-tight-coupling M-7/M-8 | Core-side `Multiplexer` trait gone. `ZellijMux` is concrete. Tiny test-local seam in `launch/traits.rs` is out-of-scope per the mux-tight-coupling audit. |
| T-017 | v1 scope-lock constants | SUPERSEDED | pivot | The "allowed engine/orchestrator slugs" concept is void — there are no engines/orchestrators anymore. `MUX_V1 = ["zellij"]` is now documented as an inline comment in `orchestration.rs`. |

### Tier 1 — Config, Mux, Layouts, Pane, Control-socket primitives

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-018 | Figment layering | DONE | pre-pivot | `crates/config/src/lib.rs` figment_defaults → user → project → env → flags. |
| T-019 | TOML schema structs + unknown-key reject | DONE | pre-pivot | `crates/config/src/schema.rs`. |
| T-020 | Config::defaults() all shipped values | DONE | pre-pivot | `schema.rs:119-150` area. |
| T-021 | Hook entry parsing + `{{var}}` + match filters | DONE | pre-pivot | `crates/config/src/hooks.rs`. |
| T-022 | ARK_* env var mapping | DONE | pre-pivot | figment env provider plumbed. |
| T-023 | Template config.toml ships | DONE | pre-pivot | `crates/config/templates/config.toml`. |
| T-024 | Stub command executor + real tokio::process | DONE | pre-pivot | `crates/mux/zellij/src/executor.rs`. |
| T-025 | ZellijMux::ensure_session | DONE | pre-pivot | `crates/mux/zellij/src/mux.rs` — `switch-session` without `--create`, setsid outside, ULID collision handling. |
| T-026 | ZellijMux::create_tab | DONE | pre-pivot | `mux.rs` new-tab + first-tab short-circuit + TabHandle. |
| T-027 | ZellijMux::close_tab + rename_tab | DONE | pre-pivot | `mux.rs` idempotent close + rename. |
| T-028 | ZellijMux::pipe | DONE | pre-pivot | `mux.rs` pipe to ark-status + ark-picker. |
| T-029 | Layout stem resolver | DONE | pre-pivot | `crates/mux/zellij/src/layout_resolver.rs`. |
| T-030 | minijinja template renderer | DONE | pre-pivot | `crates/mux/zellij/src/layout_template.rs` — strict UndefinedBehavior + KDL validation. (Note: scene-v3 moved expression language to Rhai, but mux layout templates remain minijinja — legacy layout flow still ships.) |
| T-031 | Rendered-KDL writer + `.kdl` extension | DONE | pre-pivot | `crates/mux/zellij/src/layout_writer.rs`. |
| T-032 | ZellijMux preflight ≥ 0.44.1 | DONE | pre-pivot | Preflight in `mux.rs`. |
| T-033 | Ship builder.kdl | DONE | pre-pivot | `crates/mux/zellij/layouts/builder.kdl`. |
| T-034 | Ship classic.kdl | DONE | pre-pivot | `crates/mux/zellij/layouts/classic.kdl`. |
| T-035 | Ship focused/triple-column/review/log | DONE | pre-pivot | All 4 present in `layouts/`. |
| T-036 | Default-layout resolution + --layout | SUPERSEDED | scene-v3 | `--layout` flag superseded by `--scene`. Default layout per orchestrator no longer a concept (no orchestrators). `crates/mux/zellij/scenes/` shows scene-wrapped layouts. |
| T-037 | `ark layouts list` diagnostic | CUT | pivot | No `layouts` subcommand in `Commands` enum (`crates/cli/src/commands/mod.rs:38`). Replaced by `ark scene` family. |
| T-038 | User-authored layout validation + doctor hook + docs/layouts.md | DONE | pre-pivot | `crates/mux/zellij/docs/layouts.md` present; doctor has KDL snippet printing. |
| T-039 | Pane cmd shared chrome | DONE | pre-pivot | `crates/pane/src/app.rs` + `tracing_init.rs`. |
| T-040 | `ark pane diff` | DONE | pre-pivot | `crates/pane/src/diff.rs`. |
| T-041 | `ark pane git` | DONE | pre-pivot | `crates/pane/src/git.rs`. |
| T-042 | `ark pane log` | DONE | pre-pivot | `crates/pane/src/log.rs`. |
| T-043 | Control-socket primitive NDJSON | DONE | pre-pivot | `crates/core/src/control_socket.rs` — `interprocess::local_socket::Listener` + Tokio. |
| T-044 | Stale-socket GC (50ms connect → unlink) | DONE | pre-pivot | in `control_socket.rs` + picker bootstrap. |
| T-045 | Agents-socket-dir helper | DONE | pre-pivot | `crates/core/src/socket_paths.rs`. |

### Tier 2 — ark-hook sidecar, Engine, Event-bus consumers

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-046 | ark-hook skeleton | SUPERSEDED | cleanup T-005 → claude-code-ext | `crates/hook/` deleted (df7206f). Replaced by `extensions/claude-code/bin/cc-hook/main.rs`. |
| T-047 | ark-hook payload parser | SUPERSEDED | claude-code-ext | Logic moved to `extensions/claude-code/src/hook_payload.rs` + `hook_event.rs`. |
| T-048 | ark-hook per-event JSONL writer | SUPERSEDED | claude-code-ext | `extensions/claude-code/bin/cc-hook/main.rs` handles the JSONL append. |
| T-049 | ark-hook zellij-pipe forwarding | SUPERSEDED | claude-code-ext + scene ark-bus | Pipe forwarding now lives in ark-bus + scene emit flow. |
| T-050 | ark-hook PermissionRequest auto-approve | SUPERSEDED | claude-code-ext | TUI-owned permission flow per memory `project_claude_code_extension`. Auto-approve path preserved inside the extension, not via a hook binary contract. |
| T-051 | ark-hook fail-open | SUPERSEDED | claude-code-ext | `extensions/claude-code/tests/cc_hook_fail_open.rs` covers it. |
| T-052 | ClaudeCodeEngine settings injection | SUPERSEDED | claude-code-ext | `extensions/claude-code/src/settings_json.rs` — deep-merge + `.ark-backup` + idempotent. Not via a core `Engine` trait — extension owns it. |
| T-053 | Engine transcript tailer | SUPERSEDED | claude-code-ext | `extensions/claude-code/src/transcript.rs`. |
| T-054 | Permission policy enforcement (ask / auto_approve_*) | SUPERSEDED | claude-code-ext | Permission flow is TUI-owned; extension manages decisions without the deleted `ApprovalPolicy` enum. |
| T-055 | Engine Done/SessionEnd detection | SUPERSEDED | claude-code-ext | Surfaced via hook events and `CoreEvent::Ext` in the extension. |
| T-056 | Engine stall watcher | SUPERSEDED | claude-code-ext | Stall tracking folded into extension event flow; no core-side Engine trait. |
| T-057 | EngineHandle + JoinSet teardown | CUT | cleanup T-010 | EngineHandle struct deleted; extensions own their own lifecycle via `ark-ext-proto::supervision`. |
| T-058 | Engine preflight (claude on PATH, ~/.claude, cwd writable, ark-hook discoverable) | SUPERSEDED | claude-code-ext | `extensions/claude-code/src/doctor.rs`. `ark doctor` checks claude binary presence. |
| T-059 | state_writer consumer | DONE | pre-pivot | `crates/core/src/consumers/state_writer.rs`. |
| T-060 | status_pipe consumer | DONE | pre-pivot | `crates/supervisor/src/consumers/status_pipe.rs`. |
| T-061 | hook_dispatcher consumer | SUPERSEDED | scene reaction_dispatcher (T-5.7) | Deleted as a standalone consumer. `[[hooks]]` entries are compiled by `ark_scene::hook_compat::build_hook_registry` into synthetic reactions run by `reaction_dispatcher` via `ark.core.exec`. Documented in `crates/config/src/hooks.rs:25-29`. |

### Tier 3 — Supervisor lifecycle + control socket + Orchestrators

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-062 | Supervisor fork + setsid double-fork daemonize | DONE | supervisor-wiring W-3 | `crates/supervisor/src/daemon.rs` + `bootstrap.rs`. |
| T-063 | Supervisor --no-detach foreground | DONE | supervisor-wiring W-4 | `crates/supervisor/src/foreground.rs`. |
| T-064 | File lock `$STATE/locks/{id}.lock` | DONE | pre-pivot | `crates/supervisor/src/lock.rs`. |
| T-065 | Control-socket bind (step 3) | DONE | pre-pivot | `crates/supervisor/src/control_socket.rs` — bind post-setsid/StateDir/lock; fatal on failure. |
| T-066 | Per-supervisor socket command handlers | DONE | pre-pivot | `crates/supervisor/src/commands.rs`. |
| T-067 | Socket cleanup (Drop + SIGTERM handler unlink) | DONE | pre-pivot | `supervisor/src/control_socket.rs` + `signals.rs`. |
| T-068 | Control-socket audit log | DONE | pre-pivot | `crates/supervisor/src/audit_log.rs`. |
| T-069 | Supervisor orchestration sequence | SUPERSEDED | cleanup T-009 (`6975f58`) | `run_supervisor_with` signature cut from 10 to 7 args; R3 step 6 collapsed to mux-only, step 10 skipped, step 13 bare-session park, step 15 skipped. Orchestration sequence preserved modulo engine/orchestrator boxes. |
| T-070 | SIGTERM handler: cancel + 10s grace + escalate | DONE | pre-pivot | `crates/supervisor/src/signals.rs` + `kill.rs`. |
| T-071 | Crash detection via `kill(pid,0)` | DONE | pre-pivot | `crates/supervisor/src/crash.rs`. |
| T-072 | Auto-close on done/fail/kill | DONE | pre-pivot | `crates/supervisor/src/auto_close.rs`. |
| T-073 | ClaudeCodeOrchestrator::detect | CUT | cleanup T-004 (`df7206f`) | `crates/orchestrators/claude-code/` deleted. |
| T-074 | ClaudeCodeOrchestrator::run | CUT | cleanup T-004 (`df7206f`) | Tab-open-and-wait workflow replaced by scene/extension surface. |
| T-075 | CavekitOrchestrator::detect | CUT | cleanup T-003 (`df7206f`) | `crates/orchestrators/cavekit/` deleted. |
| T-076 | CavekitOrchestrator::engine() + builder tab | CUT | cleanup T-003 | Scene-v3 drives tab open. |
| T-077 | Impl-tracking watcher | CUT | cleanup T-003 | Watcher moved out of v0.1; workflow now an extension concern. Fixture `cavekit-project/` preserved in `crates/test-fixtures/` as test input. |
| T-078 | Build-site total-task extractor | CUT | cleanup T-003 | Deleted with cavekit orchestrator. |
| T-079 | Ralph-loop watcher | CUT | cleanup T-003 | Deleted. |
| T-080 | Phase detection + review tab logic | CUT | cleanup T-003 | Deleted. |
| T-081 | Codex findings watcher | CUT | cleanup T-003 | Deleted. |
| T-082 | Git diff/numstat watcher | CUT | cleanup T-003 | Deleted. |
| T-083 | Cavekit orchestrator done-signal resolver | CUT | cleanup T-003 | Deleted. |

### Tier 4 — CLI

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-084 | ark-cli binary scaffold | DONE | pre-pivot | `crates/cli/src/cli.rs`. clap derive, NO_COLOR, <80-col help. |
| T-085 | Exit-code contract (0/1/2/3/4/5) | DONE | pre-pivot | `crates/cli/src/exit.rs` + `error.rs`. |
| T-086 | ID resolution helper | DONE | pre-pivot | `crates/cli/src/id_resolver.rs`. |
| T-087 | `ark spawn` subcommand | SUPERSEDED | claude-code-first pivot | `spawn` verb removed from `Commands` enum. Launch flow is bare `ark` → `supervisor_handoff.rs` → `launch/real.rs::spawn_and_wait_for_ready` (test `help should not list 'spawn'` in `cli.rs:245` enforces). spec.json write + fork supervisor + <1s return + file lock + $ZELLIJ branching semantics preserved internally. |
| T-088 | `ark list` | DONE | pre-pivot | `crates/cli/src/commands/list.rs`. |
| T-089 | `ark kill` | DONE | pre-pivot | `crates/cli/src/commands/kill.rs`. |
| T-090 | `ark config` | DONE | pre-pivot | `crates/cli/src/commands/config.rs`. |
| T-091 | `ark doctor` | DONE | pre-pivot | `crates/cli/src/commands/doctor.rs` (1200+ LOC). |
| T-092 | `ark pane` routing | DONE | pre-pivot | `crates/cli/src/commands/pane.rs` routes to ark-pane impls. |
| T-093 | env-var recognition (ARK_LOG / NO_COLOR / ARK_*) | DONE | pre-pivot | plumbed across cli + tracing_init + figment provider. |

### Tier 5 — Wasm plugins (status + picker)

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-094 | ark-plugin-status scaffolding | DONE | pre-pivot | `crates/plugins/status/Cargo.toml` cdylib wasm32-wasip1 + zellij-tile + permissions. |
| T-095 | Status plugin pipe ingestion + cache | DONE | pre-pivot | `crates/plugins/status/src/lib.rs`. |
| T-096 | Status plugin chip rendering | DONE | pre-pivot | `crates/plugins/status/src/chip.rs`. |
| T-097 | Status plugin WASI fs fallback | DONE | pre-pivot | `crates/plugins/status/src/fs_scan.rs`. |
| T-098 | Status plugin distribution wiring | DONE | pre-pivot | `crates/cli/build.rs` compiles + embeds. |
| T-099 | ark-plugin-picker scaffolding | DONE | pre-pivot | `crates/plugins/picker/Cargo.toml` uses `nucleo-matcher` + ChangeApplicationState + ReadApplicationState + MessageAndLaunchOtherPlugins. |
| T-100 | Picker state model | DONE | pre-pivot | `crates/plugins/picker/src/state.rs`. |
| T-101 | Picker bootstrap | DONE | pre-pivot | `crates/plugins/picker/src/bootstrap.rs` — scan status.json + socket dir + reachability + cross-ref. |
| T-102 | Picker W1 List screen | DONE | pre-pivot | `crates/plugins/picker/src/render_list.rs`. |
| T-103 | Picker W2 Detail screen | DONE | pre-pivot | `crates/plugins/picker/src/render_detail.rs` + on-demand socket Status. |
| T-104 | Picker W3 New-agent form | PARTIAL | claude-code-first pivot | **Re-inspected 2026-04-20:** CLI-exec drift is resolved in kit (reads bare `ark --scene <o> --cwd <c> --session <n>` at R6 line 97 — no `ark spawn` verb). Residual drift: the kit's Orchestrator radio (`cavekit \| claude-code`) references deleted orchestrator crates (Cleanup Packet A). Proper fix requires design direction on the post-pivot form — does the "Orchestrator" slot collapse into Scene selection, or get dropped entirely? Does Cmd remain an independent field? Needs user scoping, not mechanical fixup. Marked PARTIAL pending that direction. |
| T-105 | Picker W4 Confirm-kill + rename + detach | DONE | pre-pivot | `crates/plugins/picker/src/render_confirm.rs` + `socket_cmd.rs`. |
| T-106 | Picker resurrect flow | DONE | pre-pivot | bootstrap + socket_cmd cover it (re-exec `ark` with saved spec). |
| T-107 | Picker Enter switch_session + Esc hide_self | DONE | pre-pivot | `crates/plugins/picker/src/lib.rs`. |
| T-108 | Picker keybinding map + W5 help | DONE | pre-pivot | `crates/plugins/picker/src/render_help.rs`. |
| T-109 | Picker distribution wiring | DONE | pre-pivot | `crates/cli/build.rs` compiles + embeds picker. |

### Tier 6 — Testing, distribution, release plumbing

| Task | Title | Status | Landing / Reason | Notes |
|------|-------|--------|------------------|-------|
| T-110 | ark-test-fixtures crate | DONE | pre-pivot | `crates/test-fixtures/` + README. |
| T-111 | Fixture cavekit-project/ | DONE | pre-pivot | `crates/test-fixtures/tests/fixtures/cavekit-project/`. (Preserved for test input even though runtime CavekitOrchestrator is CUT.) |
| T-112 | Fixture claude-transcripts/ | DONE | pre-pivot | `crates/test-fixtures/tests/fixtures/claude-transcripts/` — 7 JSONL files covering ToolUse/Message/FileEdited/rotation/malformed/empty. |
| T-113 | Fixture hook-payloads/ | DONE | pre-pivot | `crates/test-fixtures/tests/fixtures/hook-payloads/`. |
| T-114 | Engine contract suite | CUT | cleanup T-010 | `engine_contract.rs` deleted with the trait. |
| T-115 | Orchestrator contract suite | CUT | cleanup T-010 | `orchestrator_contract.rs` deleted with the trait. |
| T-116 | Multiplexer contract suite | SUPERSEDED | mux-tight-coupling M-5/M-6 | `ZellijMux` is concrete; mux contract testing happens via `MockMux` / `StubMux` / `NoopMux` helpers landed in mux-tight-coupling. |
| T-117 | ark-types round-trip unit tests | DONE | pre-pivot | 85 tests per impl-overview. |
| T-118 | ark-core unit tests | DONE | pre-pivot | state dir + events.jsonl + status.json + crash-recovery tests live. |
| T-119 | ark-config unit tests | DONE | pre-pivot | 39 tests per impl-overview. |
| T-120 | ark-engines-claude-code unit tests | SUPERSEDED | claude-code-ext | Moved to `extensions/claude-code/tests/` (settings_reconcile, payload_fields, cc_hook_fail_open, view_integration, rhai_envelope, claude_code_smoke). |
| T-121 | ark-orchestrators-cavekit unit tests | CUT | cleanup T-003 | Parsers deleted with crate. (Fixture preserved.) |
| T-122 | ark-mux-zellij unit tests | DONE | pre-pivot | minijinja templating + argv construction + `.kdl` extension validation tests live. |
| T-123 | ark-pane unit tests | DONE | pre-pivot | arg parsing + rendering + NO_COLOR + SIGWINCH. |
| T-124 | control-socket NDJSON tests | DONE | pre-pivot | GC helper behavior covered. |
| T-125 | Plugin tests (fuzzy / render / pipe / scan) | DONE | pre-pivot | Live in plugins crates. |
| T-126 | Mock `claude` shim | SUPERSEDED | test-fixtures/claude-code | `crates/test-fixtures/claude-code/` crate ships `mock-claude` binary. |
| T-127 | E2E scenarios (spawn/list/kill/stall/done/crashed + picker) | SUPERSEDED | cli + launch tests | E2E coverage is now `crates/cli/tests/launch_pty.rs` + `launch_integration.rs` + `w8_spawn_integration.rs` (supervisor-wiring W-8). Uses bare `ark`, not `ark spawn`. |
| T-128 | E2E CI gating via ARK_E2E=1 | DONE | pre-pivot | CI honored via gate env var. |
| T-129 | CI workflow (ubuntu + macOS, zellij ≥0.44.1 + delta, wasm build) | DONE | pre-pivot | `.github/workflows/ci.yml`. |
| T-130 | ark-cli build.rs compile wasm + include_bytes | DONE | pre-pivot | `crates/cli/build.rs` + `crates/cli/src/embedded.rs`. |
| T-131 | Wasm release profile + wasm-opt -Oz | DONE | pre-pivot | Cargo.toml + CI release step. |
| T-132 | CI wasm size delta-watch (>25% growth fails) | DONE | pre-pivot | `.github/workflows/wasm-size.yml`. |
| T-133 | cargo-dist init + 4 targets | DONE | pre-pivot | `.github/workflows/release.yml` + dist config. |
| T-134 | Homebrew tap `rlch/ark/ark` + cargo install + binstall | DONE | pre-pivot | `scripts/generate-brew-formula.sh`. |
| T-135 | Standalone .wasm release assets | DONE | pre-pivot | release.yml publishes wasm tarballs. |

## Actually-pending tasks

**None.** Zero T-rows require new implementation work. The single PARTIAL
(T-104) is a cosmetic/documentation mismatch — the picker's New-agent
form functionally works by exec'ing `ark`, but the kit wording says
`ark spawn`. No code change needed; it's a kit-text update that belongs
in a future kit-refresh pass, not a build packet.

## Suggested dispatch plan

**No packets required.** Every functional build-site requirement is
either DONE, has been replaced by a newer canonical site
(phase-2/claude-code-ext/cleanup/scene-v3), or has been explicitly CUT
from v0.1 scope. The site is effectively closed for v0.1.

Optional future work (v0.2+):

- **Kit refresh** — `context/plans/build-site.md` is dated 2026-04-14
  and references cut concepts (ark-hook, ark spawn, orchestrator traits,
  engine trait, Multiplexer trait). Keep as historical record or
  archive into `context/plans/archive/build-site-pre-pivot-2026-04-14.md`
  and write a replacement site that reflects the post-pivot workspace
  shape. Non-blocking.
- **T-104 picker wording** — update `cavekit-plugin-picker.md` R6 to
  say "execs `ark`" instead of "execs `ark spawn`". Again, non-blocking.

## Plan-overview update

Row for build-site.md currently shows `135 / 0 / DRAFT`.

Recommendation: flip to **DONE 2026-04-18 (audit)** with qualifier text
akin to:

> `ark v1` / `build-site.md` / 135 / 135 / **DONE 2026-04-18 (audit)** —
> 91 DONE / 23 SUPERSEDED (phase-2 soul + claude-code-ext + scene-v3 +
> cleanup) / 20 CUT (2026-04-18 pivot deletions: ark-hook, ark spawn,
> orchestrator crates, Engine/Orchestrator/Multiplexer traits,
> permission.rs, factory.rs, cavekit-orchestrator tests, engine/orch
> contract suites) / 1 PARTIAL (T-104 picker wording, non-blocking) /
> 0 PENDING. See `context/impl/impl-build-site-ark-v1-audit.md`.

## Uncertainties

1. **T-120 boundary** — extension tests in `extensions/claude-code/tests/`
   are strong replacements for the deleted `ark-engines-claude-code`
   test surface. Classed SUPERSEDED. Could be DONE under a looser
   interpretation.
2. **T-127 boundary** — the v0.1 test suite does cover spawn/list/kill
   through bare `ark` + supervisor-wiring W-8 PTY tests; "crashed
   supervisor archive" scenario is lighter than the original kit
   wording but the archive pathway itself is exercised. Marked
   SUPERSEDED.
3. **T-087 `spawn` contract preservation** — The file-lock, spec.json
   write, <1s parent return, and $ZELLIJ branching acceptance bullets
   are all still satisfied by the post-pivot launch flow. The only
   broken bullet is the literal `ark spawn` verb. Marked SUPERSEDED
   rather than PARTIAL because the underlying acceptance is preserved.
