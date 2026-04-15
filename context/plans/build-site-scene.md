---
created: "2026-04-16"
last_edited: "2026-04-16"
---

> **Stack decision locked (2026-04-16):** `facet` + `facet-kdl` replaces `knuffel`. Knus author confirms facet-kdl is the successor. Runtime reflection via facet SHAPE drives parse, type safety, LSP hover/completion, and schema generation from a single derive. See `cavekit-scene.md` Design Decisions Locked table for full stack.

# Build Site: Scene — Reactive KDL + Extension System

## Why this exists

`cavekit-scene.md` (R1–R15) specifies a KDL-based reactive configuration artifact (the **scene**) and an extension system (wasm + optional sidecar fragment) that together give ark nvim-class extensibility. Today ark has: minijinja-templated zellij layout files, a hardcoded `Vec<HookEntry>` consumer for `AgentEvent` broadcast, wasm plugins hand-wired into orchestration, no runtime composition. This site closes that gap.

The work is large (~100 tasks) and phases into six user-visible milestones. **Tier numbering was rearranged after the ACP integration + extension protocol split; this table supersedes any prior mapping.**

| Milestone | What ships | Tier coverage |
|---|---|---|
| v0.1 — Scene | Scene grammar, reactions, keybinds, ark-bus plugin, layout compile, intent registry, plugin lifecycle. Inline picker+status plugins (pre-extension). No extensions. No ACP (uses existing engine-claude-code hook path). | T-0 through T-8 |
| v0.2 — Composition | `extends`, `include`, scene-search-path, `clear-*` directives, composition merge rules. | T-9 |
| v0.3 — Extensions + ACP | Extension protocol (R16), ACP client (R17), `use`, wasm metadata, local ext resolution, shipped ACP engines (claude/codex/gemini-cli), picker+status ported to extensions. Default scene migrates from inline plugins to `use`-based form (silent; inline compat retained indefinitely). | T-9.5 + T-ACP + T-10 |
| v0.4 — Declared capabilities | Capability declarations in `ExtensionMetadata`, install-time disclosure of requested caps. (Runtime enforcement deferred.) | T-13.3 through T-13.6 |
| v0.5 — Hot reload + package mgr + trust | `reload_scene` op + file-watcher, `ark ext add github:…` / `path:` / `url:`, publisher-trust prompt + audit trail, `ark scene graph` / `explain` polish. | T-11 + T-12.9–T-12.11 + T-13.1–T-13.2 |
| v1.0 — Freeze | `ark.core.*` intents frozen; extension protocol v1 locked for 1.x; formal spec docs. | T-15 |

Each milestone is independently shippable. v0.1 already delivers the core value prop (reactive config) against the existing engine path; v0.3 is the "real" coding-agent release (ACP + extensions land together).

## Cavekit traceability

All requirements are defined in `context/kits/cavekit-scene.md`:

- **R1** (scene file grammar) — T-0.x, T-1.x
- **R2** (scope rules) — T-1.x
- **R3** (layout compilation) — T-3.x
- **R4** (reactions) — T-5.x
- **R5** (keybinds) — T-4.x
- **R6** (plugin lifecycle) — T-5.x
- **R7** (op vocabulary) — T-4.x
- **R8** (CEL expressions) — T-2.x
- **R9** (templating) — T-2.x
- **R10** (extensions) — T-7.x, T-8.x
- **R11** (composition + merge) — T-6.x, T-9.x
- **R12** (diagnostics) — T-1.x, T-13
- **R13** (CLI surface) — T-12.x
- **R14** (hot reload) — T-11.x
- **R15** (migration) — T-14.x
- **R16** (extension protocol / runtime RPC) — T-10.x, new protocol-definition sub-tier
- **R17** (ACP integration) — new Tier ACP

Cross-refs: `cavekit-mux-zellij.md` R4/R5 (pipe, layout rendering), `cavekit-supervisor.md` R3 (event bus), `cavekit-types-state-events.md` R3 (AgentEvent), [ACP spec (external)](https://agentclientprotocol.com).

## Three-layer vocabulary (locked 2026-04-16)

| Layer | What | Owner |
|---|---|---|
| **Scene** | User KDL config file | Ark |
| **Extension protocol** | Runtime JSON-RPC ark↔extensions (in-proc / subprocess / wasm-component) | Ark |
| **ACP** | Agent Client Protocol — editor↔coding-agent open standard | External (we adopt) |

Engines are ACP agents, NOT ark extensions. A single "engine extension" that wraps a non-ACP tool (e.g., aider adapter) is itself an ark extension that speaks the extension protocol AND spawns an ACP-speaking subprocess.

## Open questions — RESOLVED (2026-04-16, research-backed)

1. **ark-bus plugin mount type** → **hidden (zellij suppressed-pane API, first-class).** Precedent: zellij-autolock. Not a floating-geometry hack.
2. **CEL matches** → **canonical `matches(str, regex)` via CEL stdlib (RE2, no ReDoS) + custom `glob(str, pattern)`.** Both available.
3. **Scene file discovery** → **two axes.** `ARK_APPNAME` env for profile isolation (NVIM_APPNAME pattern). `--scene NAME` / `ARK_SCENE` for scene selection within a profile. Multi-scene, named; default `default`.
4. **`UserEvent { name, payload, source }`** → **confirmed.** `source` field enables LSP attribution + `ark scene explain`.
5. **Capability model** → **phased, not single strict cut.** v0.3 publisher trust prompt (VSCode 1.97 analog). v0.4 declared caps in SHAPE (Chrome analog). v0.5+ runtime enforcement.

## Tasks

### Tier 0 — Foundations

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-0.1 | New crate `crates/scene/` in workspace. Cargo.toml with deps: `facet = "0"`, `facet-kdl = "0"`, `kdl = "6.5"` (formatter only), `miette = { version = "7", features = ["fancy"] }`, `cel-interpreter = "0"`, `minijinja = "2"`, `thiserror`. Re-export from `ark_scene`. Drops: `knuffel`, `kdl-schema-check` (runtime schema derived from facet SHAPE). | scene | R1 | none | S |
| T-0.2 | Core AST types in `crates/scene/src/ast.rs`: `SceneNode`, `LayoutNode`, `TabNode`, `PaneNode`, `PluginNode`, `OnNode`, `KeybindNode`, `UseNode`, `ExtendsNode`, `IncludeNode`, `ClearNode`. All with `#[derive(Facet)]`. Rust doc-comments on every field — these surface as LSP hover docs via SHAPE reflection. Spans propagate automatically through facet-kdl. | scene | R1 | T-0.1 | M |
| T-0.3 | Add `UserEvent { name: String, payload: serde_json::Value, source: String }` variant to `AgentEvent` in `crates/types/src/event.rs`. `source` values: `"scene"`, `"ext:<name>"`, `"hook:<name>"`, `"agent"`, `"core"` — drives `ark scene explain` attribution. Update serde tag mapping (`#[serde(rename = "user_event")]`). Update `event.rs` tests + schema snapshot. | types | R4, R8 | none | S |
| T-0.4 | `crates/scene/src/error.rs`: error hierarchy with `SceneError` enum, `miette::Diagnostic` impl per variant, full error-code enum per R12: `scene/parse`, `scene/grammar`, `scene/misplaced-node`, `scene/unknown-node`, `scene/duplicate-node`, `scene/plugin-ambiguous-lifecycle`, `scene/ext-not-used`, `scene/ambiguous-file-shape`, `scene/empty-or-unknown`, `scene/include-cycle`, `scene/engine-conflict`, with `help()` and `labels()`. Cross-file errors use `Diagnostic::related` with per-file `NamedSource` (no aggregator type needed). Unit test per variant validating snapshot output. | scene | R12 | T-0.1, T-0.2 | M |
| T-0.5 | `SceneId` type in `crates/scene/src/id.rs`: `SceneId { path: PathBuf, content_hash: blake3::Hash }`. Drives hot-reload delta detection, `ark scene graph` attribution, compile-cache keying. Add `blake3 = "1"` to crate deps. Display impl formats as `<path>#<hash-prefix-8>`. | scene | R4, R14 | T-0.1 | S |

### Tier 1 — Parser + grammar

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-1.1 | `crates/scene/src/parse.rs`: `parse_scene(src: &str, path: &Path) -> Result<SceneIR, SceneError>`. Uses `facet_kdl::from_str::<SceneNode>`. Span-preserving via facet-kdl's source-location tracking. Secondary `kdl::KdlDocument` pass only for formatter round-trip (not validation — validation is via facet types). | scene | R1 | T-0.2, T-0.4 | M |
| T-1.2 | Scope-rule enforcement pass: walk SceneIR, reject misplaced nodes. Emit `error[scene/misplaced-node]` with parent context. Test matrix: `on` inside `layout`, `tab` at scene root, `when=` on `on` block, `intent=` in `on` block body, etc. Every combination in R2 table verified. **Contingency:** facet-kdl's variant-rejection error surface is expected to cover most misplacements (spanned + typo-suggesting per facet docs); if insufficient during impl, add a loose `kdl::KdlDocument` first pass for structural validation (+M effort). | scene | R2 | T-1.1 | M |
| T-1.3 | "Did-you-mean" typo suggestions. facet provides this natively on deserialization failure for field names. Extend with `strsim::jaro_winkler` for cross-type suggestions (unknown scene-root nodes, unknown op verbs, unknown extensions, unknown intent refs). Threshold 0.75; suggest top 3. | scene | R1, R12 | T-1.2 | S |
| T-1.4 | Snapshot testing infrastructure using `insta`: one fixture KDL file per diagnostic scenario; snapshot of `miette` rendered output (stripped of ANSI color). 15+ fixtures covering R1, R2. | scene, testing | R1, R2, R12 | T-1.3 | M |
| T-1.5 | Build binary `gen-scene-schema` in `crates/scene/src/bin/` that dumps the full scene grammar as a `scene.kdl-schema` file by walking facet SHAPE for every AST type. Schema emission is pure reflection; no hand-maintained document. CI check regenerates + diffs vs committed schema to catch drift. Ships at `crates/scene/share/scene.kdl-schema` for editor consumption. **Phase A (ships now):** structural schema — node names, types, required/optional children — drives editor completion. **Phase B (follow-up):** validation constraints via facet custom attributes `#[facet(kdl_pattern = "…", kdl_enum = [...])]` — drives editor diagnostics. Phase B tracked as a separate enhancement; Phase A unblocks downstream. | scene | R1, R13 | T-0.2 | M |

### Tier 2 — Expressions + templating

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-2.1 | `crates/scene/src/cel.rs`: thin wrapper over `cel-interpreter`. `compile(expr: &str) -> Result<Program, SceneError>`; `eval(&Program, &Context) -> Result<Value, SceneError>`. CEL errors translated to `error[cel/*]` diagnostics. | scene | R8 | T-0.1 | M |
| T-2.2 | Context builder: `build_context(event: &AgentEvent, payload: Option<&Value>, agent: &AgentSnapshot, session: &SessionSnapshot) -> cel::Context`. **Event shape follows k8s CEL idiom:** `event.kind` is always present (variant snake_case discriminator); variant-specific fields flat-mapped onto `event.*` (e.g., `event.to` on `PhaseTransition`). Accessing a field absent on the current variant is a CEL error — guarded in user expressions by short-circuit `event.kind == "phase_transition" && event.to == "review"`. Matches k8s admission-policy muscle memory. Unit tests for every `AgentEvent` variant. | scene | R8 | T-2.1, T-0.3 | M |
| T-2.3 | CEL functions. Use canonical `matches(str, regex)` from CEL stdlib (RE2-backed, no ReDoS). Register custom functions: `glob(str, pattern)` via `globset`, `starts_with`, `ends_with`, `contains`, `size`. Registered via `cel-interpreter`'s `add_function` API. Unit tests per function. | scene | R8 | T-2.1 | S |
| T-2.4 | Compile-time template rendering: wrapper around existing `layout_template.rs` minijinja. Input: `(template: &str, vars: LayoutVars) -> String`. Strict-undefined, error propagates via `miette`. Keep existing 5-var surface. | scene, mux-zellij | R9 | T-0.1 | S |
| T-2.5 | Runtime template rendering: new minijinja environment with `UndefinedBehavior::Chainable` (supports `{{ payload.a.b.c }}` on absent chain, renders empty string without fail); context `(event, payload, agent, session)`. Debug-log every undefined-access trail for user diagnosis via `ark pane log`. Shared engine + context builder with T-2.2. | scene | R9 | T-2.2, T-2.4 | S |
| T-2.6 | Compile-pass template/CEL validation: walk SceneIR, pre-compile every `when=` and `if=` expression + every template string. Early errors surface at `ark scene check`. **Static guards on CEL:** `max_expression_length = 4096` bytes per expression, `max_ast_depth = 64`. These bound adversarial-predicate slowness without runtime cost tracking (cel-interpreter has no budget API; CEL non-Turing-completeness + input limits suffice v1). | scene | R8, R9 | T-2.1, T-2.4, T-2.5 | M |

### Tier 3 — Layout compile

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-3.1 | `crates/scene/src/compile/layout.rs`: lower `LayoutNode` AST → zellij KDL string. **Use `kdl::KdlDocument` builder API** (not string concat) — guarantees correct escaping + formatting + post-build validation. Pass-through attrs zellij owns (name, command, args, size, …). Strip ark attrs (`when=`). Preserve structure. | scene, mux-zellij | R3 | T-0.2, T-2.4 | M |
| T-3.2 | Conditional branch resolution: evaluate `when=` CEL expression against **static compile-time context only** — `agent.{cwd, cmd, args, id, name, orchestrator, engine}`, `session.{name}`. No `phase`, no `event`, no `payload` (none exist yet at spawn). Prune false branches. Log retained/pruned count at debug. Document: dynamic predicates belong in reaction `if=`, not layout `when=`. | scene | R3, R8 | T-3.1, T-2.2 | S |
| T-3.3 | **DEFERRED to v0.2 (post-v0.1 milestone).** Dynamic `when=` reactivity requires either `override-layout` (wipes session state — scrollback, cursor) or fine-grained pane diffing (much harder). Most use-cases (orchestrator/engine branching) are static; defer until a concrete dynamic-layout user-need surfaces. Layout `when=` in v0.1 is evaluate-once-at-spawn. | scene, mux-zellij | R3 | — | — |
| T-3.4 | Output writer: write rendered KDL to `${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl`. Reuse `layout_writer.rs::write_rendered` for extension enforcement + 0600 perms. Validate output parses via `kdl::KdlDocument::parse`. | scene, mux-zellij | R3 | T-3.1 | S |
| T-3.5 | Integration with existing `ZellijMux::create_tab`: optional scene path in `AgentSpec`; supervisor compiles scene → writes KDL → mux uses path. **Three-tier fallback chain:** (1) `--scene NAME` explicit → load `<name>.kdl`; (2) no flag, `$CONFIG/scenes/default.kdl` exists → load it; (3) no default scene → legacy `--layout <stem>` path with T-14.1 auto-wrap. Covers both greenfield scene users + existing layout-only users with zero migration friction. | scene, mux-zellij, supervisor | R3, R15 | T-3.4 | M |

### Tier 4 — Intent registry + ops

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-4.1 | `crates/scene/src/intent.rs`: `async fn Intent::dispatch(&self, args: Self::Args, ctx: &IntentContext) -> Result<Option<IntentValue>, IntentError>`. Per-op typed `Args` struct (`#[derive(Facet)]`) parsed by facet-kdl at scene parse. `IntentRegistry` with `register()` + `dispatch_dyn(name, kdl_args, ctx)`. Thread-safe (`RwLock<HashMap>`). `IntentContext` bundles `{mux: Arc<ZellijMux>, bus: EventBus, supervisor: SupervisorHandle, scene_id: SceneId, origin: ReactionOrigin}`. | scene, supervisor | R7 | T-0.1, T-0.5 | M |
| T-4.2 | Core ops as `Intent` impls under `crates/scene/src/ops/`, grouped by subject: `tabs.rs` (open_tab/close_tab/rename_tab/focus_tab), `panes.rs` (split_pane/close_pane), `plugins.rs` (mount_plugin/unmount_plugin), `messaging.rs` (pipe/emit/set_status), `control.rs` (exec/reload_scene). Ops 1–13 of R7 across 5 files. Registered into the namespace `ark.core.*`. Each op's Args type is a facet-derived struct. ACP-interaction ops (14–17) land in Tier ACP, not here. | scene, mux-zellij, supervisor | R7 | T-4.1 | L |
| T-4.3 | Op **cross-reference** validation at scene compile (type/presence comes free via facet-kdl parse into typed Args): walk `on`/`keybind` bodies; `split_pane into="tab:X"` → verify X declared in `layout { tab name="X" }`; `pipe plugin="Y"` → verify Y declared in `plugin "Y" { }`; `mount_plugin name="Z"` → verify Z exists. Error: `error[op/unresolved-ref]` with span on offending attribute + available-refs suggestion. | scene | R7, R12 | T-4.2, T-1.4 | M |
| T-4.4 | Op runtime templating: at dispatch, render every string arg with minijinja runtime context (T-2.5, `Chainable` mode). `exec script="cargo test {{ payload.filter }}"` resolves at fire time. Undefined-chain → empty string + debug log. | scene | R7, R9 | T-4.2, T-2.5 | S |
| T-4.5 | Op dispatch sequencing + idempotency + fail-fast. **Idempotency policy per op (document in intent schema):** `open_tab` if-absent-focus-else-create; `close_tab`/`rename_tab`/`focus_tab`/`close_pane`/`unmount_plugin`/`reload_scene` idempotent-noop-on-absent; `mount_plugin` launch-or-focus (zellij primitive already); `split_pane`/`pipe`/`emit`/`exec`/`set_status` always-side-effect. Fail-fast: op failure logs `error[op/failed]` with reaction origin + op kind + error; remaining ops in that reaction skipped; event loop continues. Per-op `if_exists="focus|create|error"` override deferred to v0.2. | scene | R4, R7 | T-4.2 | S |

### Tier 5 — Reactions runtime

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-5.1 | `crates/scene/src/reactions.rs`: `ReactionRegistry` with primary index by `EventKind`; secondary index for `UserEvent:<name>` by namespaced name (avoids linear-scan across all UserEvent reactions). Each entry: `{selector, predicate: Option<cel::Program>, ops: Vec<CompiledOp>, origin: ReactionOrigin}`. Populated at scene compile. | scene | R4 | T-4.1, T-2.1 | M |
| T-5.2 | Event selector parser + matcher: parse `"<EventKind> field=\"val\""` → `EventSelector {kind, field_patterns}`. Matcher evaluates field patterns against live AgentEvent. `UserEvent:<namespace.name>` matches on name field + looks up in secondary index. | scene | R4 | T-5.1 | M |
| T-5.3 | `ReactionDispatcher` consumer: replaces `hook_dispatcher` in `crates/core/src/consumers/`. Subscribes to `broadcast<AgentEvent>`, looks up reactions by kind (+ name for UserEvent), filters by selector + CEL predicate, dispatches matching op lists. | scene, supervisor | R4 | T-5.1, T-5.2, T-2.2, T-4.5 | L |
| T-5.4 | Cascade depth bounding: per-event-chain counter incremented on `emit`; exceeds bound → error log + drop. Bound configurable **per scene** via top-level `scene "<name>" max-cascade-depth=<N> { … }` attribute; default 4. | scene | R4 | T-5.3, T-0.2 | S |
| T-5.5 | Compile-time cycle detection + `emit` variant restriction. **Scene author can `emit "UserEvent:<name>"` only — not core variants.** Compile-error for `emit "Started"`, `emit "Failed"`, etc. (core events come from agent/supervisor/plugins). This makes cycle detection tractable: build DAG of user-event emit-targets → on-selectors; detect cycles A→B→A. Warn at `ark scene check`; error if `--strict`. Also enforces `UserEvent.source` canonical values per R4 attribution convention. | scene | R4 | T-0.3, T-1.1, T-5.1 | M |
| T-5.6 | Reaction-firing telemetry via `tracing` target `scene::reactions` at `debug` level. Records `{reaction_origin, event_kind, event_name?, ops_run, status}`. Users enable via `RUST_LOG=scene::reactions=debug`. Shares supervisor log pipeline; no new file. | scene, supervisor | R4, R12 | T-5.3 | S |
| T-5.7 | Migration: remove hardcoded `hook_dispatcher` in `crates/supervisor/src/orchestration.rs:163`. Ported hook config (TOML `[[hooks]]`) compiles to a synthetic scene fragment appended to user scene at compile. **Mapping:** `[[hooks]] on="started", command=["foo","bar"]` → `on "Started" { exec script="foo" args="bar" }`. Preserves zero-migration for existing users. | scene, supervisor, config | R4, R15 | T-5.3 | M |

### Tier 6 — Keybinds + ark-bus plugin

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-6.1 | New crate `crates/plugins/ark-bus/` — headless zellij wasm plugin. Cargo.toml with `crate-type = ["cdylib"]`, `zellij-tile = "0.*"`. `src/lib.rs` skeleton: `ZellijPlugin` impl, register_plugin! macro. Build via `cargo build --target wasm32-wasip1 -p ark-bus`. | scene, plugins | R5, runtime | none | M |
| T-6.2 | ark-bus intent dispatch via **hidden-command-pane bridge**. Pipe endpoint `ark-intent` receives JSON `{intent: <name>, args: <map>}` OR `{ops: [<op>…]}`. ark-bus spawns a hidden command pane running `ark-hook intent --json "<payload>"`; `ark-hook` (existing binary) connects to supervisor's control socket (`cavekit-hook-ipc.md` R1) and dispatches through intent registry (T-4.1). ~10ms per call overhead, acceptable for keybind UX. Zero new socket architecture. | scene, plugins, supervisor | R5 | T-6.1, T-4.1 | L |
| T-6.3 | ark-bus event forwarder (same bridge as T-6.2): subscribes `CommandPaneOpened`, `CommandPaneExited`, `PaneClosed`, `FileSystemUpdate`; on match, spawns hidden `ark-hook emit --event '<json>'` which routes to supervisor via control socket; supervisor broadcasts as `AgentEvent::UserEvent { name: "ark.zellij.<kind>", payload: {…}, source: "ext:ark-bus" }`. Closes the "no pane-lifecycle CLI stream" zellij gap. | scene, plugins | R4, runtime | T-6.1, T-0.3, T-6.2 | M |
| T-6.4 | ark-bus rebind endpoint: receives `{rebind: [{key, action}…]}`, invokes zellij-tile `rebind_keys` shim. Used by `reload_scene` keybind diff. | scene, plugins | R14 | T-6.1 | M |
| T-6.5 | Keybind compilation: walk `SceneIR.keybinds`; for each, build a `MessagePlugin "ark-bus" { name "ark-intent"; payload <JSON>; }` KDL action block. Aggregate into a `keybinds { }` block at top of rendered layout (NOT inside `layout { }`). Zellij merges additively with user config (no `clear-defaults`, per B2 research). | scene, mux-zellij | R5 | T-6.2, T-3.4 | M |
| T-6.6 | **Loose** chord-string validation at scene compile: grammar `(Mod )*KEY` where Mod ∈ {Ctrl, Alt, Shift, Super} and KEY is alphanumeric or single zellij-known special (Tab, Enter, Space, F1–F12, arrow names). Reject clearly-invalid forms at compile. Finer errors (unknown KEY, unsupported combo) surface at first zellij session-spawn via miette. Tradeoff: less strict compile-time validation in exchange for zero maintenance burden as zellij chord grammar evolves. | scene | R5, R12 | T-6.5 | S |
| T-6.7 | ark-bus auto-mount: scene compiler injects `plugin "ark-bus" { source "shipped:ark-bus"; mount "hidden" }` IF ANY: (a) any `keybind` declared; (b) any `on` subscribes to zellij-side events (`CommandPaneExited`, `PaneClosed`, `FileSystemUpdate`); (c) any plugin's `subscribes` includes zellij-side events. Skip injection for pure-AgentEvent scenes (save one plugin load). `mount "hidden"` = zellij first-class suppressed-pane API, not a geometry hack. Precedent: zellij-autolock. | scene | R5, R6 | T-6.1, T-6.5 | S |

### Tier 7 — Plugin lifecycle

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-7.1 | `crates/scene/src/plugin.rs`: `PluginDecl { name, source, mount, lifecycle, subscribes, config }`. Lifecycle parse from body: inference rules per R6 — always (neither summon nor on-trigger), summon (has `summon`), event-mount (has plugin-level `on` trigger). Both `summon` and `on` set simultaneously = `error[scene/plugin-ambiguous-lifecycle]` with caret on both attrs. | scene | R6 | T-0.2 | M |
| T-7.2 | `PluginLifecycleManager` in supervisor: registers always-on plugins at session spawn via ark-bus → `launch-or-focus-plugin`. Tracks `{name → MountState::{Mounted{pane_id} \| Dormant \| Failed{reason}}}`. On mount failure (wasm parse error, version mismatch): log `error[plugin/mount-failed]`; emit `AgentEvent::UserEvent { name: "ark.plugin.failed", payload: {plugin, reason}, source: "core" }` so scene can react (alt mount, set_status). | scene, supervisor, mux-zellij | R6 | T-7.1, T-6.2 | M |
| T-7.3 | Summon lifecycle: register plugin's `summon` selector as event/intent listener; match → check mount state — if Dormant, `launch-or-focus-plugin` → Mounted; if already Mounted, no-op (launch-or-focus is zellij-native idempotent). `dismiss` selector → if Mounted, `close-pane` + set Dormant; if Dormant, no-op. | scene, supervisor | R6 | T-7.2, T-5.3 | M |
| T-7.4 | Event-mount lifecycle: plugin's `on "<selector>"` registered as event-bus subscriber. First match → `launch-or-focus-plugin` + Mounted state. Subsequent matches while Mounted → focus (zellij-native). `dismiss` match (if declared) → close + Dormant. | scene, supervisor | R6 | T-7.2, T-5.3 | M |
| T-7.5 | `subscribes` forwarding: after plugin mount, register selectors with event bus; on match, ark-bus pipes the event to plugin via `zellij pipe --plugin <url> --name ark-event -- '<json>'`. **Payload format = identical to `events.jsonl` (serde_json of `AgentEvent`)**, one event per pipe message. Plugins deserialize via their own schema. | scene, supervisor, plugins | R6 | T-7.2, T-6.3 | M |
| T-7.6 | Config handoff (v0.1 scope): shipped plugins (picker, status, ark-bus) declare Config struct via `register_extension!` macro in the compiled-in path — schema available from facet SHAPE without wasm inspection. At scene compile, validate user's `config { }` block keys + values against the in-proc schema; type mismatch = `error[plugin/bad-config]` with span. At `launch-or-focus-plugin`, serialize typed Config (facet) to zellij's stringly `HashMap<String, String>` via facet's flat-string mode. v0.1 ships this path only; wasm-cartridge config schemas (from `ark.metadata` custom section) land in T-10.2 + T-10.5 at v0.3, and unknown config keys degrade to untyped pass-through until then. | scene, mux-zellij, plugins | R6, R10 | T-7.2 | M |

### Tier 8 — v0.1 integration + test

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-8.0 | **Scene path resolver as pure function.** `resolve_scene_path(flag: Option<&str>, env_scene: Option<&str>, env_appname: Option<&str>, cwd: &Path) -> Result<PathBuf>` in `crates/scene/src/path.rs`. Precedence: (1) --scene NAME; (2) ARK_SCENE env; (3) `./.ark/scene.kdl`; (4) `$XDG_CONFIG_HOME/<appname>/scenes/default.kdl` (appname default `ark`, override via ARK_APPNAME); (5) built-in default scene compiled into binary. **v0.1 built-in default: inline-declares `plugin "picker" { source "shipped:picker"; mount "floating" }` + `plugin "status" { source "shipped:status"; mount "status-bar" }` + keybind + reactions** (no `use` — `use` ships at v0.3). T-10.10 migrates the built-in default to `use "picker"` + `use "status"` form silently; inline form remains supported indefinitely (Rust-editions precedent). Unit tests per rung. | scene, types | R13 | T-0.1 | S |
| T-8.1 | Wire scene compile into supervisor orchestration (`crates/supervisor/src/orchestration.rs`). **Ordering rewrite:** parse spec → resolve scene path (T-8.0) → compile scene (parse + resolve extensions + merge fragments + validate + register intents/reactions) → build event bus subscribers (scene dispatcher replaces `hook_dispatcher` at line 163) → render layout KDL with keybinds + auto-injected ark-bus → launch zellij → register always-on plugins → agent Started fires → reactions process. Bigger refactor than originally scoped (L, not M). | scene, supervisor | R3, R4 | T-3.5, T-5.3, T-5.7, T-6.7 | L |
| T-8.2 | Add `scene: Option<PathBuf>` to `AgentSpec`. CLI: `ark spawn --scene NAME`, `ARK_SCENE` + `ARK_APPNAME` env vars (NVIM_APPNAME analog). Uses T-8.0 resolver. If no scene found at any rung → auto-wrap legacy `--layout <stem>` per T-14.1 (zero-migration). | scene, cli, types | R13, R15 | T-8.0 | S |
| T-8.3 | E2E test `crates/cli/tests/scene_e2e.rs::scene_reactions_fire`: fixture scene with `on "Started" { exec script="touch <tmp>/fired" }`. Spawn agent via `--no-detach`; within 2s assert `<tmp>/fired` exists. Pure filesystem observability — avoids status-bar/pipe chain dependency. Gated on zellij-on-PATH. | scene, cli, testing | R4, R7 | T-8.1 | M |
| T-8.4 | E2E test `scene_keybind_dispatches`: fixture scene with `keybind "Alt q" intent="ark.core.close_tab" { name="builder" }`. Dispatch via `zellij action message-plugin ark-bus --name ark-intent --payload '{"intent":"ark.core.close_tab","args":{"name":"builder"}}'` — exercises dispatch path, bypasses key-press simulation. Assert tab closes within 2s via `zellij action list-tabs --output-json`. | scene, cli, testing | R5 | T-8.1 | M |
| T-8.5 | Port shipped `crates/mux/zellij/layouts/builder.kdl` → `crates/mux/zellij/scenes/builder.kdl` as minimal scene: wrap existing content in `scene "builder" { layout { … } }`. **Zero behavior change** — structural migration only. Verify no regression in existing builder tests. T-14.2 handles the other shipped layouts in bulk. | scene, mux-zellij | R15 | T-8.1 | S |
| T-8.6 | Docs: short README in `crates/scene/README.md` covering: what a scene is, one-screen example, `ark scene check` usage, ARK_APPNAME + --scene precedence, link to cavekit-scene.md for the spec. | scene | R13 | T-8.1 | S |

**⬆ Milestone v0.1 shippable here.**

### Tier 9 — Composition (extends, include)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-9.1 | `extends "<name>"`: resolves via **scene-search-path** (distinct from extension-search-path): (1) `./.ark/scenes/<name>.kdl`; (2) `$CONFIG/<appname>/scenes/<name>.kdl`; (3) built-in shipped scenes baked into binary. Load parent first → apply parent contributions → then child contributions (child wins on conflict, matches user-layer-wins semantics). One `extends` per scene. | scene | R11 | T-1.1, T-5.1, T-8.0 | M |
| T-9.2 | `include "<path>"`: splice another KDL fragment at this point. Path **relative to current scene file** (not scene-search-path — include is for splitting one scene into multiple files). Multiple includes allowed. Cycle detection unified with extends graph (`error[scene/include-cycle]` with trace). | scene | R11 | T-1.1 | M |
| T-9.3 | `clear-reactions selector="<sel>"`, `clear-keybind "<chord>"`, `disable-plugin "<name>"` directives. **Evaluation order:** after parent+included fragments are merged, before own scene's additions apply. Matches intuitive "inherit but drop this piece." Parent-scoped clears cannot drop descendants' contributions (parent doesn't know they exist) — silent noop. Literal selector match v1; glob/regex deferred. | scene | R11 | T-5.1, T-6.5, T-7.1 | S |
| T-9.4 | Merge semantics tests: fixture scenes exercising all rules in R11 table — reactions append in load order, keybinds last-wins, plugin dup errors unless `override=true`, layout tab dup errors. Plus corner: grandparent clear targeting descendant-only pattern (silent noop), cycle detection on extends/include graphs. Snapshot tests via `insta`. | scene, testing | R11 | T-9.1, T-9.2, T-9.3 | M |

**⬆ Milestone v0.2 shippable here.**

### Tier 9.5 — Extension protocol (runtime RPC definition)

This tier defines the ark extension protocol itself — the JSON-RPC contract (R16) that every extension speaks regardless of delivery mode. Must land before Tier 10 (extensions) because extensions implement against it.

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-9.5.1 | `crates/ark-ext-proto/` — canonical Rust trait `ArkExtension` with default-impl methods per R16 method surface (~22 methods grouped: lifecycle, async/cancel, events, intents, UI, host services, logging). Every method has `#[derive(Facet)]` on its request/response types with doc-comments. `#[async_trait]`. | scene | R16 | T-0.1 | L |
| T-9.5.2 | Spec emitter: `cargo run --bin gen-extension-spec` walks the `ArkExtension` trait + types via facet SHAPE and emits a canonical `extension-protocol.kdl` file (ark-side analog of JSON Schema). Ships as the cross-language reference. CI diffs regenerated vs committed to catch drift. | scene | R16 | T-9.5.1 | M |
| T-9.5.3 | JSON-RPC 2.0 framing over NDJSON stdio for subprocess extensions: `crates/ark-ext-proto/src/transport/ndjson.rs`. Bidirectional, request/notification/response, `id` correlation. Request timeout default 5s, overridable per call. `$/cancel` notification cancels in-flight request by id. | scene | R16 | T-9.5.1 | M |
| T-9.5.4 | In-process trait dispatcher for compiled-in extensions: `crates/ark-ext-proto/src/transport/in_proc.rs`. Zero-overhead; `Arc<dyn ArkExtension>` with a registry. Same trait signature as subprocess; no JSON-RPC serialization cost. | scene | R16 | T-9.5.1 | M |
| T-9.5.5 | Handshake + capability negotiation: `initialize` carries `{protocolVersion, clientCapabilities}` → `{protocolVersion, extensionCapabilities, extensionInfo}`. Object-of-objects capabilities per R10 (no boolean soup). Version-mismatch = `error[ext-proto/unsupported-version]`. | scene | R16 | T-9.5.3, T-9.5.4 | M |
| T-9.5.6 | Task handle + progress for long-running operations: `task/create` returns `{taskId}`; `task/get` polls; `task/cancel` aborts. `$/progress` notifications reference `taskId` with 0-100 percent + message. Used by any op that can exceed 5s. | scene | R16 | T-9.5.5 | M |
| T-9.5.7 | Supervision tree for subprocess extensions: shutdown stdin-close → wait 2s → SIGTERM → SIGKILL. Per-extension `SupervisorHandle` tracks pid + log tail for crash diagnostics. Crash → `error[ext/crashed]` log + emit `UserEvent:ark.ext.crashed { name, exit_code, stderr_tail }`. No auto-restart v1. | scene, supervisor | R16 | T-9.5.3 | M |
| T-9.5.8 | Reverse-request gating: `host/fs/*`, `host/proc/*`, `host/net/*` calls check extension's declared capabilities + session token. Unauthorized = `error[ext-proto/capability-denied]`. Token issued at `initialize`; valid only for session lifetime. | scene, supervisor | R16 | T-9.5.5 | M |
| T-9.5.9 | Protocol conformance test harness: `crates/ark-ext-proto/tests/conformance/` — spec suite that both subprocess + in-process dispatchers must pass. Exercises every method, every error code, handshake edge cases. Shipped as reference for third-party extension authors. | scene, testing | R16 | T-9.5.3, T-9.5.4, T-9.5.5 | L |

### Tier ACP — Agent Client Protocol client

Ark's first-class client implementation of ACP. Lands before engines are exposed via scene `engine { }` block.

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-ACP.1 | Add `agent-client-protocol = "*"` crate as workspace dep. Vendored/pinned version tracked. Document in cavekit-distribution the ACP-version-ark-ships. | scene, supervisor | R17 | none | S |
| T-ACP.2 | `crates/acp-client/` — ark's ACP client. Wraps `agent-client-protocol::Client`; translates ACP session events → internal `AgentEvent::UserEvent { name: "ark.acp.<kind>", payload: {…}, source: "core" }`. Consumes `session/update` stream (plan, agent_message_chunk, tool_call, tool_call_update), `session/request_permission`, `fs/*`, `terminal/*`. | scene, supervisor | R17 | T-ACP.1, T-0.3 | L |
| T-ACP.3 | Engine launch spec in scene grammar: `engine { name "claude"; command "claude"; args "--acp"; env { KEY "VAL" } }` parses into `EngineLaunch` facet struct. At most one per scene. Registered as the session's ACP-agent launch config. | scene | R17 | T-0.2 | S |
| T-ACP.4a | Engine resolution chain (rungs 1, 2, 4, 5) — WITHOUT extension-declared engines: (1) `--engine NAME` flag; (2) scene `engine { }` block; (4) config `engines.<name>` in `config.toml`; (5) hardcoded `claude --acp` default. First found wins. Ships with Tier ACP, independent of Tier 10. | scene, supervisor, cli | R17 | T-ACP.3 | M |
| T-ACP.4b | Engine resolution rung 3 (extension-declared engines): scene `use` of extension with `capabilities { agent { engine { speaks "acp" } } }` slots between rung 2 and rung 4. Also enforces **intra-scene mutual exclusion**: scene containing BOTH `engine { }` block AND `use "engine-*"` (extension with engine capability) = `error[scene/engine-conflict]` with carets on both. | scene, supervisor, cli | R17 | T-ACP.4a, T-10.4 | S |
| T-ACP.2b | **ACP-interaction core ops** (R7 ops 14–17): `prompt`, `acp/cancel`, `acp/permit`, `set_mode` as `Intent` impls in `crates/scene/src/ops/acp.rs`. Each maps to the corresponding ACP method via the acp-client crate (T-ACP.2). `acp/cancel` blocks up to 5s for the `stopReason: cancelled` response; `acp/permit` routes to the outstanding `session/request_permission` by JSON-RPC id correlation. Unstable ACP ops (`session/fork`, `nes/*`, `elicitation/*`, etc.) NOT registered in v1; gated behind capability flags. | scene, supervisor | R7, R17 | T-ACP.2, T-4.1 | M |
| T-ACP.2c | **Turn-inflight tracker.** Per ACP session, maintain `turn_inflight: AtomicBool`, set true on `session/prompt` request dispatch, cleared only when corresponding response arrives with any `stopReason` (`end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`). Expose `SupervisorHandle::any_turn_inflight() -> bool` for the reload gate (T-11.1). Stable key = `(session_id, jsonrpc_id)` in the wait table; late responses drop with a debug log. | supervisor | R14, R17 | T-ACP.2 | M |
| T-ACP.5 | Tool-permission dispatch with **Zed 5-tier precedence**: on ACP `session/request_permission`, correlate via JSON-RPC `id`, emit `UserEvent:ark.acp.permission_requested { request_id, tool, params, options }`. Rule eval order: (1) security-deny (v0.4+, stub v0.3), (2) auto-deny scene rule, (3) auto-confirm (force picker), (4) auto-allow scene rule, (5) picker fallback. Reactions respond via `acp/permit` op (T-ACP.2b); picker plugin responds via same op. Ark routes to ACP by request_id. Reactions observable via `ark scene dry-run --event 'ark.acp.permission_requested{...}'`. | scene, supervisor, plugins | R17 | T-ACP.2, T-ACP.2b | L |
| T-ACP.5b | **Permission timeout + late-response handling.** Config key `[acp] permission_timeout_ms` in `config.toml` (default: 300000 interactive; 0 when `ARK_NONINTERACTIVE=1` or stdin is non-TTY). On expiry, respond `outcome: reject_once` with `option_id = "timeout"` (NOT ACP `Cancelled` — that verb is reserved for `session/cancel`-driven aborts). Emit `UserEvent:ark.acp.permission_timeout { request_id, tool }`. Late user responses (arriving after timeout or after `session/cancel`) are dropped silently + debug log; picker plugin checks request validity before sending via `acp/permit`. | scene, supervisor, plugins, config | R17 | T-ACP.5 | M |
| T-ACP.6 | `ark doctor` ACP check: spawns default engine with `--acp` argv; verifies `initialize` round-trip in <1s. Failure = actionable diagnostic (e.g., "claude not on PATH" or "claude rejected --acp — update to ≥ version X"). | cli, supervisor | R17 | T-ACP.2 | S |
| T-ACP.7 | Retire `crates/plugins/engine-claude-code/` hook-injection + transcript-tailing code. Replace with a trivial launch spec `engines.claude = { command = "claude", args = ["--acp"] }` in `config.toml` defaults. **No backwards compat** — engine implementations rewritten. | scene, engine, supervisor | R17 | T-ACP.4a | M |
| T-ACP.8 | Shipped ACP engine launch specs: `claude`, `codex`, `gemini-cli` (all speak ACP natively). Baked into built-in default `config.toml` + documented in `crates/scene/README.md`. | scene, docs | R17 | T-ACP.4a | S |
| T-ACP.9 | (Optional, v0.4+) aider adapter extension: `crates/extensions/engine-aider-adapter/` — subprocess extension speaking both extension-protocol (to ark) and spawning `aider` as subprocess with stdio translation to ACP format. Demonstrates the "non-ACP tool via adapter" pattern. Defer unless user demand. | scene, plugins | R17 | T-ACP.4, T-9.5.9 | L |

### Tier 10 — Extensions (use + wasm metadata)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-10.1 | New crate `crates/ark-ext-metadata-types/` — shared type definitions for `ExtensionMetadata { name, version, ark_range, zellij_range, requires, intents: Vec<IntentDecl>, events: Vec<EventDecl>, config: ConfigSchema, capabilities: Vec<String> }`. All `#[derive(Facet)]` with doc-comments. Imported by both plugin (construct) and core (read). New crate `crates/ark-ext-metadata/` — plugin-side helper + `register_extension!` macro: authors construct an `ExtensionMetadata` value; macro serializes via **facet-kdl** to KDL bytes; bytes written to wasm custom section `ark.metadata` via `.custom_section` linker attr (or `wasm-metadata` crate). Wire format is KDL text — introspectable via `ark ext inspect`, stable across facet version bumps. Unit test: expand, compile to wasm, inspect section, round-trip. | scene, plugins | R10 | none | L |
| T-10.2 | `crates/scene/src/wasm_meta.rs`: read `ark.metadata` custom section from a `.wasm` file using `wasmparser`. **Decode via facet-kdl into `ExtensionMetadata` struct** (shared type from T-10.1). No parallel schema definition; type is single source of truth. Produce typed struct ready for scene compiler consumption. | scene | R10 | T-10.1 | M |
| T-10.3 | Extension search path resolver: walk `./.ark/extensions/`, `${XDG_DATA_HOME}/ark/extensions/`, `/usr/share/ark/extensions/`, built-in list. First match wins. `resolve(name) -> Result<ExtensionPath>`. | scene | R10 | T-0.1 | S |
| T-10.4 | `use "<name>"` resolution + inspection: resolve path via T-10.3, read wasm metadata via T-10.2, register namespaced intents/events in symbol table, parse sidecar `scene.kdl` (optional) via T-1.1 as fragment. Validate version ranges against host. **Re-using `use` for config override:** user can write `use "ext"` multiple times; compiler merges (last-wins for config values, extension-side side-effects computed once). Documented pattern for overriding transitive extensions' config without root-level `use`. | scene | R10 | T-10.3, T-10.2, T-1.1 | L |
| T-10.5 | Config block validation: user `use "<name>" { config { … } }` block parsed via facet-kdl directly into the plugin's declared `Config` struct type (reconstructed from wasm metadata's `ConfigSchema`). Type mismatches → facet's native `error[ext/bad-config]` with span + typo suggestions. Unknown config keys → `error[plugin/unknown-config-key]` with available-keys listed. | scene | R10 | T-10.4 | M |
| T-10.6 | Transitive `use`: if extension's scene fragment contains `use "<other>"`, resolve recursively. Topological sort. Cycle detection (`error[ext/cycle]`). Depth limit 16. | scene | R10, R11 | T-10.4 | M |
| T-10.7 | Namespacing enforcement with **context-sensitive rewrite** of unprefixed names: **in user scenes** unprefixed `intent="foo"` → `user.foo`; **in extension fragments** unprefixed `intent="foo"` → `<ext-name>.foo`; core ops are NEVER the target of an unprefixed rewrite — extension authors must write `intent="ark.core.open_tab"` fully qualified. Avoids Python-style import-shadow footgun. `ark.core.*` reserved; collision = `error[ext/reserved-namespace]`. | scene | R11 | T-10.4 | M |
| T-10.8 | `ark ext inspect <path>`: CLI that dumps wasm metadata in KDL form without executing. Useful for debugging. | scene, cli | R13 | T-10.2 | S |
| T-10.9 | `ark ext list` (tabular: name, version, ark-protocol range, source) and `ark ext info <name>` (full metadata dump for one installed extension — name, version, capabilities, intents, events, config-schema, install-source from `.ark-install` dotfile). Both read from `$XDG_DATA_HOME/ark/extensions/<name>/extension.kdl` via T-10.2. | scene, cli | R13 | T-10.3, T-10.2 | S |
| T-10.10 | Port `crates/plugins/picker` and `crates/plugins/status` to the extension model: add `ExtensionMetadata` declarations via `register_extension!` macro; write sidecar scene fragments with their default mount + lifecycle. **Migrate the built-in default scene (from T-8.0 rung 5):** rewrite the inline `plugin "picker"` / `plugin "status"` blocks to `use "picker"` + `use "status"` form. Both forms compile to identical runtime behavior; keep the inline form parseable indefinitely (Rust-editions / Neovim-Lua-shim convention). On first v0.3 startup, emit a one-time notice "default scene updated to use `use "picker"`" via `set_status severity=info` (not into user's scene file — do not rewrite user files). Validate via e2e that `use "picker"` + `use "status"` in a scene works identically to the v0.1 hardcoded-inline setup. | scene, plugins | R10, R15 | T-10.4, T-10.1 | L |

**⬆ Milestone v0.3 shippable here.**

### Tier 11 — Hot reload

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-11.1 | `reload_scene` op impl: re-parse current scene file, compile, diff against live state, apply deltas in safety order (reactions → keybinds → plugins). **Turn-inflight gate** (via T-ACP.2c `SupervisorHandle::any_turn_inflight()`): if any ACP session has a `session/prompt` awaiting response, queue the reload + emit `UserEvent:ark.scene.reload_pending { reason: "turn-inflight", pending_sessions }`, DO NOT apply. A queued reload fires automatically when every session receives a `stopReason` response. **Re-entry guard:** single-slot mutex `reload_in_progress: AtomicBool`; subsequent `reload_scene` calls while reload active = drop + debug log. Prevents cascade-induced infinite reload. **In-flight-reaction drain:** reactions already dispatching ops at the instant of diff complete their op list against the OLD registry; atomic registry swap happens after the drain — no reaction straddles old + new. | scene, supervisor | R14, R17 | T-5.3, T-6.4, T-7.2, T-ACP.2c | L |
| T-11.2 | Subscription-set diff via **AST-structural hashing** (not raw source bytes). Hash = `blake3(normalized_selector ‖ compiled_predicate_ir ‖ ops_ir)`. Comment-only + whitespace edits must NOT register as reaction changes. Compute add + remove sets; apply atomically to reaction registry. | scene | R14 | T-11.1 | M |
| T-11.3 | Keybind diff: compare old vs new keybind map by chord. Added/removed/changed → batched `rebind_keys` via ark-bus. | scene, plugins | R14 | T-11.1, T-6.4 | M |
| T-11.4 | Plugin lifecycle diff — four cases: (a) `source` changed → `start-or-reload-plugin` (wasm state lost, emit `UserEvent:ark.plugin.reloaded`); (b) `mount` changed → close + relaunch at new target; (c) `config` changed, same source → try zellij's `reconfigure_plugin` API if available (verify zellij-tile 0.44 support); else close + relaunch; (d) `lifecycle` changed (always → summon) → close if mounted, re-register as dormant. | scene, supervisor | R14 | T-11.1, T-7.2 | M |
| T-11.5 | Layout diff conservative v0.1: ANY structural layout change (tab add/remove, pane reorg, `when=` truth flip) → skip layout update + emit `UserEvent:ark.scene.reload_partial { reason: "layout-change", details }`; reactions/keybinds/plugins still apply. User sees "session restart required for layout changes" via status. Fine-grained pane diff = v0.2+. | scene, mux-zellij | R14, R3 | T-11.1 | M |
| T-11.6 | File watcher (optional): `notify` crate watches **resolved scene path** (not parent dir). Debounced 200ms. Ignore editor temp files by suffix pattern: `.swp`, `.tmp`, trailing `~`, leading `.#`, `.bak`. Auto-fire `reload_scene` on accepted change. Config knob `[scene] watch = true` in `config.toml`; default `false` (opt-in). | scene, config | R14 | T-11.1 | M |
| T-11.7 | Reload failure recovery: apply deltas in safety order (reactions → keybinds → plugins). Parse/compile failure before first stage = keep old config fully, emit `set_status severity=error` with miette message, no tear-down. Failure mid-sequence: prior stages remain applied; failed stage + later stages aborted; emit `set_status severity=error` naming which stage failed + `UserEvent:ark.scene.reload_failed { stage, error }` for reactions to observe. | scene | R14, R12 | T-11.1 | S |
| T-11.8 | **Reload telemetry.** Every completed reload emits `UserEvent:ark.scene.reloaded { duration_ms, reactions_added, reactions_removed, keybinds_changed, plugins_changed, status: "ok"\|"partial"\|"failed" }`. Scene authors can observe reload cycles via reactions on this event. Also logged at tracing target `scene::reload`. | scene, supervisor | R14, R12 | T-11.1 | S |

### Tier 12 — CLI surface

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-12.1 | New top-level `ark scene` subcommand tree in `crates/cli/src/commands/scene/`. Sub-subs: `check`, `fmt`, `dry-run`, `graph`, `explain`, `reload`. | scene, cli | R13 | T-1.1 | M |
| T-12.2 | `ark scene check [path]`: full parse + resolve-extensions + validate + CEL-compile + template-check. Exit 0 on green; non-zero with diagnostics on any error. Emits every error, not just first. | scene, cli | R13 | T-12.1, T-2.6 | M |
| T-12.3 | `ark scene fmt [path]`: canonical-format scene file using `kdl` crate's formatter + ark-specific node ordering (extends/include/use → layout → plugin → on → keybind). Idempotent. `--check` flag for CI. | scene, cli | R13 | T-12.1 | M |
| T-12.4 | `ark scene dry-run --event '<selector>' [--payload <json>]`: simulate one event fire against current scene; print resolved op list per matching reaction without side effects. Uses same reaction registry + CEL eval as runtime. | scene, cli | R13 | T-12.1, T-5.3 | M |
| T-12.5 | `ark scene graph [path]`: render attribution tree of extensions, plugins, reactions, keybinds, intents. Each leaf tagged with origin file:line. Default ASCII-tree text output; `--format json` for scripts + future ark-lsp code-lens integration. | scene, cli | R13 | T-12.1, T-10.4 | L |
| T-12.6 | `ark scene explain <ref>`: refs `intent:<name>`, `keybind:<chord>`, `plugin:<name>`, `reaction:<event-selector>`, `ext:<name>`. Prints "defined at <file:line>; overridden by <file:line>; final resolution: <origin>". The `ext:<name>` form lists everything that extension contributed — reactions, plugins, keybinds, intents in scope. Enables "is this behavior from picker or my scene?" debugging. | scene, cli | R13 | T-12.1, T-12.5 | M |
| T-12.7 | `ark scene reload [--session <name>]`: sends `ReloadScene` message to supervisor via control socket (cavekit-hook-ipc R1); handler invokes T-11.1. Reuses existing IPC path — no new socket architecture. | scene, cli, supervisor | R13, R14 | T-12.1, T-11.1 | S |
| T-12.8 | New top-level `ark ext` subcommand tree. Sub-subs: `add`, `remove`, `list`, `update`, `info`, `inspect` (T-10.8). | scene, cli | R13 | T-10.3 | M |
| T-12.9 | `ark ext add <source>`: sources `path:<dir>` (copy), `url:<https-tarball>` (download + extract via `ureq` + `tar`/`flate2`), `github:<user>/<repo>[@<ref>]` (shallow clone via `git2` lib — pure-Rust, no subprocess). Install target `${XDG_DATA_HOME}/ark/extensions/<name>/`. Reads wasm metadata post-install to confirm name + record install source in `.ark-install` dotfile. | scene, cli | R13 | T-12.8, T-10.2 | L |
| T-12.10 | `ark ext remove <name>` / `ark ext update [name]`: remove = `fs::remove_dir_all`; update = re-fetch from `.ark-install` source + re-prompt for new caps if version-bumped (T-13.5). | scene, cli | R13 | T-12.9 | M |
| T-12.11 | `ark scene explain-merge <scene>`: trace composition. Output: "extends base.kdl contributes {reactions: R1, R2}; override.kdl contributes {reactions: R3, keybinds: K1}; user scene contributes {clear-reactions: R2, keybinds: K2}; **final**: reactions={R1, R3}, keybinds={K1 overridden by K2 on Alt-p}." Debug tool for composition issues. | scene, cli | R13 | T-12.1, T-9.1 | M |
| T-12.12 | `ark scene schema-dump [--format kdl\|json]`: emits scene-grammar schema from facet SHAPE. Default kdl (for kdl-lsp consumption); `--format json` for JSON-Schema-based editors. Shipped schema (T-1.5) is this command's output captured at build; CLI form supports dev/debug + future "install kdl-lsp schema" workflow. | scene, cli | R13, R1 | T-12.1, T-1.5 | S |

### Tier 13 — Capabilities (phased)

Phased per research: declared-caps at v0.4 (Chrome-style manifest, no prompt — no install command exists yet), publisher-trust prompt at v0.5 (ships with `ark ext add` install command), runtime enforcement future. **Sequencing corrected 2026-04-16**: at v0.3 users drop extension files manually and `use` them, no install flow exists — so there is nothing to prompt on.

#### v0.4 — Declared capabilities (no enforcement)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-13.3 | `capabilities: Vec<String>` field in `ExtensionMetadata` facet struct (T-10.1). Values from `{exec, fs-read, fs-write, pipe, network, hook}`. Empty = no special caps. Read at ext inspection via SHAPE. | scene, plugins | R10 | T-10.1 | S |
| T-13.4 | Install-time capability disclosure: `ark ext add` reads declared caps from wasm SHAPE; prints "requests: exec, pipe, network"; prompt accept. Chrome-extension analog. Stored in trust file alongside publisher trust. | scene, cli | R10 | T-13.1, T-13.3 | M |
| T-13.5 | Version-bump re-prompt: installing `foo@1.2` when `foo@1.1` was trusted with caps `{pipe}` but 1.2 adds `{exec}` = re-prompt for the new cap only. Trust file tracks per-version caps. | scene, cli | R10 | T-13.4 | M |
| T-13.6 | Declare-only enforcement: scene compiler reads trust file; if a loaded ext's declared caps are not trusted, emit warning + continue. No runtime gating yet (wasm host-functions still unrestricted). | scene | R10 | T-13.4 | S |

#### Future — Runtime enforcement (v0.5+, out of this site)

Runtime capability enforcement (gating wasm host-function imports based on trust file) is deferred — requires zellij-side plugin-permission integration and is a separate design piece.

#### v0.5 — Publisher trust (ships with `ark ext add`)

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-13.1 | Install-time publisher trust prompt: `ark ext add <source>` shows publisher info (derived from source — github:user, url host, etc.) and prompts "Trust this publisher? [y/n]". Accept stored in `${XDG_CONFIG_HOME}/ark/extension-trust.kdl` keyed by publisher. VSCode 1.97 analog. Combined with T-13.4 cap-disclosure for a single prompt. | scene, cli | R10 | T-12.9 | S |
| T-13.2 | `--accept-all` flag on `ark ext add` for CI non-interactive path. Logs warning into audit file `${XDG_DATA_HOME}/ark/extension-audit.log`. | scene, cli | R10 | T-13.1 | S |

**⬆ Milestone v0.4 (declared caps) ships after T-13.3–T-13.6. Milestone v0.5 (package mgr + trust) ships after T-11.*, T-12.9–T-12.11, T-13.1–T-13.2.**

### Tier 14 — Migration + shipped content

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-14.1 | Legacy-layout auto-wrap. **Detection rules:** (a) file has `scene "…" { }` → use directly; (b) file has top-level `layout { }` and no `scene` → auto-wrap as `scene "default" { layout { … } }` + emit debug log; (c) file has neither → `error[scene/empty-or-unknown]` with wrap-suggestion help; (d) file has both `scene` AND top-level `layout` → `error[scene/ambiguous-file-shape]`. | scene | R15 | T-1.1 | S |
| T-14.2 | Port remaining shipped layouts: `crates/mux/zellij/layouts/{classic,focused,log,review,triple-column}.kdl` → `crates/mux/zellij/scenes/<same>.kdl` as minimal scene wrappers (builder handled by T-8.5). Preserve exact runtime behavior. Keep old files for N versions per cavekit-layouts R5. | scene, mux-zellij | R15 | T-14.1, T-8.5 | M |
| T-14.3 | Hook-config shim: existing TOML `[[hooks]]` config (cavekit-config R4) compiles to a synthetic scene fragment with equivalent `on` blocks (mapping per T-5.7). Appended to user scene at compile. Existing hooks keep firing without migration. | scene, config | R15 | T-5.7 | M |
| T-14.4 | Update `cavekit-overview.md` domain index: add scene + extension entries, mark as APPROVED once R1–R15 all implemented. **Also add docs note:** ark ships its own zellij (cavekit-distribution concern); user's `~/.config/zellij/config.kdl` still honored — scene keybinds merge additively on top per B2 semantics; `ARK_USE_SYSTEM_ZELLIJ=1` env overrides for dev. | scene, docs | R15 | T-14.2 | S |
| T-14.5 | Update `cavekit-layouts.md`: cross-reference cavekit-scene.md as the superset; note that pure-layout usage is preserved and valid (T-14.1 auto-wrap). | scene, docs | R15 | T-14.1 | S |

### Tier 15 — v1.0 API freeze

| Task | Title | Cavekit | Requirement | blockedBy | Effort |
|------|-------|---------|-------------|-----------|--------|
| T-15.1 | Document the frozen `ark.core.*` intent surface in `context/refs/intent-api-v1.md`: full schema for each of the 13 ops, version compatibility contract, deprecation policy. | scene, docs | n/a (Tier 4) | T-14.* | M |
| T-15.2 | Document the wasm metadata v1 format: field-by-field spec, custom section name, size limits, KDL 2.0 reference, example emission via proc-macro. | scene, plugins, docs | n/a (Tier 4) | T-15.1 | S |
| T-15.3 | `ark scene check --v1-strict` flag: enforces v1.0 contract. Used as CI gate for shipped scenes. | scene, cli | R13 | T-15.1 | S |

## Tier ordering

```
T-0.1 → T-0.2, T-0.3, T-0.4
            ↓
        T-1.1 → T-1.2 → T-1.3 → T-1.4
                    ↓
              T-2.1 → T-2.2, T-2.3
                    ↓        ↓
              T-2.4 → T-2.5 → T-2.6
                            ↓
                        T-3.1 → T-3.2 → T-3.3 (needs T-5.1, T-6.2)
                                    ↓
                              T-3.4 → T-3.5
                                          ↓
                                      T-4.1 → T-4.2 → T-4.3, T-4.4, T-4.5
                                                          ↓
                                                    T-5.1 → T-5.2 → T-5.3 → T-5.4, T-5.5, T-5.6, T-5.7
                                                                              ↓
                                                                        T-6.1 → T-6.2, T-6.3, T-6.4
                                                                                  ↓
                                                                            T-6.5 → T-6.6 → T-6.7
                                                                                        ↓
                                                                                  T-7.1 → T-7.2 → T-7.3, T-7.4, T-7.5, T-7.6
                                                                                                          ↓
                                                                                              T-8.1 → T-8.2 → T-8.3, T-8.4, T-8.5, T-8.6
```

v0.1 complete at T-8.*. v0.2 starts T-9. **v0.3 starts with Tier 9.5 (extension protocol) + Tier ACP (ACP client) + Tier 10 (extensions) — these three tiers land together and share the v0.3 milestone.** v0.4 = T-13.3–T-13.6 (declared capabilities). v0.5 = T-11.* (hot reload) + T-12.9–T-12.11 (package mgr) + T-13.1–T-13.2 (publisher trust). v1.0 = T-15.* (API freeze).

Parallelism inside a tier: tasks at same depth without direct arrows can run concurrently. Tier 6 has three parallel forks (T-6.2, T-6.3, T-6.4) after T-6.1 lands.

## Out of scope

- **Rhai/Lua scripting for `exec` replacement** — v2+.
- **Cryptographic signing of extensions** — v0.6+; trust file is install-time accept only.
- **Scene registry / search index / discovery UI** — v1.1+.
- **Chord sequences (vim-style `<leader>ff`)** — v2; single-chord only v1.
- **Multi-version same-ext loading** — v2; single version per name per scene.
- **User-defined CEL functions** — v2; stdlib only v1.
- **Scene-level `layout extends` with merging** — v0.3+; v1 uses full-layout replacement per extends.
- **Ark intents as ACP tools** — scene ops not exposed to the agent as callable tools in v1. Ark drives agent, not vice versa.
- **Agent-as-ark-controller** — agent affects ark only through ACP verbs, cannot `emit` or dispatch intents.
- **Runtime extension enable/disable without scene edit** — v2; edit scene + reload is the path.

## Verification

After v0.1 (T-0..T-8):
1. `cargo build --workspace` green.
2. `cargo test -p ark_scene` green.
3. `cargo test -p ark-cli scene_` green (e2e T-8.3, T-8.4).
4. Manual: `ark spawn --scene ~/.config/ark/scenes/default.kdl --orchestrator claude-code -- claude` spawns with scene-driven reactions + keybinds active.
5. `ark scene check ~/.config/ark/scenes/default.kdl` exits 0.
6. Shipped `crates/mux/zellij/layouts/builder.kdl` still produces identical session (T-14.1 auto-wrap).

After v0.3 (T-9..T-10):
7. `ark ext add path:./my-test-runner` installs; `ark ext list` shows it; user scene with `use "my-test-runner"` compiles + spawns with test-runner plugin mounted.
8. Transitive: `my-test-runner` ext uses `file-watcher` ext; both installed; user `use "my-test-runner"` pulls both; `ark scene graph` shows tree.

After v0.5 (T-11..T-14):
9. `ark ext add github:user/ark-diff-watcher` installs, caps prompt shown + stored.
10. Edit scene file; file-watcher fires `reload_scene`; session updates in < 500ms with new reactions/keybinds live.
11. Existing hook-TOML config still functional via T-14.3 shim.
12. `ark scene dry-run --event 'PhaseTransition{to=review}'` prints matching ops.
13. `ark scene graph` + `ark scene explain` produce attribution for every runtime behavior.

## Open decisions before starting

See **Open questions** at top. All five should have user answers before T-6.x begins (ark-bus mount model), T-8.1 (scene file location), T-0.3 (UserEvent variant).
