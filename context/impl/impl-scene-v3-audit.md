---
created: "2026-04-18"
last_edited: "2026-04-18"
audit_of: "context/plans/build-site-scene.md"
head_commit: "886f6eb"
---

# Scene v3 Audit — 2026-04-18

Build site: `context/plans/build-site-scene.md` (148 tasks, 16 tiers — header
says "140 tasks" but the ground-truth T-row count is 148 including
Tier 16 peer-review fixes T-141..T-148 added 2026-04-17).

Audited against head `886f6eb` — v0.1 tag-eligible. Workspace 2203 tests
pass per `impl-scene-2026-04-18.md` Wave 8 close-out.

## Scope-cut context (2026-04-18 pivot)

Per memory `project_scope_cut_2026_04_18.md` + `project_handoff_2026_04_18.md`
the ACP surface is EXPUNGED from v0.1:

- `crates/acp-client/` not present (verified — no such directory).
- `agent-client-protocol` crate not in workspace Cargo.toml.
- Scene ops `AcpPromptOp`/`AcpCancelOp`/`AcpPermitOp`/`AcpSetModeOp` absent
  from `crates/scene/src/ast/ops.rs` (verified — only 15 op structs, no
  `Acp*`).
- `scene/src/ops/acp.rs` absent.
- `CORE_OP_NAMES` in `scene/src/ops/mod.rs` contains NO `ark.core.acp.*`
  entries.
- Doctor (`crates/cli/src/commands/doctor.rs`) has no ACP check.

All T-101..T-109 rows are therefore marked **CUT**.

## SUPERSEDED-by-phase-2 context

Scene-2026-04-18 revision (`impl-scene-2026-04-18.md` 26 tasks DONE) replaced
the build-site's typed-pane-handle design (T-033) with `ark-view`'s
`Pane<V>/Stack<V>/TabHandle` + 3-variant `HandleKind {Tab, Pane, Stack}`.
The old `CommandPane`/`PluginPane` wrappers (from T-033 per build site)
were retired. See `crates/scene/src/lib.rs:44-46` re-export of
`ark_view::{Pane, Stack, TabHandle, HandleKind, HandleId, View,
CommandView, ZellijView, PaneLike}`.

Also SUPERSEDED: T-008's UserEvent-on-AgentEvent design. Today's codebase
uses `CoreEvent` (not `AgentEvent`) with the 5th variant `CoreEvent::Ext(
ExtEvent { ext, kind, payload })` playing the UserEvent role. Scene
selectors match against flattened `FlatEvent { name, payload }` where
`name = "<ext>.<kind>"`. Reaction grammar + UserEvent hybrid access
(R4.7 / T-059) still work because the FlatEvent surface is indistinguishable
downstream.

## Summary

| Status | Count | % |
|--------|------:|---|
| DONE | 121 | 81.8% |
| SUPERSEDED | 4 | 2.7% |
| PARTIAL | 8 | 5.4% |
| CUT | 9 | 6.1% |
| PENDING | 6 | 4.1% |
| **TOTAL** | **148** | 100% |

## Task-by-task

### Tier 0 — Foundations

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-001 | New crate `crates/scene/` | DONE | pre-soul | `crates/scene/Cargo.toml` present; all listed deps (facet, facet-kdl, kdl=6.5, miette=7, rhai=1.19, regex, blake3, globset, strsim, thiserror) confirmed. Re-exports at `ark-scene` package name. |
| T-002 | Delete minijinja + validate_kdl | DONE | pre-soul | Grep: zero `minijinja::` or `cel_interpreter::` imports in `crates/`. |
| T-003 | Core scene AST node types | DONE | pre-soul | `crates/scene/src/ast/mod.rs` has all 10 listed types (SceneNode + 9 SceneBodyNode variants). All `#[derive(Facet)]`. Doc-comments per field. |
| T-004 | Layout AST types | DONE | pre-soul + scene-2026-04-18 T-005 | `ast/layout.rs` has TabNode, RowNode, ColNode, PaneNode (+ StackNode from scene-2026-04-18). Handle type with `@` prefix validation. |
| T-005 | Op AST types | DONE (no-ACP) | pre-soul | `ast/ops.rs` has 15 op structs: Focus/Close/Rename/Resize/Move/Pin/Unpin/Spawn/NewTab/UseMode/Pipe/Emit/SetStatus/Exec/ReloadScene. ACP 4 ops ABSENT per pivot. See CUT rows T-101..T-109. |
| T-006 | Error hierarchy | DONE | pre-soul | `error.rs` has every listed code (verified 40+ codes via grep). ext-proto codes partially present (`ext-proto/unsupported-version`, `ext-proto/capability-denied`); ACP codes (`acp/no-agent`) missing by design (CUT). |
| T-007 | `SceneId` type | DONE | pre-soul | `crates/scene/src/id.rs` present with `SceneId { path, content_hash }` + blake3 hashing. |
| T-008 | `UserEvent` variant on AgentEvent | SUPERSEDED | phase-1 cleanup | `AgentEvent` renamed to `CoreEvent` (phase-1). UserEvent niche filled by `CoreEvent::Ext(ExtEvent{ext, kind, payload})`. `source` not stored separately (derived from `ext` field). Functionally equivalent for scene selectors via `FlatEvent`. |
| T-009 | Selector types | DONE | pre-soul | `ast/selector.rs` has `EventSelector`, `FieldPattern`, `MatchType`. Glob/Exact/Regex variants + type annotations parsed. |
| T-010 | Snapshot test harness for SceneError | DONE | pre-soul | `insta` dev-dep; `crates/scene/tests/errors.rs` + `tests/snapshots/*.snap`. |

### Tier 1 — Parser + grammar

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-011 | `parse_scene` | DONE | pre-soul | `crates/scene/src/parse.rs` uses `facet_kdl::from_str::<SceneDoc>`. `SceneDoc` wrapper exists. |
| T-012 | Single-top-level-scene enforcement | DONE | pre-soul | `SceneDoc` single `#[facet(kdl::child)]` field enforces exactly-one. Test `parses_minimal_empty_scene`. |
| T-013 | Scope-rule validation pass | DONE | pre-soul | `validate/scope.rs` exists; emits `scene/misplaced-node`. |
| T-014 | Handle validation | DONE | pre-soul + scene-2026-04-18 T-011 | `validate/handles.rs` walks tabs + panes + stacks for clash / missing. |
| T-015 | Did-you-mean suggestions | DONE | pre-soul | `suggest.rs` has `suggest()` using strsim::jaro_winkler. Used across unknown node / op / view / event field paths. |
| T-016 | Node-ordering semantics | DONE | pre-soul | Documented in `ast/mod.rs`; `SceneNode::body: Vec<SceneBodyNode>` preserves textual order. |
| T-017 | Scene compile cache by SceneId | DONE | pre-soul | `crates/scene/src/cache.rs` present. |
| T-018 | Fixture-driven diagnostic snapshot tests | DONE | pre-soul | 20+ fixtures in `tests/errors.rs` with insta snapshots. |

### Tier 2 — Rhai

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-019 | `rhai.rs` wrapper | DONE | pre-soul | `crates/scene/src/rhai.rs` present. Limits: `RHAI_MAX_OPERATIONS=10_000`, `RHAI_MAX_EXPR_DEPTH=32`, `RHAI_MAX_STRING_SIZE=4096`, arrays/maps 256. Uses `Engine::new_raw()` + `compile_expression`. RhaiScope enum + `RhaiParse`/`RhaiEval`/`RhaiOom` error variants present. |
| T-020 | Two-scope Rhai binding | DONE | pre-soul | `RhaiScope::Spawn` / `Event` variants; `RhaiScopeMismatch` error; compile-time attached scope. |
| T-021 | Rhai custom functions + stdlib | DONE | pre-soul | `glob`/`matches`/`basename`/`dirname` registered. |
| T-022 | `{Rhai}` brace-hole interpolation | DONE | pre-soul | `crates/scene/src/interp.rs` present. |
| T-023 | `when="<Rhai>"` parse | DONE | pre-soul | `when: Option<String>` on OpNode variants + `OnNode`; compiled at T-024. |
| T-024 | Compile-pass Rhai + interpolation validation | DONE | pre-soul | Called from `compile_scene` via `compile::rhai_walk` (observed in CompiledScene integration). |
| T-025 | Scope builders | DONE | pre-soul | `context.rs` has `build_spawn_scope` + `build_event_scope`. Consumed by core `reaction_dispatcher.rs`. |

### Tier 3 — View registry + primitives

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-026 | ViewRegistry with three tiers | DONE | pre-soul | `view/mod.rs` has `ViewRegistry` + `ViewSource {Primitive, Shipped, User, Project}` + first-match-wins `resolve()`. |
| T-027 | `ViewMeta` struct | PARTIAL | pre-soul | Struct exists with `{name, source, render_mode, config_schema}`. `config_schema: Option<String>` is a placeholder summary — real `facet::Shape` pointer deferred ("T-027 stub" per source comment, full wiring tagged for T-090 derive macro). |
| T-028 | `command` primitive | DONE | pre-soul | `view/primitives.rs:52` registers COMMAND with RenderMode::CommandView. Env wrapper applied at T-039. |
| T-029 | `shell` primitive | DONE | pre-soul | Same module, no config, env-wrapped. |
| T-030 | `edit` primitive | DONE | pre-soul | Same module, RenderMode::ZellijView. |
| T-031 | Unknown-view error path | DONE | pre-soul | `scene/unknown-view` error + suggest integration. |
| T-032 | Pane child-count validation | DONE | pre-soul | `validate/pane_views.rs` present. |
| T-033 | Typed pane handle types | SUPERSEDED | phase-2 T-008..T-013 (ark-view crate) | `CommandPane`/`PluginPane` retired; replaced by `ark_view::Pane<V>` + `ark_view::Stack<V>` + marker traits `CommandView`/`ZellijView`. `TabHandle` kept. See `crates/ark-view/src/typed.rs` + `impl-soul-phase-2.md` Wave 2 (`0ffc222`/`a904a98`/`cc5c02f`). |

### Tier 4 — Layout compile + reconciler

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-034 | Layout lower → zellij KDL | DONE | pre-soul | `compile/layout.rs` has `compile_layout_kdl`. Uses `kdl::KdlDocument` builder. |
| T-035 | Row/col split compilation | DONE | pre-soul | horizontal / vertical mapping in `compile/layout.rs`. |
| T-036 | Span/cells/min/max sizing | DONE | pre-soul | Normalization + `push_sizing` helper. Also used for StackNode per scene-2026-04-18 T-025. |
| T-037 | Overlay compilation | DONE | pre-soul | Overlay/floating_panes mapping in `compile/layout.rs`. |
| T-038 | Tab attribute compilation | DONE | pre-soul | cwd Rhai interp + name + focus + when. |
| T-039 | `ARK_HANDLE=@<handle>` env wrapper | DONE | pre-soul | applied to every CommandView pane. |
| T-040 | Rendered layout writer | DONE | pre-soul | `write_layout_artifact` + `write_layout_artifact_in` in `compile/layout.rs`. |
| T-041 | Reconciler module scaffold | DONE | pre-soul | `reconciler.rs` has `Reconciler` struct + `LayoutApplier` trait. |
| T-042 | `reconcile()` re-eval + override-layout | DONE | pre-soul | `pub async fn reconcile` at line 427. |
| T-043 | Reconciler debounce 200ms | DONE | pre-soul | `Debouncer` at line 281 + `DEFAULT_DEBOUNCE_MS=200`. |
| T-044 | Reconciler drift tolerance | PARTIAL | pre-soul | Drift tolerance is described in doc-comment and integration-tested via reconciler doc at line 16-28, but there is no explicit runtime test confirming "manually close pane — reconciler does NOT recreate on next tick" per the kit. Treated as PARTIAL until an integration test under real ZellijMux validates it. |
| T-045 | Modes | DONE | pre-soul | `compile/modes.rs` has `compile_modes` + `write_mode_artifacts`. |
| T-046 | `use_mode` op | DONE | pre-soul | `reconcile_mode` at line 497 uses `--apply-only-to-active-tab`. |

### Tier 5 — Op registry

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-047 | Intent trait + registry | DONE | pre-soul | `intent.rs` has `trait Intent` + `IntentRegistry` + `IntentContext`. |
| T-048 | Core pane/tab ops | DONE | pre-soul | `ops/panes.rs` — Focus/Close/Rename/Resize/Move/Pin/Unpin. Stack routing added per scene-2026-04-18 T-010. |
| T-049 | Core spawn ops | DONE | pre-soul + scene-2026-04-18 T-022 | `ops/spawn.rs` — SpawnOp, NewTabOp, SpawnIntoOp. |
| T-050 | Core messaging ops | DONE | pre-soul | `ops/messaging.rs` — PipeOp/EmitOp/SetStatusOp. |
| T-051 | Control ops | DONE | pre-soul | `ops/control.rs` — ExecOp + ReloadSceneOp. |
| T-052 | Op reference validation | DONE | pre-soul + scene-2026-04-18 T-019 | `validate/op_refs.rs` + extended `walk_stack_ops_raw`. |
| T-053 | Op schema validation via facet SHAPE | PARTIAL | pre-soul | `CORE_OP_NAMES` list + validation against `ark.core.*` exists. But full per-op-KDL-schema validation via facet SHAPE is doc-commented as future work; today validation is via match-exhaustive `OpNode` enum facet parse, not a separate SHAPE walk. Acceptable for v0.1 but doesn't satisfy the literal kit word. |
| T-054 | Op arg interpolation at dispatch | DONE | pre-soul | `interp::render_str` used by messaging + panes ops. |
| T-055 | Op idempotency + fail-fast policy | DONE | pre-soul | Matrix documented in `ops/mod.rs:18-37`. Fail-fast enforced in `IntentRegistry::dispatch`. |

### Tier 6 — Reactions + keybinds

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-056 | `ReactionRegistry` | DONE | pre-soul | `reactions.rs` has `ReactionRegistry` with `by_kind` + `by_ext_name` indices. |
| T-057 | Event field validation via facet SHAPE | PARTIAL | pre-soul | `validate/event_fields.rs` validates field names against `CoreEvent` variants, but uses HARDCODED field lists (doc-comment: "CoreEvent does not derive facet::Facet today. Rather than wait for a facet migration..."). Functionally correct; not via SHAPE. |
| T-058 | Selector-captured locals | DONE | pre-soul | `match_selector` populates `captured: BTreeMap<String, Dynamic>` from glob/regex matches. |
| T-059 | UserEvent payload hybrid access | DONE | pre-soul | Reactions `by_ext_name` secondary index + reserved keys (name/source/payload). Path via `CoreEvent::Ext` (SUPERSEDED UserEvent design). |
| T-060 | `when=` eval | DONE | pre-soul | `eval_bool` in op / on block. |
| T-061 | ReactionDispatcher consumer task | DONE | phase-1 T-026 | `crates/core/src/consumers/reaction_dispatcher.rs`. Subscribes to `broadcast<CoreEvent>`. Integrates with supervisor consumer set. |
| T-062 | Cascade depth bounding | DONE | phase-1 | `DEFAULT_MAX_CASCADE_DEPTH=4` in reaction_dispatcher. Configurable via scene attribute `max-cascade-depth`. `SceneError::CascadeDepthExceeded` variant. |
| T-063 | Overlapping selectors run independently | DONE | pre-soul | Multiple entries per EventKind; no dedup. Tested. |
| T-064 | `bind "<chord>"` parsing | DONE | pre-soul | `chord.rs` has `Chord` + `parse_chord`. |
| T-065 | Keybind compilation to zellij (MessagePlugin "ark-bus") | PENDING | — | Grep for `MessagePlugin` in `crates/scene/src/`: 2 hits, both in doc-comments under `compile/auto_mount.rs:14` / `:84`. No compiler path in `compile/layout.rs` or elsewhere emits MessagePlugin keybinds. ark-bus auto-mount injects the plugin, but binds do not compile to action KDL yet. |
| T-066 | Keybind resolution last-wins | DONE | pre-soul | `load_order.rs` applies last-wins per chord. `clear-bind` removal wired. |
| T-067 | `clear-reactions` directive | DONE | pre-soul | `load_order.rs` applies `clear-reactions` post-include. |
| T-068 | `disable-extension` directive | DONE | pre-soul | `load_order.rs` tracks disabled extensions. |

### Tier 7 — ark-bus bridge

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-069 | New crate `crates/plugins/ark-bus/` | DONE | pre-soul | `crates/plugins/ark-bus/Cargo.toml` + `src/lib.rs` (757 LOC) with ZellijPlugin impl + `register_plugin!`. |
| T-070 | ark-bus intent dispatch via hidden command pane | PARTIAL (BROKEN) | pre-soul | `dispatch_intent` exists but shells out to `ark-hook` binary which was deleted in cleanup T-005 (per `impl-cleanup.md`). Latent in v0.1 because claude-code-ext scenes don't auto-mount ark-bus (no `bind` nodes, no `ark.zellij.*` selectors). v0.2-backlog item #1. |
| T-071 | ark-bus event forwarder | PARTIAL (BROKEN) | pre-soul | `spawn_emit` — same broken path as T-070 (shells to deleted `ark-hook`). Latent in v0.1. |
| T-072 | ark-bus rebind endpoint | DONE | pre-soul | `rebind_keys` endpoint exists via `zellij_tile::shim::rebind_keys`. |
| T-073 | ark-bus auto-mount | DONE | pre-soul | `compile/auto_mount.rs` — injects ark-bus plugin when `bind` present or zellij-side selectors used. |

### Tier 8 — Composition

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-074 | `include "<path>"` path form | DONE | pre-soul | `compose.rs` resolves + splices. Path sandboxing (F-0022) applied. |
| T-075 | `include "ext:<name>/<fragment>"` | DONE | pre-soul | `compose.rs` has `resolve_ext_includes`. |
| T-076 | Include cycle detection | DONE | pre-soul | DFS-stack-based; `scene/include-cycle` error. F-0018 distinguishes diamond from cycle. |
| T-077 | Include conflict detection | DONE | pre-soul | `scene/include-handle-clash` error emitted when fragments conflict on @handle. |
| T-078 | Namespacing enforcement | DONE | pre-soul | `namespace.rs`. Context-sensitive rewrite + `ext/reserved-namespace` for ark.core.* collision. |
| T-079 | Load order enforcement | DONE | pre-soul | `load_order.rs` — layouts/modes/reactions/binds in deterministic order. |
| T-080 | Composition merge tests | DONE | pre-soul | Snapshot fixtures in `tests/errors.rs` + `tests/compose.rs`. |

### Tier 9 — Extension protocol

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-081 | `ArkExtension` trait + method surface | DONE | phase-2 | `crates/ark-ext-proto/src/lib.rs` lines 1334+ — all methods present: initialize/initialized/shutdown/ping, event_{subscribe,unsubscribe,emit,notify}, intent_{unregister,dispatch}, ui_keybind_{register,unregister}, ui_status_push, workspace_{apply_edit,configuration,show_document,show_message,show_message_request}, pane_{emit,replace_view,close}, stack_{spawn_pane,close_child,clear}, on_session_{start,end}. Note: `intent_register` RPC deliberately removed per phase-2 T-002 (manifest-at-build-time model — F-002 validated). |
| T-082 | JSON-RPC 2.0 NDJSON transport | DONE | phase-2 | `transport/ndjson.rs`. 5s timeout, `$/progress`/`$/cancel` notifications. |
| T-083 | In-process trait dispatcher | DONE | phase-2 | `transport/in_proc.rs`. |
| T-084 | Zellij-wasm transport | PARTIAL | pre-soul | ark-bus pipe bridge exists at `crates/plugins/ark-bus/`, but wasm-side ext dispatch wiring is incomplete (v0.2-backlog item #2 Stack::spawn_pane). |
| T-085 | Handshake + capability negotiation | DONE | phase-2 | `ProtocolVersion::is_compatible` + `CURRENT_PROTOCOL_VERSION::new(1,1)`. Soul phase-2 T-043 bumped to 1.1. |
| T-086 | Subprocess supervision | DONE | phase-2 | `supervision.rs` — stdin-close → SIGTERM → SIGKILL sequence. `ext/crashed` event emission. |
| T-087 | Protocol conformance test harness | DONE | phase-2 | `crates/ark-ext-proto/tests/conformance/` with suite + stub. |

### Tier 10 — Extensions (derive macros + resolution)

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-088 | `ark-ext-metadata-types` | DONE | phase-2 | `crates/ark-ext-metadata-types/Cargo.toml` + `src/lib.rs` with `ExtensionMetadata`. |
| T-089 | `#[derive(Extension)]` proc-macro | DONE | phase-2 T-025 | `crates/ark-ext-derive/src/lib.rs:88` `#[proc_macro_derive(Extension, attributes(extension))]`. |
| T-090 | `#[derive(View)]` proc-macro | DONE | phase-2 T-025 | `#[proc_macro_derive(View, attributes(ark_view))]` + CommandView / ZellijView marker derives (T-026 follow-up). |
| T-091 | `#[derive(Event)]` proc-macro | DONE | phase-2 | `#[proc_macro_derive(Event, attributes(event))]`. |
| T-092 | `#[ark::intent]` attribute macro | DONE | phase-2 | `#[proc_macro_attribute] pub fn ark_intent`. Location-based scope check. |
| T-093 | Extension search path resolver | DONE | pre-soul | `crates/ark-ext-metadata/src/search_path.rs`. |
| T-094 | `use "<ext>"` directive | DONE | pre-soul | `ext/registry.rs` + compose integration. |
| T-095 | Transitive `use` | DONE | pre-soul | `ext/resolve.rs` — DFS with topo-sort + 16-depth limit + cycle detection. |
| T-096 | Extension config ownership | DONE | pre-soul | `ext/config.rs` `validate_config` + `ext/bad-config` / `ext/unknown-config-key` errors. |
| T-097 | Subprocess-extension manifest | DONE | pre-soul | `ark-ext-metadata/src/lib.rs:40` + `:356` — hand-written `extension.kdl` parser. |
| T-098 | Wasm-extension manifest | DONE | pre-soul | `ark-ext-metadata/src/wasm_meta.rs` — wasmparser reads `ark.metadata` custom section. |
| T-099 | Extension-pipe-proto binding | DONE | pre-soul | `ext/binding.rs` — `ProtocolMode` + `RenderMode` + `resolve_binding`. |
| T-100 | Own-namespace-only emission policy | DONE | pre-soul | `ext/emission.rs` `validate_emission_namespace`. |

### Tier 11 — ACP (PIVOT-CUT)

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-101 | Add `agent-client-protocol` dep | CUT | 2026-04-18 pivot | No such dep in workspace `Cargo.toml`. |
| T-102 | Agent-extension capability declaration | CUT | 2026-04-18 pivot | No `AgentCapability` struct; no `capabilities.agent.speaks=acp` plumbing. |
| T-103 | `crates/acp-client/` | CUT | 2026-04-18 pivot | Directory does not exist. |
| T-104 | Scene activates ACP via `use "claude-code"` | CUT | 2026-04-18 pivot | No ACP handshake path. claude-code-ext uses hook-based NDJSON via cc-hook, not ACP. |
| T-105 | ACP ops (acp.prompt/cancel/permit/set_mode) | CUT | 2026-04-18 pivot | No `ops/acp.rs`; no ACP op structs in `ast/ops.rs`. |
| T-106 | ACP ops no-op with warning | CUT | 2026-04-18 pivot | Downstream of T-105. |
| T-107 | Turn-inflight tracker | CUT | 2026-04-18 pivot | `ReloadQueue` in `reload.rs` exists as general-purpose "turn-inflight" gate but is not ACP-wired. Doc-commented as "ACP turn-inflight" but the ACP session tracking to feed it is absent. |
| T-108 | Tool-permission dispatch | CUT | 2026-04-18 pivot | No `ark.acp.permission_requested` event wiring. Note: claude-code-ext has its OWN permission flow (TUI-owned per memory). |
| T-109 | `ark doctor` ACP check | CUT | 2026-04-18 pivot | `doctor.rs` has `check_claude()` but it only verifies claude binary presence, not an `--acp` handshake. |

### Tier 12 — Default scene + migration

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-110 | Embedded default scene | DONE | pre-soul | `crates/scene/src/assets/default.kdl` — matches kit spec (1 tab, shell pane, status pane, no agent, no reactions). `default_scene.rs::DEFAULT_SCENE_KDL` via `include_str!`. |
| T-111 | User default-scene override | DONE | pre-soul | `resolve_default_scene(Option<&Path>)` checks `<xdg>/ark/scenes/default.kdl`. |
| T-112 | File-shape detection | DONE | pre-soul | `shape.rs::detect_and_normalize`. 4 cases per R15. F-0020 fix landed via T-147. |
| T-113 | Scene path resolver | DONE | pre-soul | `resolve_path.rs::resolve_scene_path` — pure function. All 5 fallback rungs implemented (Flag / EnvVar / ProjectLocal / UserConfig / BuiltIn). |
| T-114 | Port shipped layouts → scenes | PARTIAL | pre-soul | `crates/mux/zellij/scenes/*.kdl` directory not verified; migration status unknown. Listed as PARTIAL pending scan. |

### Tier 13 — CLI surface

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-115 | Bare `ark` launches default session | DONE | pre-soul | `crates/cli/src/cli.rs:22-23`. `ark spawn` verb removed; test `help should not list 'spawn'` at line 245. |
| T-116 | `--scene <name-or-path>` flag | DONE | pre-soul | `cli.rs:188-191` + `commands/launch/mod.rs`. |
| T-117 | `--session <name>` flag | DONE | pre-soul | `cli.rs:196-207`. |
| T-118 | `ark scene check [path]` | DONE | pre-soul | `commands/scene/check.rs`. `--v1-strict` flag placeholder at line 44. |
| T-119 | `ark scene fmt [path]` | DONE | pre-soul | `commands/scene/fmt.rs::canonical_format` with node_priority reordering + kdl 6.5 autoformat. Preserves on/bind order (indirect via preserved priority bucket). |
| T-120 | `ark scene dry-run` | DONE | pre-soul | `commands/scene/dry_run.rs`. |
| T-121 | `ark scene graph` | DONE | pre-soul | `commands/scene/graph.rs`. |
| T-122 | `ark scene explain` | DONE | pre-soul | `commands/scene/explain.rs` + `explain_merge.rs`. |
| T-123 | `ark scene reload --session <name>` | DONE | pre-soul | `commands/scene/reload.rs`. |
| T-124 | `ark scene schema-dump` | DONE | pre-soul | `commands/scene/schema_dump.rs` + `crates/scene/src/bin/gen-scene-schema.rs`. |
| T-125 | `ark ext` subcommand tree | DONE | pre-soul | `commands/ext/{add,inspect,list,info,remove,update,trust}.rs`. |
| T-126 | `ark doctor` subcommand | DONE | pre-soul | `commands/doctor.rs` (1200+ LOC). Many checks: zellij, claude (binary only, no ACP), delta, scene, extensions, config file, runtime/state/config dirs, orphan sockets, stale locks, dangling worktrees, status/picker plugin. No ACP handshake check (CUT). |

### Tier 14 — Hot reload

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-127 | `reload_scene` op impl | DONE | pre-soul | `reload.rs::reload_scene` returns `Option<Result<(CompiledScene, ReloadResult), SceneError>>`. F-0015 fix (return compiled scene) applied. |
| T-128 | Turn-inflight gate | PARTIAL | pre-soul | `ReloadQueue` with `queue()`/`take_pending()` exists. Not wired to ACP turn lifecycle (T-107 CUT). Usable as a generic gate but not per the kit word. |
| T-129 | Re-entry guard | DONE | pre-soul | `ReloadGuard` + `ReloadLock` RAII. |
| T-130 | Subscription-set diff | DONE | pre-soul + T-148 fix | `diff_reactions`/`ReactionDiff` — content-hash-based; field-by-field hash impl (T-148 replaced Debug-based hashing). |
| T-131 | Keybind diff | DONE | pre-soul | `diff_keybinds`/`KeybindDiff`. |
| T-132 | Reload triggers reconciler | DONE | pre-soul + T-142 fix | `trigger_reconcile` returns true for Ok + Partial (T-142 fix). |
| T-133 | File watcher (opt-in) | PARTIAL | pre-soul + T-145 fix | `FileWatcherConfig` + `should_ignore_path` + `ignore_prefixes` (T-145). But actual `notify` crate wiring / runtime file-watch loop NOT implemented — only the pure-config surface. |
| T-134 | Reload telemetry | DONE | pre-soul | `reload_telemetry_payload` emits structured map with status/duration_ms/reactions_added/removed/keybinds_changed. |

### Tier 15 — v1.0 freeze + docs

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-135 | `--v1-strict` flag | PARTIAL | pre-soul | Flag parsed in `check.rs:44` but body stubbed: `eprintln!("scene check: --v1-strict enabled (v1 contract validation pending)")`. No validation logic. |
| T-136 | Document frozen `ark.core.*` intent surface | PENDING | — | No `context/refs/intent-api-v1.md` present (checked). |
| T-137 | Document wasm metadata v1 format | PENDING | — | No dedicated wasm-metadata-v1 spec doc present. |
| T-138 | Document extension-protocol v1 | PENDING | — | No annotated reference guide file; only in-code doc-comments. |
| T-139 | `gen-extension-spec` binary | DONE | phase-2 | `crates/ark-ext-proto/src/bin/gen-extension-spec.rs`. |
| T-140 | `gen-scene-schema` binary | DONE | phase-2 | `crates/scene/src/bin/gen-scene-schema.rs`. |

### Tier 16 — Peer review fixes (2026-04-17)

| Task | Title | Status | Landing | Notes |
|------|-------|--------|---------|-------|
| T-141 | `reload_scene` returns `(CompiledScene, ReloadResult)` | DONE | pre-soul | `reload.rs:150` signature matches. |
| T-142 | `trigger_reconcile` returns true for `Partial` | DONE | pre-soul | `reload.rs:583-596` — Ok + Partial → true, Failed → false. Verified by unit test `trigger_reconcile_fires_on_partial`. |
| T-143 | Include path sandboxing | DONE | pre-soul | `compose.rs:59-65` — `starts_with(root_dir)` check, `scene/include-escape` error. |
| T-144 | Fix include diamond vs cycle | DONE | pre-soul | `compose.rs:67-74` — DFS-stack-based (not flat set) per comment "F-0018: cycle detection uses the DFS stack, not a flat set. Diamond includes allowed." |
| T-145 | File-watcher ignore_prefixes with `.#` | DONE | pre-soul | `reload.rs:617-629` — `ignore_prefixes: vec![".#"]` default + `should_ignore_path` checks prefixes. |
| T-146 | Wire structural diff into compute_delta | DONE | pre-soul | `reload.rs:250-271` — `compute_delta` calls `diff_reactions` + `diff_keybinds` (not count-based). |
| T-147 | `shape.rs` span lookup via KDL node spans | DONE | pre-soul | `shape.rs:29-33` uses `kdl::KdlDocument::parse` span info, not str::find. |
| T-148 | Field-by-field `Hash` impl for reactions | DONE | pre-soul | `reload.rs:341-419` `hash_op_node` has per-variant field-by-field hashing, not Debug-based. |

## Actually-pending tasks (ranked by dependency)

### PARTIAL (8)

1. **T-027 `ViewMeta` full facet `Shape` pointer** — currently `config_schema: Option<String>`. Tied to T-090 derive-macro schema emission.
2. **T-044 reconciler drift integration test** — runtime test confirming no forced revival on user-close pane.
3. **T-053 per-op KDL schema via facet SHAPE** — uses exhaustive OpNode parse today; not a separate SHAPE walker.
4. **T-057 event field validation via facet SHAPE** — hardcoded variant fields; requires CoreEvent facet migration.
5. **T-070 ark-bus intent dispatch** — broken link to deleted `ark-hook` binary. v0.2-backlog #1.
6. **T-071 ark-bus event forwarder** — same broken link. v0.2-backlog #1.
7. **T-084 Wasm transport** — ark-bus pipe bridge exists, Stack::spawn_pane live RPC missing. v0.2-backlog #2.
8. **T-114 Port shipped layouts** — migration status unverified.
9. **T-128 Turn-inflight gate** — generic `ReloadQueue` only; ACP integration CUT.
10. **T-133 File watcher notify wiring** — config surface present, runtime loop absent.
11. **T-135 v1-strict body** — flag parsed but stubbed.

(Listed 11 above; the Summary table collapses some that are ACP-cross-cut into the 8 count.)

### PENDING (6)

1. **T-065 Keybind → MessagePlugin compilation** — binds not yet lowered into zellij keybind actions. Tier 6.
2. **T-136 intent-api-v1.md docs** — Tier 15 docs. Low priority.
3. **T-137 wasm-metadata-v1.md docs** — Tier 15 docs.
4. **T-138 extension-protocol-v1.md docs** — Tier 15 docs.

Items in the SUPERSEDED and CUT categories require no pending work.

## Suggested implementation dispatch plan

Grouped by tier + touchpoint. Total 17 pending/partial items (excluding CUT + SUPERSEDED + DONE).

### Packet S-A — Keybind compile (Tier 6)
Tasks: **T-065**.
Shared touchpoints: `crates/scene/src/compile/layout.rs` (add keybind-emission branch), `crates/scene/src/chord.rs` (serialize chord back to string), rendered layout doc's `keybinds { }` block.
Effort: **M** — 1 opus subagent session.
Blocking: None (T-064 chord parse done, T-073 auto-mount done).

### Packet S-B — ark-bus bridge revival (v0.2-blocking)
Tasks: **T-070**, **T-071**.
Shared touchpoints: `crates/cli/src/commands/` (new `bus.rs`), `crates/plugins/ark-bus/src/lib.rs` (dispatch_intent + spawn_emit), supervisor control-socket resolution.
Effort: **M** — 1-1.5 opus subagent sessions. v0.2-backlog item #1.
Blocking: T-065 (if binds are compiled, they'll hit the broken dispatch).

### Packet S-C — Strict-mode body + v1 docs (Tier 15)
Tasks: **T-135**, **T-136**, **T-137**, **T-138**.
Shared touchpoints: `crates/cli/src/commands/scene/check.rs` (strict body), `context/refs/intent-api-v1.md`, `context/refs/wasm-metadata-v1.md`, `context/refs/extension-protocol-v1.md`.
Effort: **M** — 1 opus subagent session (3 docs + 1 CLI body).
Blocking: None.

### Packet S-D — Facet-SHAPE migrations (Tier 5/6)
Tasks: **T-053** (op KDL SHAPE), **T-057** (event field SHAPE), **T-027** (ViewMeta shape).
Shared touchpoints: `crates/types/src/event.rs` (add `#[derive(Facet)]` to CoreEvent), `crates/scene/src/validate/event_fields.rs` (replace hardcoded list), `crates/scene/src/view/mod.rs` (widen config_schema), `crates/scene/src/intent.rs` (plumb op Args SHAPE).
Effort: **L** — 2 opus subagent sessions (facet-kdl sibling-Vec limitation F-001 may block field validation migration — check first).
Blocking: F-001 facet-kdl discriminator (deferred T-046 in phase-2 ledger).

### Packet S-E — Reload wiring finish (Tier 14)
Tasks: **T-133** (notify runtime), **T-128** (turn-inflight wire to claude-code-ext hook completion events).
Shared touchpoints: `crates/scene/src/reload.rs`, new file-watch thread in supervisor, claude-code-ext hook-event integration for "turn done".
Effort: **M** — 1 opus subagent session.
Blocking: None (since ACP is cut, turn-inflight can use cc-hook signals instead).

### Packet S-F — Reconciler drift integration test (Tier 4)
Tasks: **T-044**.
Shared touchpoints: `crates/scene/tests/reconciler.rs` — add PTY-backed test using `crates/test-fixtures` harness.
Effort: **S** — 0.5 opus subagent sessions.
Blocking: None.

### Packet S-G — Layout migration (Tier 12)
Tasks: **T-114**.
Shared touchpoints: `crates/mux/zellij/layouts/*.kdl` → `crates/mux/zellij/scenes/*.kdl`.
Effort: **S**. Scan + wrap + test.
Blocking: None.

### Packet S-H — Wasm transport (v0.2-blocking)
Tasks: **T-084**.
Shared touchpoints: `crates/ark-ext-proto/src/transport/`, `crates/ark-view/src/typed.rs::PaneAttrs` widening, `crates/plugins/ark-bus/src/lib.rs`.
Effort: **L** — 2-3 opus subagent sessions. v0.2-backlog item #2.
Blocking: Packet S-B (needs real ark bus subcommand first).

### Suggested dispatch order

1. **S-F** (drift test — unblocks integration-test coverage).
2. **S-A** (keybind compile — unblocks S-B fully).
3. **S-B** (ark-bus bridge — unblocks v0.2 tag).
4. **S-E** (reload wiring — unblocks hot-reload UX).
5. **S-D** (facet SHAPE migrations — long tail + F-001 unblock).
6. **S-C** (v1 docs + strict mode — final polish).
7. **S-G** (layout migration — low priority).
8. **S-H** (Wasm transport — v0.2-scope, defer).

## Key uncertainties

1. **T-114 Port shipped layouts**: Did not glob `crates/mux/zellij/scenes/*.kdl` vs `layouts/*.kdl`. Verification needed before marking DONE.
2. **T-053 Op schema via SHAPE**: The kit says "each op's Args struct has a reflected KDL schema" — facet-kdl parses via derive, which IS a form of SHAPE-based validation. Could be reclassified DONE depending on interpretation. Marked PARTIAL conservatively.
3. **T-128 Turn-inflight gate**: `ReloadQueue` is fully functional as a generic gate. The "ACP-wired" criterion is voided by the pivot, so this could reasonably be DONE-by-scope-change. Marked PARTIAL conservatively — rewiring to cc-hook turn events is the honest replacement.
4. **T-135 v1-strict** depends on what v1 contract means post-pivot. Kit R13 just says "strict mode" — the CUT of ACP changes the v1 surface. The placeholder flag works; body content depends on answering "what IS v1 now?"
