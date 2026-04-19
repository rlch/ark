---
created: "2026-04-15"
last_edited: "2026-04-16"
---

# Spec: Scene — Reactive KDL Configuration + Extension System

## Scope

Ark's extensibility has **three distinct layers**, each with its own audience and lifecycle:

| Layer | What | Audience | Lifecycle |
|---|---|---|---|
| **Scene** (this spec) | User-facing KDL config file declaring layout + reactions + keybinds + plugin lifecycle + extension composition | Scene author (end user) | Parsed at `ark spawn`; produces runtime registries |
| **Extension protocol** | JSON-RPC 2.0 contract between ark core and running extensions. Bidirectional, NDJSON over stdio for subprocess extensions; in-process trait calls for compiled-in; wasm component calls for wasm-component extensions | Extension author | Session-long; runtime |
| **ACP** (Agent Client Protocol, [external open standard](https://agentclientprotocol.com)) | JSON-RPC 2.0 contract between ark and coding-agent processes (Claude Code, Codex, Gemini CLI, aider via adapter) | Coding-agent author | Per-agent-session; runtime |

The scene is the one artifact a user writes by hand. The extension protocol is ark's internal plugin API. ACP is an external standard ark adopts to interoperate with every ACP-compliant coding agent.

The **scene** is a **preprocessed superset** of zellij's layout KDL — the `layout { }` block stays zellij-parseable; everything else (`on`, `keybind`, `plugin`, `use`, `extends`, `include`) is ark-native and compiles into four runtime artifacts: (1) a rendered zellij layout KDL for `--layout`, (2) an event-bus subscriber registry, (3) a plugin lifecycle manifest, (4) an ACP engine-launch registry.

**Extensions** are ark-level plugin bundles: a binary (wasm, subprocess binary, or compiled-in Rust) plus an optional sidecar `scene.kdl` fragment and a declared capability set. Activated via `use "name"` in a scene. Extensions talk to ark core over the **extension protocol**; they are distinct from ACP agents.

**Engines** are ACP-speaking coding-agent processes. Ark bundles an ACP client; engines are invoked via `engine { name "claude"; command "claude"; args "--acp" }` (or via an extension that advertises `capabilities { speaks "acp" }` and supplies launch args). Ark does NOT design its own agent protocol — the industry converged on ACP; ark conforms.

One scene file = one composed configuration for one `ark spawn`.

## Requirements

### R1: Scene file grammar
**Description:** A scene is a KDL 2.0 document with a single top-level `scene "<name>" { … }` node. Body admits a fixed set of child nodes per the scope table.
**Acceptance Criteria:**
- [ ] Single top-level `scene` node. Multiple = parse error.
- [ ] Scene-root body admits: `extends`, `include`, `use`, `layout`, `plugin`, `on`, `keybind`, `engine`, `clear-reactions`, `clear-keybind`, `disable-plugin`. Unknown node = parse error with "did you mean …?" suggestion (facet's built-in typo suggester). The `engine { name "…" command "…" args "…" env { … } }` block is a direct ACP launch spec (R17); only one per scene; `engine` and `use "engine-*"` in the same scene = compile error per R17 mutual-exclusion rule.
- [ ] Node ordering is semantically irrelevant for `extends`, `include`, `use`, `layout`, `plugin`, `engine`, `clear-*`, `disable-plugin` (compiler iterates by kind). EXCEPTION: `on` blocks and `keybind` blocks execute in textual order within a scene file (R11 merge rules). `ark scene fmt` MUST preserve the relative order of `on`/`keybind` nodes within a file; reordering any other kind is safe.
- [ ] Parser uses `facet-kdl` derive macros (`#[derive(Facet)]`) with span info preserved for every node via facet SHAPE. `kdl` 6.5.0 kept for formatter round-trip in `ark scene fmt`.
- [ ] Parse errors surface via `miette::Diagnostic` with file name, line/col, caret, help text. facet + facet-kdl integrate miette natively.
**Dependencies:** cavekit-mux-zellij R5

### R2: Scope rules
**Description:** Every construct has a well-defined set of legal parent nodes. The compiler rejects misplaced nodes with a location-specific diagnostic.
**Acceptance Criteria:**
- [ ] `on`, `keybind`, `plugin`, `use`, `extends`, `include` legal only at scene root.
- [ ] `tab`, `pane`, `floating-panes` legal only inside `layout { }` (or nested inside another `pane`/`tab` per zellij rules).
- [ ] `when=` attribute legal on `tab` and `pane` nodes only.
- [ ] `source`, `mount`, `summon`, `dismiss`, `on`, `subscribes`, `config` legal only inside `plugin { }`.
- [ ] `if=` attribute legal on `on { }` nodes only.
- [ ] `intent=` attribute legal on `keybind` (shorthand form) only.
- [ ] Scope violation produces `error[scene/misplaced-node]` with parent-node context.

### R3: Layout compilation
**Description:** The `layout { }` block is compiled to a zellij-compatible KDL file at spawn time. Conditional branches are resolved against initial agent state; ark-only attributes are stripped.
**Acceptance Criteria:**
- [ ] `when="<CEL>"` evaluated against initial state; true → branch retained, false → branch pruned before emission.
- [ ] `tab`/`pane` attributes ark does not own (name, command, args, split_direction, size, focus, cwd, stacked, …) pass through unchanged.
- [ ] Template strings (`"{{ agent_cmd }}"`) rendered with compile-time minijinja context (cwd, agent_cmd, agent_args, id, name) per existing `layout_template.rs` surface.
- [ ] Rendered output written to `${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl`, extension `.kdl` enforced (zellij #4994).
- [ ] Rendered output passes `kdl::KdlDocument::parse` without error before handoff to zellij.
- [ ] Dynamic `when=` branches (those that may change post-spawn) register a subscriber that `override-layout`s when the expression's truth value flips.

### R4: Reactions (`on` blocks)
**Description:** A reaction is an event selector + optional CEL predicate + ordered op list. At runtime, matching events invoke all subscribed reactions in load order; each reaction's ops run sequentially with fail-fast semantics.
**Acceptance Criteria:**
- [ ] Selector syntax: `"<EventKind>"`, `"<EventKind> field=\"value\""` (sugar), or `"UserEvent:<namespaced-name>"`. `*` matches all kinds.
- [ ] Multiple `on` blocks with overlapping selectors each run; no silent dedup.
- [ ] `if="<CEL expression>"` evaluated per fire; false → skip reaction entirely.
- [ ] Op failure logs `error[scene/op-failed]` with reaction source span + op kind + error; remaining ops in that reaction skipped; event loop continues.
- [ ] `emit` op fires a new event synchronously; cascade depth bounded at 4 (default); exceeding = error log + drop. Configurable per-scene via top-level attribute: `scene "<name>" max-cascade-depth=<N> { … }` (facet struct field with default 4).
- [ ] Cycle detection: `emit A → on A emits B → on B emits A` caught at compile time when statically detectable; runtime check as fallback.
- [ ] **`UserEvent.source` attribution convention** (canonical values, to keep `ark scene explain` consistent across origins):
    - `"core"` — emitted by ark-core (supervisor, scene runtime, ACP client, file watcher).
    - `"ext:<name>"` — emitted by an ark-native extension with that manifest name.
    - `"plugin:<name>"` — emitted by a zellij wasm plugin (direct `event/emit` via ark-bus).
    - `"hook:<name>"` — emitted by a legacy TOML `[[hooks]]` entry (compat path per R15 / T-14.3).
    - `"scene"` — emitted by a scene reaction's `emit` op (source file tracked separately via reaction origin).
  - `source` is NEVER empty. Compiler rejects `emit` ops that can't be statically assigned one.

### R5: Keybinds
**Description:** `keybind` declarations compile into a `keybinds { }` block injected into the rendered layout KDL. Each bind dispatches a named intent via the `ark-bus` plugin, letting user config overrides (per B2 zellij merge semantics) layer additively.
**Acceptance Criteria:**
- [ ] Two forms: shorthand `keybind "Alt p" intent="picker.show"` and block `keybind "Alt p" { <op+> }`.
- [ ] Key string validated against zellij's key chord lexer at compile time.
- [ ] Block body uses same op grammar as `on` reactions.
- [ ] Compiled to zellij KDL: `bind "Alt p" { MessagePlugin "ark-bus" { name "ark-intent"; payload "<JSON>"; } }` where JSON encodes the intent or op list.
- [ ] Keybinds block is added to rendered layout WITHOUT `clear-defaults=true` so user `~/.config/zellij/config.kdl` binds survive (per B2 zellij merge confirmation).
- [ ] Duplicate chord across scene + transitive extensions: last-wins, with `ark scene graph` showing resolution order.
- [ ] Hot-reload keybind diff via `rebind_keys` (R14) MUST preserve user's `~/.config/zellij/config.kdl` bindings identically to the initial layout-merge path. Conformance test: load user-config with custom binding; spawn scene; reload scene with keybind delta; verify user-config binding still fires.

### R6: Plugin lifecycle
**Description:** The `plugin "<name>" { … }` block declares a **zellij wasm plugin** (not an ark-native extension — see R10). A plugin has a lifecycle mode (always / summon / on-event) and a mount target. The keywords `plugin` and `use` are NOT interchangeable: `plugin` refers only to zellij wasm cartridges; `use` refers only to ark-native extension bundles (which may themselves contribute plugin declarations via sidecar fragments).
**Acceptance Criteria:**
- [ ] `source` (required, exactly 1): `shipped:<name>` (built into ark binary), `ext:<name>` (wasm cartridge contributed by a `use`d extension's sidecar), `file:<path>`, `url:<https://…>`.
- [ ] Compiler enforces: `plugin "<name>" { source "ext:X" }` is rejected with `error[scene/ext-not-used]` unless scene also contains `use "X"` OR a transitive `use` pulls X in. Prevents referring to extension-bundled wasm without activating the extension.
- [ ] `mount` (required): `status-bar` | `floating` | `pane` | `hidden` + positional attrs (`into`, `split`, `size`, `x`, `y`, `width`, `height`). `hidden` uses zellij's first-class suppressed-pane API (not a layout hack) — idiomatic per zellij-autolock.
- [ ] **Mount mechanism for `always` + `hidden`:** plugin declaration is baked into the rendered zellij layout KDL as a suppressed-pane entry; zellij loads it during `--layout` startup, before ark issues any runtime `launch-or-focus-plugin`. For `always` + non-hidden mounts, same applies — layout-injected. Post-spawn `launch-or-focus-plugin` is used only for `summon` / `event-mount` transitions.
- [ ] Lifecycle inferred from body:
  - neither `summon` nor `on` → **always** (layout-injected at session spawn).
  - `summon "<selector>"` present → **summon** (dormant; `launch-or-focus-plugin` on selector match).
  - `on "<event-selector>"` present → **event-mount** (dormant; `launch-or-focus-plugin` when event fires).
  - `summon` + `on` both present = `error[scene/plugin-ambiguous-lifecycle]` with carets on both attrs.
- [ ] `dismiss "<selector>"` mirror for summon/event-mount; closes the plugin pane/floating.
- [ ] `subscribes "<selector>"+` forwards matching events via `zellij pipe --plugin <url>`; unrelated to mounting.
- [ ] `config { }` block validated against the plugin's declared config schema. Schema source: for `shipped:*` plugins, declared via `register_extension!` macro (same path as wasm custom section, compiled-in variant); for `ext:*` plugins, declared via the extension's sidecar; for `file:*` / `url:*`, declared via the wasm `ark.metadata` custom section. Schema-less plugins pass config through as untyped JSON map. Passed to plugin at `launch-or-focus-plugin` call.
- [ ] `override=true` attribute on `plugin` allows user scene to replace an extension's default plugin block without an error.

### R7: Op vocabulary
**Description:** Canonical v1 op set (= intent surface). Each op maps to a single intent registered in ark core; plugins register additional namespaced intents (`<ext>.<name>`).
**Acceptance Criteria:**
- [ ] Core ops (all in namespace `ark.core.*`). Grouped by subject:

  **Tabs + panes (mux):**
    1. `open_tab name=<str> [layout=<str>] [focus=<bool>]`
    2. `close_tab (name=<str>|index=<int>)`
    3. `rename_tab (name=<str>|index=<int>) to=<str>`
    4. `focus_tab (name=<str>|index=<int>)`
    5. `split_pane into=<str> side=<"left"|"right"|"up"|"down"> [size=<percent|int>] { command <str>; args <str>*; cwd <str>? }`
    6. `close_pane (id=<str>|selector=<str>)`

  **Plugins:**
    7. `mount_plugin name=<str> [at=<str>] [into=<str>]`
    8. `unmount_plugin name=<str>`

  **Messaging:**
    9. `pipe plugin=<str> [severity=<str>] [name=<str>] { text <str> OR json <str> }`
    10. `emit <user-event-name=<str>> { <kv payload>* }`
    11. `set_status text=<str> [severity=<str>] [ttl_ms=<int>]` (sugar over pipe to ark-status)

  **Control:**
    12. `exec script=<str> [shell=<str>] [timeout_ms=<int>] [cwd=<str>] [env { <kv>* }]`
    13. `reload_scene` (re-parse scene file, apply deltas; respects turn-inflight guard per R17)

  **ACP-interaction (added per R17 ACP integration):**
    14. `prompt [session=<str>] { text <str> OR parts { <MessagePart>* } }` — send a user message into the ACP agent, drive a turn. Maps to ACP `session/prompt`. Default session = current engine session.
    15. `acp/cancel [session=<str>]` — maps to ACP `session/cancel` notification. Agent MUST respond to the in-flight `session/prompt` with `stopReason: cancelled` per ACP spec; op does NOT return until that response arrives (blocks up to 5s, then logs error).
    16. `acp/permit request_id=<str> outcome=<"allow"|"reject_once"|"reject_always"> [option_id=<str>]` — respond to an outstanding ACP `session/request_permission`. Scene reactions on `UserEvent:ark.acp.permission_requested` use this to auto-approve/deny. See R17 permission-dispatch acceptance criteria.
    17. `set_mode mode=<str> [session=<str>]` — maps to ACP `session/set_mode` (plan / edit / etc.). Mode values agent-dependent; scene compiler does not validate.

- [ ] ACP-interaction ops (14–17) no-op with warning if no ACP session is active (e.g., scene `engine { }` unset OR engine has not yet completed `initialize`). Reactions MUST be tolerant.
- [ ] Unstable-ACP ops (from `meta.unstable.json`: `session/fork`, `session/resume`, `elicitation/*`, `nes/*`, `document/did*`, etc.) are NOT included in v1. Gate behind capability flag negotiated at ACP `initialize`; surface as namespaced `ark.acp.unstable.*` ops only when engine advertises support.
- [ ] Each op has a KDL schema (used for `ark scene check` validation) and a Rust impl implementing the `Intent` trait.
- [ ] Unknown op at scene root = `error[scene/unknown-op]` with "did you mean …?" suggestions.
- [ ] All op attrs and body strings support runtime templating (see R9).

### R8: Expression language (CEL)
**Description:** `if=` predicates and `when=` attributes use Common Expression Language (CEL) evaluated via `cel-interpreter` crate. Deterministic, bounded, no I/O.
**Acceptance Criteria:**
- [ ] CEL expressions parsed once at scene-compile time, stored as AST; evaluated per-fire with injected context.
- [ ] Context bindings provided to every CEL eval:
  - `event` — map of firing `AgentEvent`'s fields (flat-mapped; tag + fields).
  - `payload` — populated for `UserEvent`; arbitrary JSON as CEL map.
  - `agent` — `{id, name, phase, state, orchestrator, engine}`.
  - `session` — `{name, created_at, attached}`.
- [ ] CEL canonical `matches(str, regex)` via CEL stdlib (RE2-backed; linear-time, no ReDoS risk). Custom functions added v1: `glob(str, pattern)` (globset), `starts_with(str, prefix)`, `ends_with(str, suffix)`, `contains(str, substr)`, `size(list|map)`.
- [ ] No user-defined functions v1; no loops; no side effects. CEL's design guarantees termination.
- [ ] Expression compile errors surface at `ark scene check` with span + CEL error text.
- [ ] Runtime eval errors (e.g., field access on absent payload) logged + reaction skipped; supervisor does not panic.

### R9: Templating (two scopes)
**Description:** String values in scene files support minijinja templating with one of two contexts: compile-time (for `layout { }` subtree) or runtime (for op strings in reactions/keybinds).
**Acceptance Criteria:**
- [ ] Compile-time context (existing): `cwd`, `agent_cmd`, `agent_args`, `id`, `name`. Rendered at scene compile; undefined = hard error.
- [ ] Runtime context (new): `event`, `payload`, `agent`, `session` (same shape as CEL bindings). Rendered at op dispatch; undefined = warn + substitute empty.
- [ ] Same minijinja engine; different `Value` injection per scope.
- [ ] Template engine `strict_undefined` for compile-time; `lenient_undefined` for runtime (mismatched fields in a reaction should not crash the session).
- [ ] Templates in `layout { }` use compile-time context ONLY; templates in `on`/`keybind` op args use runtime context ONLY. Compiler enforces.

### R10: Extensions
**Description:** An extension is a directory under the ark ext search path. A single extension manifest declares one or more capabilities; extension code implements the extension protocol (R16) and runs in one of three delivery modes. Activated via `use "<name>"` in a scene.
**Acceptance Criteria:**
- [ ] Search path (precedence order):
    1. `./.ark/extensions/<name>/` (project-local, vendored)
    2. `${XDG_DATA_HOME}/ark/extensions/<name>/` (user-installed)
    3. `/usr/share/ark/extensions/<name>/` (system-installed)
    4. Built-in extensions compiled into the `ark` binary
- [ ] **Manifest + code layout:**
    - `extension.kdl` — KDL-encoded `ExtensionMetadata` declaration (name, version, ark-protocol range, delivery, capabilities, requires, config schema).
    - Code artifact per delivery mode: `ext.wasm` (wasm-component), `bin/<exec>` (subprocess), built-in registration (compiled-in).
    - Optional **sidecar scene fragment** — a `scene.kdl` file at the extension root containing declarative contributions (plugin blocks, reactions, keybinds). Throughout the docs this is called a "sidecar scene fragment" — `scene fragment` and `scene sidecar` are deprecated synonyms.
- [ ] **Three delivery modes share one extension protocol:**
    - `compiled-in` — Rust crate in workspace; registered via `register_extension!` macro; in-process trait-object dispatch.
    - `subprocess` — any-language binary; ark spawns + pipes stdio; extension protocol as NDJSON JSON-RPC 2.0.
    - `wasm-component` — WASI Preview 2 component; wasmtime host; extension protocol as wit-bindgen generated calls.
- [ ] **`ExtensionMetadata` wire format:** for wasm-component + subprocess extensions, the manifest is a `extension.kdl` file at the extension root (human-readable KDL serialized from the `ExtensionMetadata` struct via facet-kdl). For compiled-in, the same struct is emitted via the `register_extension!` macro. Identical data, identical type. Struct fields are `#[derive(Facet)]` with Rust doc-comments that surface as LSP hover text.
- [ ] **Capabilities are object-of-objects** (no `kind` enum): `{ ui: {keybind, pane, status}?, intents: {provide, dispatch}?, events: {subscribe, emit}?, agent: {engine: {speaks: "acp"}}?, orchestrator: {...}? }`. New capabilities added without breaking existing extensions (MCP pattern).
- [ ] `ark-ext-metadata` helper crate: shared `ExtensionMetadata` type (both extension authors and ark core import it); proc-macro `register_extension!` emits compiled-in registration OR wasm custom section OR subprocess manifest depending on build target.
- [ ] `ark ext inspect <path>` dumps metadata without executing the extension.
- [ ] `use "<name>"` resolution:
    1. Walk search path; first match wins.
    2. Parse `extension.kdl`; validate version ranges against host; register namespaced intents + events into scene compiler symbol table.
    3. If sidecar scene fragment exists, parse as scene fragment (same grammar, no `scene { }` wrapper); contributions added to merge pool.
    4. If user `use` block has `config { }`, validate against extension-declared schema; store for extension-startup handoff.
- [ ] Sidecar-contributed plugin blocks **auto-mount** on `use` unless the user writes `disable-plugin "<name>"` at scene root. User can override a specific attribute (mount target, config) by declaring their own `plugin "<name>" { override=true; … }` block — last-wins per R11 merge rules.
- [ ] Sidecar-contributed reactions and keybinds are always merged into the scene per R11. User can silence them via `clear-reactions selector="<sel>"` or `clear-keybind "<chord>"`.
- [ ] Sidecar is optional: extension with only code + manifest is valid; `use` brings its API (intents, events) into scope but mounts/reacts nothing — user wires via their own scene.
- [ ] Missing extension = `error[ext/missing]` with Levenshtein-suggested alternatives from the known set.
- [ ] Version mismatch (manifest declares `ark ">=1.0"` but host is `0.9`) = `error[ext/version]` with install hint.

### R11: Composition and merge
**Description:** Scenes compose via `extends`, `include`, `use`. The compiler resolves a dependency DAG, topologically sorts, applies contributions with documented merge rules.
**Acceptance Criteria:**
- [ ] `extends "<scene-name>"` — inherits a base scene. Child overrides parent per merge rules. One `extends` per scene.
- [ ] `include "<path>"` — splices another KDL fragment into the current scene at this point in load order. Multiple includes allowed.
- [ ] `use "<ext>"` — transitive; extension's `use` references resolve recursively. Topological sort. Cycle = `error[ext/cycle]` with trace.
- [ ] Namespacing (MANDATORY): every intent, every event, every user-event carries a namespace `<owner>.<name>`. Owners: `ark.core.*` (core, reserved; extension declarations here = `error[ext/reserved-namespace]`), `<ext-name>.*` (extension), `user.*` (scene author).
- [ ] **Context-sensitive unprefixed-name rewrite** (not a blanket `user.*` rewrite):
    - In a **user scene file** (top-level author-written scene or `extends`-rooted user base): unprefixed `intent="foo"` / event refs / `emit "foo"` → `user.foo`.
    - In an **extension sidecar scene fragment**: unprefixed `intent="foo"` → `<ext-name>.foo`. Extensions referring to core ops MUST write the fully-qualified form `ark.core.open_tab` etc. — no auto-rewrite to core. Prevents import-shadow footguns.
    - Fully-qualified names (contain a `.`) are never rewritten.
- [ ] Load order of contributions (determines reaction execution order and last-wins resolution):
    1. Extensions (in topological order of `use` dependency DAG).
    2. Includes (in source position within the current scene file).
    3. Parent scenes (if `extends`) — parent contributions before child.
    4. User scene's own contributions — applied last.
- [ ] Merge rules:
    - **reactions** (`on` blocks): all retained, executed in load order (parents before children, extensions before user). Within a single file, textual order preserved. `clear-reactions selector="<sel>"` drops prior matches.
    - **keybinds**: last-wins per chord. `clear-keybind "<key>"` drops prior. User scene loaded last → user wins by default.
    - **plugin blocks**: duplicate `plugin "<name>"` = error unless later block has `override=true`. `disable-plugin "<name>"` drops prior.
    - **layout**: duplicate `tab` / `pane` by name = error; mergable via explicit `merge` attribute (deferred to v0.3+).
- [ ] `ark scene graph` shows origin of every reaction/keybind/plugin by file:line, ordered by load position.

### R12: Diagnostics
**Description:** Every compile-time and runtime error surfaces via `miette` with file:line:col, source span, caret, help text, and optional "did you mean …?" suggestion.
**Acceptance Criteria:**
- [ ] Error codes namespaced and enumerated at their introduction site (no generic stand-in codes):
    - `scene/*` family: `scene/parse`, `scene/grammar`, `scene/misplaced-node`, `scene/unknown-node`, `scene/duplicate-node`, `scene/plugin-ambiguous-lifecycle`, `scene/ext-not-used`, `scene/ambiguous-file-shape`, `scene/empty-or-unknown`, `scene/include-cycle`.
    - `ext/*` family: `ext/missing`, `ext/version`, `ext/cycle`, `ext/reserved-namespace`, `ext/bad-config`, `ext/crashed`.
    - `ext-proto/*` family (runtime protocol): `ext-proto/unsupported-version`, `ext-proto/capability-denied`.
    - `op/*` family: `op/unresolved-ref`, `op/unknown`, `op/failed`.
    - `plugin/*` family: `plugin/mount-failed`, `plugin/bad-config`, `plugin/unknown-config-key`.
    - `cel/*` family: `cel/parse`, `cel/eval`, `cel/unknown-function`.
    - `acp/*` family (R17): `acp/initialize-failed`, `acp/permission-timeout`, `acp/unstable-not-supported`.
- [ ] All errors implement `miette::Diagnostic` with `code()`, `severity()`, `help()`, `labels()`.
- [ ] Every AST node retains origin span; merged contributions track source extension + line for attribution.
- [ ] `ark scene check` exits non-zero on any error, prints every diagnostic (not just first).
- [ ] Runtime reaction errors (op failure, CEL eval error) logged at `warn` with full span chain; do not crash supervisor.
- [ ] Test suite includes at least one unit test per error code verifying the diagnostic output matches a snapshot.

### R13: CLI surface
**Description:** Scene-related commands under `ark scene` and `ark ext` subcommands.
**Acceptance Criteria:**
- [ ] `ark scene check [path]` — parse + schema + symbol-table + cross-ref validation. Exit 0 on green. Drop-in for editor save hook.
- [ ] `ark scene fmt [path]` — canonical format. Idempotent.
- [ ] `ark scene dry-run --event '<selector>' [--payload '<json>'] [--agent-json '<json>'] [--session-json '<json>']` — print op list that would fire. No side effects. CEL evaluation uses provided `--agent-json` / `--session-json` context overrides; absent context falls back to a stub snapshot with safe defaults (`agent.phase = "started"`, `session.name = "<dry-run>"`). Emits a warning naming every CEL expression that read from the stub, so users know which predicates their fixture didn't cover.
- [ ] `ark scene graph [path]` — text tree: extensions loaded, plugins mounted, reactions registered (with origin file:line), keybinds resolved, intents/events in scope.
- [ ] `ark scene explain <ref>` — where does this come from? Refs: `intent:<name>`, `keybind:<chord>`, `plugin:<name>`, `reaction:<event>`.
- [ ] `ark ext add <source>` — install from `github:user/repo[@tag]`, `path:<dir>`, `url:<tarball>`. Shows requested capabilities, prompts accept.
- [ ] `ark ext list` — list installed extensions with versions. `ark ext info <name>` — dump a single installed extension's metadata (name, version, ark-protocol range, capabilities, declared intents/events, installed-from source). `ark ext inspect <path>` — same metadata dump but from a path without installation. `ark ext remove <name>` — removes from `${XDG_DATA_HOME}/ark/extensions/`. `ark ext update [name]` — re-fetch from recorded install source, re-prompt for new caps if version-bumped.
- [ ] `ark ext` commands only modify `${XDG_DATA_HOME}/ark/extensions/`; never touch project or system paths.

### R14: Hot reload
**Description:** `ark scene reload` (also auto-fired on file change when enabled) re-parses the scene and applies deltas without restarting the session. Gated on ACP turn state (R17) to avoid tearing down dispatch tables mid-stream.
**Acceptance Criteria:**
- [ ] `reload_scene` op + `ark scene reload --session <name>` CLI both enter the same reload path.
- [ ] **Turn-inflight gate** (per R17): if any ACP session has a `session/prompt` awaiting response, queue the reload, emit `UserEvent:ark.scene.reload_pending { reason: "turn-inflight", pending_sessions: […] }`, and DO NOT apply. Apply when every tracked session has received a `stopReason` response. User can force by issuing `acp/cancel` (which triggers `stopReason: cancelled`) then the queued reload fires.
- [ ] Reload algorithm (runs ONLY when no turn is in flight):
    1. Re-parse + validate. On failure: keep old config running, surface error via `set_status severity="error"`. Do NOT tear down.
    2. Diff subscription set. Add new `on` blocks, drop removed. **In-flight drain:** reactions already dispatching ops at the moment of diff complete their op list (fail-fast as per R4) against the OLD registry; the atomic registry swap happens after in-flight reactions drain. Reactions mid-swap do NOT straddle old + new.
    3. Diff keybinds. Issue `rebind_keys` via ark-bus for deltas.
    4. Diff plugin lifecycles. Summon/dismiss deltas via `launch-or-focus-plugin` / close.
    5. Diff always-on plugin `source` values. Changed = `start-or-reload-plugin`.
    6. Layout structural diff: ANY structural change (tab add/remove, pane reorg, `when=` truth flip) = skip layout update, emit `UserEvent:ark.scene.reload_partial { reason: "layout-change", details }`; reactions/keybinds/plugins still apply. User sees "session restart required for layout changes" via status. Fine-grained pane diff + dynamic `when=` `override-layout` deferred to v0.2+.
- [ ] Single-slot re-entry guard: concurrent `reload_scene` calls while a reload is active are dropped with a debug log. Prevents cascade-induced infinite reload.
- [ ] Reload telemetry event: on completion (success OR partial OR failed), emit `UserEvent:ark.scene.reloaded { duration_ms, reactions_added, reactions_removed, keybinds_changed, plugins_changed, status: "ok"|"partial"|"failed" }`. Scene authors can react to this event.
- [ ] File-watcher (optional, `[scene] watch = true` in `config.toml`) uses `notify` crate; watches resolved scene path (not parent dir); debounced 200ms; ignores editor temp files by suffix pattern (`.swp`, `.tmp`, trailing `~`, leading `.#`, `.bak`); auto-fires `reload_scene` on accepted change.
- [ ] Reload completes in < 500ms for a typical scene (≤ 20 reactions, 5 plugins), measured from "no turn in flight" signal to registry swap.

### R16: Extension protocol (runtime RPC)
**Description:** JSON-RPC 2.0 contract between ark core and running extensions. Bidirectional. NDJSON over stdio for subprocess extensions; in-process trait-object calls for compiled-in; wit-bindgen generated calls for wasm-component. Same message contracts across all three delivery modes — only transport differs.
**Acceptance Criteria:**
- [ ] **Method surface v1**, namespaced:
    - Lifecycle: `initialize`, `initialized` (notif), `shutdown`, `ping`
    - Async + cancel: `$/cancel` (notif), `$/progress` (notif), `task/create`, `task/get`, `task/cancel`
    - Event bus: `event/subscribe`, `event/unsubscribe`, `event/emit`, `event/notify` (notif, host→ext)
    - Intents: `intent/register`, `intent/unregister`, `intent/dispatch`
    - UI intent channels (all delivery modes): `ui/keybind/register`, `ui/keybind/unregister`, `ui/status/push` (notif)
    - UI surface (narrow, all delivery modes): `ui/pane/request`, `ui/pane/close` — see narrowing rules below
    - Workspace intent (all delivery modes; LSP-style reverse-request): `workspace/applyEdit`, `workspace/configuration`, `workspace/showDocument`, `workspace/showMessage` (notif), `workspace/showMessageRequest`
    - Scene intent (all delivery modes): `scene/getRoot` (returns current scene path + CWD)
    - Host syscall-proxies (WASM-COMPONENT ONLY, capability-gated — subprocess extensions MUST use OS directly): `host/fs/read`, `host/fs/write`, `host/proc/spawn`, `host/net/fetch`
    - Logging: `log/write` (notif), `log/setLevel`

- [ ] **Principle for `host/*` vs `workspace/*`:** `host/*` are sandbox-escape proxies (wasm can't do syscalls; host does them on its behalf). `workspace/*` are intent channels (ask the host to do a user-facing thing — edit a file, show a message, resolve config). Subprocess extensions have direct OS access and SHOULD NOT call `host/*`; conformance suite (R16 test task) rejects subprocess calls to `host/*` with `ext-proto/capability-denied`. `workspace/*` is legitimate for all delivery modes, same as LSP.

- [ ] **`ui/keybind/register` does NOT register raw keys.** It registers a **command ID + metadata** (title, when-clause CEL predicate, suggested default chord). The user's scene file binds actual keys to the command ID via `keybind "Alt p" intent="<ext-name>.<cmd-id>"`. Model: VSCode `registerCommand` (handler) + `contributes.keybindings` (key), JetBrains `EmptyAction` reservation. User-scene binding ALWAYS wins. Colliding suggested-default chords across two extensions → warning + leave unbound.

- [ ] **`ui/pane/request` narrow scope.** Two-tier model:
    - Declarative layer (scene KDL): scene declares named pane slots (VSCode `viewsContainers` analog) via `layout { pane-slot name="<id>" … }`. Extensions CONTRIBUTE to slots via their sidecar fragment.
    - Imperative RPC (`ui/pane/request`): extension can ONLY (a) fill a slot type it contributed to a slot the user composed into their layout, OR (b) open an ephemeral overlay (floating pane, diff viewer). Extensions CANNOT synthesize new slot types at runtime — only fill existing ones. Attempting to request a slot type not declared in the user's scene = `ext-proto/slot-not-declared`.

- [ ] **Version-bump policy** (MCP-synthesis; 12 rules):
    1. Add new method → MINOR.
    2. Rename or remove method → MAJOR.
    3. Add optional field to request/response → MINOR (receivers MUST ignore unknown fields).
    4. Add required field → MAJOR.
    5. Narrow a field's type → MAJOR.
    6. Widen enum → MINOR if spec mandates default-fallback handling; MAJOR if clients must branch.
    7. Change semantics of existing method without schema change → MAJOR.
    8. Add new capability flag → MINOR.
    9. Add new event/notification type → MINOR.
    10. Reorder positional params / reused IDs / changed error codes → MAJOR.
    11. Deprecate a method → MINOR; removal requires MAJOR + ≥ 1 minor warning window.
    12. Add new transport → MINOR.
- [ ] **Version-negotiation wire format:** dual scheme — `initialize` carries `{ protocolVersion: "MAJOR.MINOR" (semver, no patch), capabilities: {…} }`. `protocolVersion` is the coarse epoch gate (MCP-style, but semver not YYYY-MM-DD so extension manifests can express ranges like `>= 1.2, < 2.0`). Capability flags are fine-grained feature detection (LSP-style, orthogonal).
- [ ] **Out-of-range policy (3 tiers):**
    - Extension's declared ark-protocol range excludes running ark, ark is NEWER → warn, run in best-effort mode (missing methods return JSON-RPC `-32601`). `--strict` flag hard-fails.
    - Extension's range excludes running ark, ark is OLDER → hard error, refuse to load (`ext-proto/unsupported-version`).
    - In-range → load normally; graceful degradation via capability flags.

- [ ] Agent-lifecycle / engine methods are explicitly OUT of this surface — ark uses ACP for that (see R17). Extensions that manage agents speak ACP as subprocesses of themselves; they do not reinvent agent plumbing via the extension protocol.
- [ ] `initialize` handshake carries `{protocolVersion, clientCapabilities}` (ark) → `{protocolVersion, extensionCapabilities, extensionInfo}` (extension). Object-of-objects capabilities per R10 (MCP-style; not boolean soup).
- [ ] Long-running operations MUST return a `task` handle (`task/create`). Clients poll `task/get` or subscribe to `$/progress` notifications. Every request is cancellable via `$/cancel`. Late responses arriving after cancel/timeout = silently dropped + debug log (MCP cancellation semantics).
- [ ] Per-call timeouts: ark enforces a default 5s timeout on in-flight requests; extensions can request longer via `$/progress` heartbeats.
- [ ] Protocol-first, trait-derived: canonical JSON-RPC spec generated from the Rust trait via facet reflection. Subprocess + wasm extensions implement the spec in their language; compiled-in Rust extensions implement the trait directly. One source of truth.
- [ ] Supervision: subprocess extensions are supervised children — shutdown sequence is stdin-close → wait 2s → SIGTERM → SIGKILL. Crash → log `error[ext/crashed]` + emit `UserEvent:ark.ext.crashed { name, exit_code, stderr_tail }` for scene to react. No auto-restart v1.
- [ ] Per-extension session token issued at `initialize`; reverse-requests gated by the token + extension's declared capabilities.
- [ ] **Runtime-registered UI state is ephemeral** (extension-lifetime). An extension that registers command IDs via `ui/keybind/register` MUST unregister them in `shutdown`, and ark MUST drop them automatically on crash. Scene-declared keybinds (R5) are session-long; runtime-registered command IDs are extension-long. The two surfaces never alias.

### R17: ACP integration (coding-agent protocol)
**Description:** Ark is a first-class client of the Agent Client Protocol ([agentclientprotocol.com](https://agentclientprotocol.com)) — the open standard (Zed-originated, Linux-Foundation-governed) for editor↔coding-agent JSON-RPC. Engines are ACP agents; ark does not invent an agent protocol.
**Acceptance Criteria:**
- [ ] Ark bundles the `agent-client-protocol` Rust crate as a workspace dependency. Version pinned; recorded in `cavekit-distribution`.
- [ ] **Supported ACP method surface v1 (stable, from `schema/meta.json`):**
    - Client → agent: `initialize`, `authenticate`, `session/new`, `session/load`, `session/list`, `session/prompt`, `session/cancel` (notif), `session/set_mode`, `session/set_config_option`.
    - Agent → client (ark handles as reverse-requests): `session/update` (notif, stream), `session/request_permission`, `fs/read_text_file`, `fs/write_text_file`, `terminal/create`, `terminal/output`, `terminal/wait_for_exit`, `terminal/kill`, `terminal/release`.
- [ ] **Unstable ACP ops gated behind capability negotiation.** Methods from `schema/meta.unstable.json` (`session/fork`, `session/resume`, `session/set_model`, `logout`, `session/close`, `document/did*`, `providers/*`, `nes/*`, `elicitation/*`, `$/cancel_request`) are NOT part of v1. Exposed only when the engine advertises support in `initialize` capabilities; surfaced as `ark.acp.unstable.*` events and ops. Scene usage of unstable ops without capability = `error[acp/unstable-not-supported]`.
- [ ] `AgentSpec` resolution chain for selecting an engine (across layers: last-wins / most-specific-wins, per Helm + kubectl convention):
    1. `--engine NAME` CLI flag (explicit override)
    2. Scene's `engine { name "…"; command "…"; args … }` block (direct ACP launch spec)
    3. Scene's `use "…"` of an extension declaring `capabilities { agent { engine { speaks "acp" } } }` which supplies a launch spec
    4. Config default (`engines.<name>` in `config.toml`)
    5. Hardcoded fallback (`claude` with `--acp` argv)
- [ ] **Intra-scene mutual exclusion** (clap / Cargo `workspace+local` convention): a single scene file containing BOTH an inline `engine { }` block AND `use "engine-*"` (an extension advertising `agent.engine.speaks = "acp"`) = `error[scene/engine-conflict]` with carets on both declarations. User picks one. If the restriction is ever relaxed in a future version, inline `engine { }` wins over `use "engine-*"`.
- [ ] Ark exposes ACP-session events onto the internal event bus as `AgentEvent::UserEvent { name: "ark.acp.<kind>", payload: {…}, source: "core" }` — scene reactions can observe `session/update` streams, tool calls, permission requests. Per-event `kind`s include: `turn_started`, `turn_finished`, `agent_message_chunk`, `plan`, `tool_call`, `tool_call_update`, `permission_requested`, `permission_responded`, `fs_read`, `fs_write`, `terminal_created`, `terminal_exited`.
- [ ] **Turn lifecycle tracking** (critical for hot-reload gating per R14):
    - Per ACP session, ark tracks `turn_inflight: bool`, set true on `session/prompt` send, cleared only when the corresponding response arrives with any `stopReason` (`end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`).
    - Cancel contract: scene `acp/cancel` (R7 op 15) sends `session/cancel` notification; the engine MUST still respond to the outstanding `session/prompt` with `stopReason: cancelled` per ACP spec. `acp/cancel` op BLOCKS (up to 5s) waiting for that response, to give the reload gate a clean edge.
    - No spec-defined idle probe exists. "Safe reload window" is defined strictly as "no `session/prompt` awaiting response on any tracked session."
- [ ] **Tool-permission dispatch (5-tier precedence, Zed convention):**
    1. On `session/request_permission` arrival, ark correlates via JSON-RPC `id` (no custom correlation token).
    2. Ark emits `UserEvent:ark.acp.permission_requested { request_id, tool, params, options }`.
    3. Rule evaluation order (first match wins):
       a. **security-deny** — ark-core hard rules (e.g., absolute-path writes outside scene root, deferred v0.4+).
       b. **auto-deny** — scene/ext rule `on "UserEvent:ark.acp.permission_requested" if="…" { emit "ark.acp.permission_response" outcome="reject_always" }` matches.
       c. **auto-confirm** — a scene rule explicitly forces the picker despite other rules matching ("force human") — emits `ark.acp.permission_request_picker` instead of directly responding.
       d. **auto-allow** — scene/ext rule emits `outcome="allow"`.
       e. **default** — fall through to picker plugin (ark-picker) modal.
    4. Configurable timeout (`config.toml` key `[acp] permission_timeout_ms` default 300000 for interactive / 0 for `ARK_NONINTERACTIVE=1` or CI). Timeout = respond `outcome: reject_once` with `option_id = "timeout"` (NOT ACP `Cancelled` — that verb is reserved for `session/cancel`-driven aborts).
    5. Late responses (user answers AFTER timeout fired) are dropped silently + debug log (MCP cancellation convention). Picker plugin MUST check request validity before sending.
    6. Scene op `acp/permit` (R7 op 16) is how reactions and pickers both respond; ark routes to ACP regardless of source.
- [ ] Engines are NOT ark extensions via the extension protocol. They speak ACP, not the extension protocol. An "engine extension" that wraps a non-ACP agent (e.g., an aider adapter) is itself an ark extension that speaks the extension protocol AND spawns an ACP process; the adapter's job is to translate aider's native stdio into ACP.
- [ ] `ark doctor` verifies the default engine's `--acp` argv spawns cleanly and responds to `initialize` in < 1s. Failure = actionable diagnostic naming the resolved engine command + stderr tail.

### R15: Migration + backward compatibility
**Description:** Existing ark installations using layout-only KDL files continue to work without modification.
**Acceptance Criteria:**
- [ ] File-shape detection rules (applied at scene load):
    - (a) File has `scene "<name>" { }` wrapper → use directly.
    - (b) File has top-level `layout { }` and no `scene` wrapper → auto-wrap as `scene "default" { layout { … } }` + emit debug log.
    - (c) File has neither → `error[scene/empty-or-unknown]` with a help message suggesting the wrap form.
    - (d) File has both `scene "<name>" { }` AND a top-level (non-scene-child) `layout { }` → `error[scene/ambiguous-file-shape]` with carets on both nodes; user must delete one.
- [ ] Shipped layouts in `crates/mux/zellij/layouts/*.kdl` migrated to scene format over time; old format remains supported indefinitely.
- [ ] `cavekit-layouts.md` R1–R6 remain valid; scene is a superset, not a replacement.
- [ ] User's existing `${XDG_CONFIG_HOME}/ark/layouts/<stem>.kdl` overrides still resolve; a `.scene.kdl` sibling takes precedence when present.
- [ ] `ark scene check` on a pre-scene layout file passes (reports "layout-only scene, 0 reactions, 0 keybinds, 0 plugins").

## Runtime model

### Scene compile pipeline

```
scene.kdl (source)
    ↓ parse (kdl 6.5.0 → knuffel derive)
SceneIR { extends, includes, uses, layout_ast, plugin_decls, reactions, keybinds }
    ↓ resolve extensions (walk search path, read wasm metadata, parse fragments)
    ↓ topo-sort contributions (extensions → fragments → user)
    ↓ merge per R11 rules
    ↓ validate (schema, refs, CEL compile, intent registry)
CompiledScene
    ↓ split
├── render → zellij layout KDL (for --layout) — strip when= false branches, inject keybinds{}
├── register → EventBus subscribers (one per `on` block; selector + CEL predicate + ops)
├── manifest → plugin lifecycle manager (always / summon / on-event / subscribes)
└── index → intent registry (core + namespaced from extensions)
```

### At session spawn

1. Supervisor compiles scene. Compile errors = abort spawn with miette diagnostic.
2. Write rendered zellij layout to `${runtime}/ark/layouts/{id}-scene.kdl`.
3. Inject `plugin "ark-bus"` decl (auto-prepended if user didn't include it explicitly).
4. Invoke zellij with `--layout <path>`. ark-bus plugin loads first, registers pipe endpoints.
5. Mount always-on plugins via `launch-or-focus-plugin`.
6. Register every `on` block as an event-bus subscriber.
7. Register summon/event-mount plugins as dormant subscribers.

### At runtime — event

1. `AgentEvent` broadcasts on bus.
2. Lookup reactions by `EventKind` → candidates.
3. For each candidate: evaluate field patterns + CEL predicate with context (`event`, `payload`, `agent`, `session`). False → skip.
4. For each surviving reaction: render runtime-templated op args; dispatch ops sequentially through intent registry.
5. Ops invoke mux operations, pipes, plugin launches, emits (cascade).

### At runtime — keybind

1. User presses chord in zellij.
2. Zellij `bind "Alt p" { MessagePlugin "ark-bus" { name "ark-intent"; payload "<JSON>"; } }` fires.
3. ark-bus receives pipe; JSON decoded → intent name + args OR op list.
4. Single-intent: dispatch through registry.
5. Op list: same path as reaction ops.

### Plugin mount lifecycle

- **always**: mounted post-`zellij --layout` via `launch-or-focus-plugin`. Lives until session ends.
- **summon**: registered as listener for `summon` selector; on first match, `launch-or-focus-plugin`. On `dismiss` match, close pane/float.
- **event-mount**: registered as event-bus subscriber for plugin's `on` selector; first match → `launch-or-focus-plugin`; subsequent matches focus existing pane. On `dismiss` match, close.

## Out of Scope (v1)

- **Multi-version same-extension** (two versions loaded concurrently). Single version per name per scene.
- **User-defined CEL functions** — extension point reserved; not exposed v1.
- **Rhai/Lua scripting** — `exec script="…"` shell escape hatch only. Richer scripting is v2+.
- **Chord sequences** (`<leader>ff`) — single-chord keybinds only v1. Zellij mode-based sequences via ark-bus intercept is v2.
- **Runtime plugin hot-swap from scratch** — changing plugin `source` requires `start-or-reload-plugin`; wasm state is lost. Documented.
- **Merge of `layout` blocks across extensions** — extensions can only mount plugins and declare reactions; layout remains user-authored v1. Tab templates via `layout extends="base"` deferred to v0.3+.
- **Signed extensions / cryptographic trust** — capability prompt is declare-only v1. Signing is v0.4+.
- **Scene registry / search index** — extensions installed by URL/git only v1.
- **Ark intents exposed as ACP tools** — scene `ark.core.*` ops (and extension-namespaced intents) are NOT advertised to the ACP engine as callable tools in v1. Ark is strictly an ACP client that drives the agent; the agent does not call back into ark's intent surface. Explicit no. Revisit post-v1 if demand emerges.
- **Agent-as-ark-controller pattern** — relatedly, no v1 surface for an agent to `emit` ark `UserEvent`s or dispatch intents. Agents affect ark only through ACP spec verbs (`session/update`, `session/request_permission`, `fs/*`, `terminal/*`).

## Cross-References

- cavekit-mux-zellij.md — zellij integration, layout file rendering, pipe targets.
- cavekit-layouts.md — existing layout KDL authoring conventions; scene R15 preserves compatibility.
- cavekit-types-state-events.md — `AgentEvent` enum; R4 selectors match on kind + fields.
- cavekit-supervisor.md — scene compilation + event bus registration happens in supervisor.
- cavekit-config.md — figment-layered config for scene path defaults; `ark.scene.watch` toggle.
- cavekit-plugin-status.md + cavekit-plugin-picker.md — shipped plugins referenced as `ext:status` / `ext:picker`; scene fragments shipped alongside.
- cavekit-cli.md — new `ark scene` + `ark ext` subcommand trees.
- cavekit-hook-ipc.md — ark-bus plugin IPC surface; adds `ark-intent` pipe endpoint.
- cavekit-engine-claude-code.md — will be rewritten v0.3+ as an ACP client launch spec (per R17); today's hook-injection + transcript-tailing becomes irrelevant once Claude speaks ACP natively.
- [Agent Client Protocol (external standard)](https://agentclientprotocol.com) — R17 authoritative reference.

## Design Decisions Locked

| Decision | Value | Why |
|---|---|---|
| KDL version | 2.0 | Stable, future-proof |
| Parse stack | `facet` + `facet-kdl` + `kdl` 6.5 (formatter) + `miette` | Active sponsorship (AWS/Zed/Depot); one derive covers parse/reflect/schema/LSP-hover. Knus author explicitly names facet-kdl as successor. |
| Schema | Runtime-derived from facet SHAPE (no hand-maintained `.kdl-schema`) | Zero drift; `ark scene schema-dump` regenerates on demand |
| Expression language | CEL via `cel-interpreter` — canonical stdlib + custom `glob()` | Standardized, RE2-safe, sandboxed |
| Template engine | `minijinja` | Already in use; two context scopes |
| Extension noun | **ext / extension** | Chosen over kit/pack/addon |
| Single-file scene | Yes, preprocessed superset | Co-location > split-file purity |
| Keybind dispatch | Layout-embedded `keybinds{}` → `MessagePlugin "ark-bus"` | Zellij merges additively with user config (B2) |
| Wasm metadata location | wasm custom section `ark.metadata`, **facet SHAPE bytes** | Self-describing binaries; reflection drives LSP hover/completion |
| `use` semantics | Compile-time include: resolves path + inspects wasm SHAPE + merges fragment | Pure scene-compile op, no runtime magic |
| Namespacing | Mandatory `<owner>.<name>`; user-scope auto-prefixed `user.*` | Prevents shadowing, enables `ark scene graph` attribution |
| Profile isolation | `ARK_APPNAME` env → `~/.config/<appname>/` (NVIM_APPNAME pattern) | Multi-profile support follows nvim idiom |
| Scene selection | `--scene NAME` / `ARK_SCENE` env | Select within a profile's `scenes/` dir |
| ark-bus mount | Zellij suppressed-pane API (first-class `hidden`) | Idiomatic; not a layout hack |
| Capability model | Phased: v0.3 publisher-trust prompt → v0.4 declared caps in SHAPE → v0.5+ runtime enforcement | VSCode-1.97-style → Chrome-extension-style → full sandbox |
| Agent protocol | **ACP** (Agent Client Protocol, external open standard) | Don't reinvent; ecosystem already converged (Claude, Codex, Gemini CLI). Ark is an ACP client; engines are ACP agents. |
| Extension protocol | **Ark's own JSON-RPC 2.0 over NDJSON/in-proc/wasm-component** (~22 methods, trait-derived spec, MCP-style capabilities, task-handle for long ops) | One protocol, three delivery modes. Distinct from ACP (engines) to keep concerns clean. Future-proofed via object-of-objects capabilities. |
| Delivery modes | compiled-in (workspace crate) · subprocess (any-language) · wasm-component (WASI p2) | Zed pattern — single trait, multiple transports. Replaces the `kind` enum anti-pattern. |
| Naming (extension protocol) | No acronym in prose. Brand as "the ark extension protocol" or "AEP" only when a spec handle is needed. | Avoids ACP/APC visual collision. |
| Naming (config artifact) | **scene** | Theatrical — "what's on stage + what cues fire" |
| `plugin` vs `use` keyword | `plugin` = zellij wasm (matches Zellij upstream KDL); `use` = ark-native extension (matches Emacs use-package + Rust use) | Two distinct concepts; compiler enforces separation |
| `host/*` scope | Syscall-proxies (`host/fs/*`, `host/proc/*`, `host/net/*`) are **wasm-component only**. `workspace/*` intent channels are all delivery modes. | LSP-style reverse-request split — sandbox-escape vs user-intent |
| Extension protocol version | Dual: semver `MAJOR.MINOR` `protocolVersion` string (MCP pattern, no YYYY-MM-DD so ranges work) + capability flags (LSP pattern, fine-grained) | Coarse epoch gate + orthogonal feature flags — MCP learned pure capability-bag = N² matrix |
| Extension out-of-range | 3-tier: newer ark = warn + best-effort (`--strict` override); older ark = hard error; in-range = cap flags | Matches VSCode practical reality + MCP "disconnect on mismatch" for older |
| Permission dispatch precedence | Zed 5-tier: security-deny → auto-deny → auto-confirm → auto-allow → picker fallback | `always_confirm` above `always_allow` lets you force picker even when default=allow |
| Permission timeout | Configurable; default 300000ms interactive / 0ms CI; expiry → `outcome: reject_once` (NOT ACP `Cancelled`) | `Cancelled` reserved for `session/cancel` per ACP spec |
| Mid-turn reload gate | Track `turn_inflight: bool` per ACP session; queue reload until every session receives a `stopReason` | Only spec-grounded safe edge; no ACP idle probe exists |
| Engine-spec intra-scene | Inline `engine { }` + `use "engine-*"` in one scene = compile error (clap/Cargo convention) | No natural specificity ladder at same scope; error beats silent override |
| Keybind surface split | Scene KDL = key→command-id (session-long); `ui/keybind/register` = command-id+metadata (ext-lifetime). User scene always wins | VSCode `registerCommand` + `contributes.keybindings` + JetBrains `EmptyAction` |
| UI pane surface | Declarative pane slots (scene) + imperative fill-slot-or-ephemeral (`ui/pane/request`); extensions cannot synthesize slot types at runtime | VSCode `viewsContainers` + `createWebviewPanel` — LSP-anemia rejected |
| UserEvent attribution | `source` field canonical values: `core` / `ext:<n>` / `plugin:<n>` / `hook:<n>` / `scene` | Keeps `ark scene explain` consistent; compiler rejects empty |
