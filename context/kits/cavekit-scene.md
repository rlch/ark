---
created: "2026-04-15"
last_edited: "2026-04-16"
supersedes: cavekit-scene.md
---

# Spec: Scene — Reactive KDL Configuration + Extension System (v3)

## Scope

Ark is a **terminal IDE**. AI is one extension capability, not the core.

Ark's extensibility has **two layers**:

| Layer | What | Audience | Lifecycle |
|---|---|---|---|
| **Scene** (this spec) | User-facing KDL config declaring layout + reactions + keybinds + extension activation + composition | Scene author (end user) | Parsed at `ark` launch; reconciled at runtime |
| **Extension** (this spec) | Bundles providing views, intents, events. Three delivery modes: compiled-in, subprocess, zellij-wasm. Includes ACP agent capability. | Extension author | Session-long; loaded when `use`d |

The scene is the one artifact a user writes by hand. Extensions provide capabilities. ACP (Agent Client Protocol) is an extension capability, not a separate layer — any extension can speak ACP by declaring `capabilities { agent { speaks "acp" } }`.

One scene file = one composed configuration for one `ark` session.

## Requirements

### R1: Scene file grammar

**Description:** A scene is a KDL 2.0 document with a single top-level `scene "<name>" { … }` node.

**Acceptance Criteria:**
- [ ] Single top-level `scene` node. Multiple = parse error.
- [ ] Scene-root body admits: `use`, `include`, `layout`, `mode`, `on`, `bind`, `clear-reactions`, `clear-bind`, `disable-extension`. Unknown node = parse error with "did you mean …?" suggestion.
- [ ] Node ordering semantically irrelevant for `use`, `include`, `layout`, `mode`, `clear-*`, `disable-extension`. EXCEPTION: `on` blocks and `bind` blocks execute in textual order within a scene file. `ark scene fmt` preserves relative order of `on`/`bind` nodes.
- [ ] Parser uses `facet-kdl` derive macros (`#[derive(Facet)]`) with span info preserved for every node.
- [ ] Parse errors surface via `miette::Diagnostic` with file name, line/col, caret, help text.

### R2: Scope rules

**Description:** Every construct has a well-defined set of legal parent nodes.

**Acceptance Criteria:**
- [ ] `use`, `include`, `on`, `bind`, `mode`, `clear-reactions`, `clear-bind`, `disable-extension` legal only at scene root.
- [ ] `tab` legal only inside `layout { }`. No bare `pane`/`row`/`col` at layout root.
- [ ] `row`, `col`, `pane` legal inside `tab` or nested inside another `row`/`col`.
- [ ] `when=` attribute legal on `tab`, `pane`, `row`, `col`, and individual op nodes inside `on`/`bind` bodies.
- [ ] `@handle` required on every `tab` and `pane` node. Compile error if missing.
- [ ] Handle namespace is flat and scene-scoped. Tab + pane handles share one namespace. Duplicate handle = `error[scene/handle-clash]`.
- [ ] Scope violation produces `error[scene/misplaced-node]` with parent-node context.

### R3: Layout DSL

**Description:** Ark owns the layout DSL. Zellij is a rendering backend, not a vocabulary source. The `layout { }` block compiles to zellij-compatible KDL at spawn time.

**Acceptance Criteria:**

**Structure:**
- [ ] Layout body contains only `tab @handle { … }` nodes. No bare panes/rows/cols at root.
- [ ] `tab @handle` attributes: `cwd` (string, Rhai interp), `name` (string), `focus` (bool, exactly one per layout), `when` (Rhai predicate).
- [ ] `row { … }` = horizontal split. `col { … }` = vertical split. Compile to zellij `pane split_direction="horizontal"/"vertical"`.
- [ ] `pane @handle` = leaf node. Must contain exactly one view child node (the content). Compile error if zero or >1.

**Sizing — spans:**
- [ ] `span=N` — relative weight within container. Siblings normalize to 100% at render. Compiles to zellij `size="N%"`.
- [ ] `cells=N` — fixed N cells. Compiles to zellij `size=N`.
- [ ] `min=N` / `max=N` — bounds in cells.

**Overlays (floating panes):**
- [ ] `pane @handle overlay pos=<pos> size=<WxH>` declares a floating pane. Tab-scoped.
- [ ] `pos` accepts: `top-right`, `top-left`, `bottom-right`, `bottom-left`, `center`, or explicit `X%xY%`.
- [ ] `size` accepts: `WxH` (cells) or `W%xH%` (percentage).
- [ ] `sticky=true` → zellij `pinned=true` (survives tab switch).
- [ ] Compiles to zellij `floating_panes { pane name="handle" x=… y=… width=… height=… }`.

**View resolution:**
- [ ] View child node name resolved via registry: primitives → compiled-in extensions → user extensions → project-local extensions. First match wins.
- [ ] Unknown view = `error[scene/unknown-view]` with suggestions from registry.

**Env wrapper for pane identity:**
- [ ] Every pane command wrapped with `env ARK_HANDLE=@<handle> <cmd>`. Makes commands unique for override-layout matching.
- [ ] Wrapper is transparent to user — pane process has extra env var but runs normally.

**Rendering:**
- [ ] Rendered output written to `${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl`.
- [ ] Rendered output passes `kdl::KdlDocument::parse` before handoff to zellij.

### R4: Reactions (`on` blocks)

**Description:** A reaction is an event selector + optional Rhai predicate + ordered op list.

**Acceptance Criteria:**
- [ ] Selector syntax: `on <EventKind> field=pattern … { ops }`. Event kind is a bare identifier; field patterns are KDL properties.
- [ ] Field names validated against `AgentEvent` variant fields via facet SHAPE. Unknown field = `error[scene/unknown-event-field]` with suggestions.
- [ ] Field pattern default match types: glob for path-like, exact for strings/enums. Override via `(glob)`, `(exact)`, `(regex)` type annotations.
- [ ] `when="<Rhai>"` attribute on `on` block: evaluated per fire; false = skip reaction.
- [ ] `when="<Rhai>"` also legal on individual op nodes inside the body — per-op guards.
- [ ] Predicates containing string literals MUST use KDL raw strings (`when=#"agent.phase == "review""#`) because Rhai uses double quotes. Formatter (`ark scene fmt`) auto-converts plain → raw when predicate body contains `"`.
- [ ] **Selector-captured locals:** field patterns bind as locals in the op body. `path="**/*.md"` matching `src/README.md` → `{path}` evaluates to `"src/README.md"` in op args.
- [ ] **UserEvent payload hybrid access:** for UserEvent, bare field names route into `payload`. Reserved top-level keys: `name`, `source`, `payload`. `payload.` prefix available as explicit escape hatch.
- [ ] Multiple `on` blocks with overlapping selectors each run; no silent dedup.
- [ ] Op failure logs `error[scene/op-failed]`; remaining ops in that reaction skipped; event loop continues.
- [ ] `emit` op cascade depth bounded at 4 (default), configurable via `scene "<name>" max-cascade-depth=<N>`.

### R5: Keybinds (`bind` blocks)

**Description:** `bind` declarations compile into zellij `keybinds { }` block.

**Acceptance Criteria:**
- [ ] Syntax: `bind "<chord>" { <ops> }`. Chord uses zellij notation (quoted, space-separated modifiers: `"Alt d"`, `"Alt Shift v"`, `"Ctrl c"`).
- [ ] Key string validated against zellij's key chord lexer at compile time.
- [ ] Block body uses same op grammar as `on` reactions.
- [ ] Compiled to: `bind "<chord>" { MessagePlugin "ark-bus" { name "ark-intent"; payload "<JSON>"; } }`.
- [ ] Keybinds added WITHOUT `clear-defaults=true` so user zellij config binds survive.
- [ ] Last-wins per chord across scene + included fragments.
- [ ] `clear-bind "<chord>"` removes a specific inherited bind.

### R6: Views

**Description:** A **view** is what fills a pane. Replaces the prior "plugin" concept. One view per pane.

**Acceptance Criteria:**
- [ ] Three tiers, same namespace:
  - **Primitives:** `command`, `shell`, `edit` — kernel-builtin, map 1:1 to zellij native content types.
  - **Shipped:** `diff`, `status`, `picker` — compiled-in extensions bundled with ark.
  - **User-installed:** `nvim`, `glow`, `lazygit` — via `ark ext add`.
- [ ] View child node syntax: `pane @handle { <view-alias> <attrs> }`. Attrs are view-specific config, schema-validated against the view's facet SHAPE.
- [ ] `command` primitive: `pane @x { command cmd="X" args=["A","B"] }` → zellij `pane { command "env" "ARK_HANDLE=x" "X"; args "A" "B" }`.
- [ ] `shell` primitive: `pane @x { shell }` → zellij `pane { command "env" "ARK_HANDLE=x" "$SHELL" }`.
- [ ] `edit` primitive: `pane @x { edit path="file.rs" }` → zellij `pane { edit "file.rs" }`. Opens in `$EDITOR`.
- [ ] View rendering mode determined by Rust trait impl:
  - `CommandView` → pane runs a command binary (zellij native subprocess).
  - `ZellijView` → pane loads a zellij wasm plugin.
- [ ] Extension views receive typed pane handles: `CommandPane` (for CommandView) or `PluginPane` (for ZellijView). Compiler validates intent target types.

### R7: Op vocabulary

**Description:** Canonical op set. Each op maps to a named intent. Extensions register additional namespaced intents.

**Acceptance Criteria:**

**Pane/tab ops (polymorphic via typed handles):**
- [ ] `focus @handle` — tab or pane (compiler resolves from handle type).
- [ ] `close @handle` — tab or pane.
- [ ] `rename @handle to="name"` — tab only.
- [ ] `resize @handle direction=<dir> by=<inc|dec>` — pane only.
- [ ] `move @handle to=<anchor>` — pane.
- [ ] `pin @handle` / `unpin @handle` — overlay pane.

**Spawn ops:**
- [ ] `spawn @handle { <view> }` — create tiled pane.
- [ ] `spawn @handle overlay pos=<pos> size=<WxH> { <view> }` — create overlay pane.
- [ ] `new_tab @handle name="name" cwd="path"` — create tab.

**Mode ops:**
- [ ] `use_mode "name"` — switch active tab to named mode layout.
- [ ] `use_mode "default"` — revert to primary layout.

**Messaging ops:**
- [ ] `pipe from=@handle to=@handle payload="…"` — multi-target, both panes.
- [ ] `emit "<event-name>" { <kv payload> }` — emit UserEvent.
- [ ] `set_status text="…" severity=<level> ttl_ms=<int>` — global, status bar extension.

**ACP ops (sub-namespaced `acp.*`):**
- [ ] `acp.prompt text="…"` — send user message into ACP agent session.
- [ ] `acp.cancel` — cancel in-flight turn.
- [ ] `acp.permit request_id="…" outcome=<allow|reject_once|reject_always>` — respond to permission request.
- [ ] `acp.set_mode mode="…"` — set agent mode (plan/edit/etc).
- [ ] ACP ops no-op with warning if no ACP-capable extension active.

**Control ops:**
- [ ] `exec script="…" shell="…" timeout_ms=<int>` — run shell script.
- [ ] `reload_scene` — re-parse scene, reconcile.

**General:**
- [ ] Each op has a KDL schema (facet SHAPE). `ark scene check` validates.
- [ ] Unknown op = `error[scene/unknown-op]` with "did you mean …?" suggestions.
- [ ] All op string attrs support `{Rhai}` interpolation (see R8).
- [ ] Handle type mismatches = compile error: `error[scene/handle-type-mismatch]`.

### R8: Expression language (Rhai expression-only mode)

**Description:** `when=` predicates and `{expr}` interpolation use Rhai via the `rhai` crate in expression-only mode (non-Turing-complete: no `fn`, no `while`/`for`/`loop`, no assignment).

**Acceptance Criteria:**

**Engine config:**
- [ ] Engine built with `Engine::new_raw()` (no auto stdlib). Ark-owned helpers registered explicitly.
- [ ] Symbols disabled: `fn`, `while`, `for`, `loop`, `return`, `break`, `continue`, `=`, `+=`, `-=`, `*=`, `/=`, `%=`, `**=`, `<<=`, `>>=`, `&=`, `|=`, `^=`, `import`, `export`.
- [ ] Resource limits set: `set_max_expr_depths(32, 32)`, `set_max_operations(10_000)`, `set_max_string_size(4096)`, `set_max_array_size(256)`, `set_max_map_size(256)`.
- [ ] Predicates compiled via `engine.compile_expression(src)` (expression-only parse), cached as `AST` per unique source string; re-used across evaluations.

**Syntax:**
- [ ] `when="<Rhai>"` — bare expression, no braces. Always a Rhai expression returning `bool`.
- [ ] `{<Rhai>}` in string values — brace-delimited interpolation holes. Each hole is a Rhai expression; result coerced to string.
- [ ] Zero holes → verbatim string, no Rhai eval.
- [ ] Single-hole whole-value (`"{expr}"`) → typed pass-through (preserves `i64`/`bool`/etc. for typed op attrs).
- [ ] Mixed (`"text {expr} more"`) → coerce hole to string, concat.
- [ ] String literals inside Rhai predicates use double quotes (Rhai native). Single quotes are `char` literals in Rhai and will reject multi-char content — use raw KDL strings (`#"..."#`) for `when=` attrs containing `"`.

**Scopes:**
- [ ] Two evaluation scopes, enforced at compile:
  - **Spawn context** (layout values): bindings `cwd` (string), `id` (string), `name` (string), `env` (map of env vars). Rendered once at spawn.
  - **Event context** (reaction/bind ops): bindings `event` (map), `payload` (map), `agent` (map with `phase`, etc.), `session` (map) + selector-captured locals. Rendered per fire.
- [ ] Compile-time scope enforcement: layout can't see `event`; reactions can't see `cwd`. Mismatch = `error[scene/rhai-scope-mismatch]` with expected vs actual bindings.

**Registered helper functions (ark-owned):**
- [ ] `glob(path, pattern)` — RE2-flavored glob match; used under-the-hood for selector field patterns too.
- [ ] `matches(str, regex)` — regex match (RE2-backed; no backrefs).
- [ ] `basename(path)` / `dirname(path)` — path components.
- [ ] Rhai built-in string methods available as-is: `starts_with`, `ends_with`, `contains`, `len`, `to_upper`, `to_lower`, `trim`, `replace`, `split`.
- [ ] Rhai built-in array methods available: `len`, `contains`, `index_of`, `is_empty`.
- [ ] Rhai built-in `if { } else { }` expression form usable anywhere (replaces ternary).

**Diagnostics:**
- [ ] Parse errors surface via `miette::Diagnostic` with scene file + line/col + caret at the offending Rhai token (Rhai `Position` mapped back onto the containing KDL attribute span).
- [ ] Runtime errors (nil access, type mismatch, operation-limit exceeded) log `error[scene/rhai-eval]` with expression source + scope snapshot; reaction/op skipped; session continues.
- [ ] Operation-limit overrun = `error[scene/rhai-oom]`; treat as programmer error, not recoverable state.

**Cleanup:**
- [ ] No minijinja anywhere. `validate_kdl()` brace scanner deleted.
- [ ] No CEL anywhere. `cel-interpreter` dep removed.

### R9: Reconciler

**Description:** Scene KDL = desired state. Ark reconciles zellij toward it via `override-layout` (Kubernetes model).

**Acceptance Criteria:**

**Mechanism:**
- [ ] When `when=` predicate inputs change (Rhai scope update), re-evaluate all predicates.
- [ ] Render complete desired layout KDL (include/exclude panes+tabs based on truth values).
- [ ] Issue `zellij action override-layout <path> --retain-existing-terminal-panes --retain-existing-plugin-panes`.
- [ ] Zellij reconciles: retains panes matched by `invoked_with()` (command + args), creates missing, closes extras.

**Pane identity:**
- [ ] Every pane command wrapped with `env ARK_HANDLE=@<handle> <cmd>`. Different handle → different args → unique match in override-layout.
- [ ] Override-layout matching is by command + args (confirmed in zellij `layout_applier.rs:235`). Env wrapper ensures no ambiguity even for duplicate commands (e.g., two shell panes).

**Triggers:**
- [ ] `when=` predicate input changes → re-eval → override-layout (debounced 200ms).
- [ ] Scene file edit + save → re-read → override-layout (debounced 200ms).
- [ ] `use_mode "name"` op → render mode layout → `override-layout --apply-only-to-active-tab`.

**Drift tolerance:**
- [ ] User-initiated changes (manually close pane, add tab via keybind) are tolerated. Reconciler only forces convergence on `when=` transitions and mode switches.

**Modes:**
- [ ] Named alternate whole-tab layouts: `mode "name" { tab @handle { … } }`.
- [ ] `use_mode "name"` → render mode KDL → override-layout with `--apply-only-to-active-tab --retain-existing-terminal-panes --retain-existing-plugin-panes`.
- [ ] Handles survive swap. Same `@handle` across base + mode = same subprocess preserved.
- [ ] Modes do NOT use zellij swap_tiled_layout. Ark modes are explicit, not pane-count-triggered.

### R10: Extensions

**Description:** Everything is an extension. No shipped-vs-user distinction in format or API. Extensions provide views, intents, events, and optionally ACP agent capability.

**Acceptance Criteria:**

**Format parity:**
- [ ] Identical manifest format for compiled-in, subprocess, and wasm extensions.
- [ ] For compiled-in Rust extensions: manifest code-generated via `#[derive(Extension)]` + facet SHAPE. Zero manifest file.
- [ ] For subprocess extensions: hand-written `extension.kdl` alongside binary.
- [ ] For wasm extensions: manifest embedded as `ark.metadata` custom section in `.wasm`.

**Delivery modes (3 for v1):**
- [ ] `compiled-in` — Rust crate in workspace. In-process trait dispatch. Registered via `inventory`/`linkme` at boot.
- [ ] `subprocess` — any-language binary. NDJSON JSON-RPC over unix socket. Ark spawns protocol handler; view process runs in pane separately.
- [ ] `wasm` — zellij plugin runtime. Loaded by zellij. Ark protocol via pipe through ark-bus.

**Resolution (no central registry file):**
- [ ] `use "<name>"` resolves by scanning:
  1. Compiled-in registry (auto-registered at boot).
  2. User-installed: `~/.local/share/ark/extensions/<name>/`.
  3. Project-local: `.ark/extensions/<name>/`.
- [ ] Missing extension = `error[ext/missing]` with Levenshtein suggestions.

**Activation:**
- [ ] Lazy. Extension loaded only when scene `use`s it.

**Agent as extension capability:**
- [ ] No `agent { }` scene-root block. ACP is an extension capability.
- [ ] Extension manifest declares `capabilities { agent { speaks "acp" } }` + launch spec.
- [ ] Scene activates via `use "claude-code"`. ACP handshake at session start.
- [ ] ACP events emitted as `ark.acp.*` on the bus (protocol-level namespace, any ACP-speaking extension emits there).

**Extension can have protocol + views:**
- [ ] One extension, one `use`. Protocol handler (subprocess/compiled-in) + view renderer (CommandView/ZellijView) as two runtime components under one name.
- [ ] When pane mounts a view from a subprocess extension: ark starts protocol handler; zellij runs view command in pane; protocol handler connects to view process via app-native RPC.

**Scene fragments — opt-in:**
- [ ] Extensions may ship scene fragments (reactions, keybinds, layout snippets).
- [ ] Fragments NOT auto-merged on `use`. Scene author opts in via `include "ext:<name>/<fragment>"`.
- [ ] `ark ext info <name>` lists available fragments.

### R11: Composition

**Description:** Scenes compose via `use` (extension activation) and `include` (fragment splicing). No inheritance.

**Acceptance Criteria:**
- [ ] `use "<ext>"` — activates extension: views, intents, events enter scope. Transitive (extension's own dependencies resolve recursively). Cycle = `error[ext/cycle]`.
- [ ] `include "<path-or-ext:fragment>"` — splices a KDL fragment into the scene at include point. No merge logic. Fragment nodes inserted verbatim. Conflicts = compile error.
- [ ] No `extends`. Flat-first composition. Dropped by design (Docker Compose lesson: most users prefer copy-paste).
- [ ] Namespacing mandatory: `<owner>.<name>`. Owners: `ark.core.*` (reserved), `<ext-name>.*`, `user.*`.
- [ ] Context-sensitive unprefixed rewrite: user scene unprefixed `emit "foo"` → `user.foo`; extension fragment unprefixed → `<ext-name>.foo`.
- [ ] `clear-reactions event="<selector>"` removes matching reactions from included fragments.
- [ ] `clear-bind "<chord>"` removes matching keybind from included fragments.
- [ ] `disable-extension "<name>"` prevents an extension from activating.
- [ ] Load order: extensions (topo order) → includes (source order) → user scene (last). Reactions additive in load order. Keybinds last-wins per chord.

### R12: Diagnostics

**Description:** Every compile-time and runtime error surfaces via `miette`.

**Acceptance Criteria:**
- [ ] Error codes namespaced: `scene/*`, `ext/*`, `op/*`, `acp/*`. Rhai errors nest under `scene/rhai-*` (`scene/rhai-scope-mismatch`, `scene/rhai-eval`, `scene/rhai-oom`).
- [ ] All errors implement `miette::Diagnostic` with `code()`, `severity()`, `help()`, `labels()`.
- [ ] Every AST node retains origin span; included fragment contributions track source file + line.
- [ ] `ark scene check` exits non-zero on any error, prints every diagnostic.
- [ ] Runtime reaction errors logged at `warn`; do not crash supervisor.
- [ ] Test suite includes at least one unit test per error code verifying diagnostic output snapshot.

### R13: CLI surface

**Description:** Scene and extension CLI commands.

**Acceptance Criteria:**
- [ ] `ark` (bare) — launch default session. No subcommand needed.
- [ ] `ark --scene <name-or-path>` — launch named scene.
- [ ] `ark --session <name>` — attach-or-create named session.
- [ ] `ark scene check [path]` — parse + validate. Exit 0 on green.
- [ ] `ark scene fmt [path]` — canonical format. Idempotent.
- [ ] `ark scene dry-run --event '<selector>'` — print ops that would fire.
- [ ] `ark scene graph [path]` — text tree: extensions, views, reactions, keybinds.
- [ ] `ark scene explain <ref>` — where does this come from? Refs: `intent:<name>`, `bind:<chord>`, `view:<name>`, `reaction:<event>`.
- [ ] `ark scene reload --session <name>` — hot-reload.
- [ ] `ark scene schema-dump` — dump schema from facet SHAPE.
- [ ] `ark ext add <source>` — install from `github:`, `path:`, `url:`.
- [ ] `ark ext list` / `info <name>` / `inspect <path>` / `remove <name>` / `update [name]` — manage extensions.
- [ ] `ark ext info <name>` lists available scene fragments.
- [ ] `ark doctor` — diagnostics (verify default scene, extensions, ACP agent).
- [ ] No `ark spawn`. Bare `ark` = default session (matches zellij idiom).

### R14: Hot reload

**Description:** Scene file changes re-parsed and reconciled without restarting the session.

**Acceptance Criteria:**
- [ ] `reload_scene` op + `ark scene reload --session <name>` CLI both enter reconcile path.
- [ ] **Turn-inflight gate:** if any ACP session has a `session/prompt` awaiting response, queue reload. Apply when every session receives a `stopReason`.
- [ ] Reconcile algorithm:
  1. Re-parse + validate. On failure: keep old, surface error via `set_status`. Do NOT tear down.
  2. Re-evaluate all `when=` predicates with current context.
  3. Render new desired layout KDL.
  4. Issue `override-layout` (reconciler R9).
  5. Diff subscription set. Add new `on` blocks, drop removed.
  6. Diff keybinds. Issue `rebind_keys` for deltas.
- [ ] Single-slot re-entry guard: concurrent `reload_scene` while reload active = dropped + debug log.
- [ ] Reload telemetry event: `ark.scene.reloaded { duration_ms, status }`.
- [ ] File-watcher (optional, `[scene] watch = true` in `config.toml`): `notify` crate, debounced 200ms, ignores temp files.

### R15: Migration + backward compatibility

**Description:** Existing ark installations using layout-only KDL files continue to work.

**Acceptance Criteria:**
- [ ] File-shape detection:
  - `scene "<name>" { }` wrapper → use directly.
  - Top-level `layout { }` without `scene` wrapper → auto-wrap as `scene "default" { layout { … } }` + debug log.
  - Neither → `error[scene/empty-or-unknown]`.
  - Both `scene` AND bare `layout` → `error[scene/ambiguous-file-shape]`.
- [ ] Default scene embedded in ark binary as asset. User overrides via `~/.config/ark/scenes/default.kdl`.
- [ ] Default scene: 1 tab, 1 shell pane, status bar. No agent. No reactions.

### R16: Extension protocol (runtime RPC)

**Description:** JSON-RPC 2.0 contract between ark core and running extensions. Same message contracts across all delivery modes — only transport differs.

**Acceptance Criteria:**
- [ ] Method surface v1 (carried forward from prior spec with namespace updates):
  - Lifecycle: `initialize`, `initialized`, `shutdown`, `ping`
  - Events: `event/subscribe`, `event/unsubscribe`, `event/emit`, `event/notify`
  - Intents: `intent/register`, `intent/unregister`, `intent/dispatch`
  - UI: `ui/keybind/register`, `ui/keybind/unregister`, `ui/status/push`
  - Workspace: `workspace/applyEdit`, `workspace/configuration`, `workspace/showMessage`
- [ ] Version negotiation: dual scheme — semver `protocolVersion` + capability flags.
- [ ] Agent-lifecycle methods use ACP (external standard), not extension protocol.
- [ ] Per-call timeout 5s default; extensions extend via `$/progress` heartbeats.
- [ ] Supervision: subprocess shutdown sequence stdin-close → 2s → SIGTERM → SIGKILL. Crash → `error[ext/crashed]` event.

### R17: Rust DX — code-generated manifest

**Description:** Extension authors in Rust declare everything via derives + trait impls. Zero manifest files.

**Acceptance Criteria:**
- [ ] One crate = one extension. All derives in the same crate auto-group via `module_path!()`.
- [ ] `#[derive(Facet, Extension)] #[extension(name = "…")]` — extension identity + config schema.
- [ ] `#[derive(Facet, View)]` — view config schema. Exactly one view per pane.
- [ ] `#[derive(Facet, Event)]` — event payload schema. Event name auto-derived from struct name (snake_case).
- [ ] `#[ark::intent]` on methods:
  - On `impl ExtensionStruct` → global intent (no pane target).
  - On `impl ViewStruct` → targeted intent (pane handle required in scene).
- [ ] View render mode via trait impl:
  - `impl CommandView for V` → pane runs a command binary.
  - `impl ZellijView for V` → pane loads a zellij wasm plugin.
- [ ] Typed pane handles:
  - `CommandView` intents receive `&CommandPane` (`.env()`, `.write_stdin()`, `.pid()`).
  - `ZellijView` intents receive `&PluginPane` (`.pipe()`).
  - Both provide `.emit()` and `.handle()`.
- [ ] Events emitted via `ctx.emit(E)` (extension-scoped) or `pane.emit(E)` (view-scoped + source handle). Auto-namespaced by extension name.
- [ ] Extensions can only emit own events. Open subscription to any event. Scene-mediated cross-extension wiring.
- [ ] Extension dependencies: normal crate deps. Import event/intent types from other extension crates for type-safe subscription.
- [ ] Config ownership: extension owns schema + defaults (struct fields + `#[facet(default)]`). Scene author owns values (`use "ext" config { … }`). Ark validates at `ark scene check`.

## Runtime model

### Scene compile pipeline

```
scene.kdl (source)
    ↓ parse (kdl 6.5.0 + facet-kdl)
SceneIR { uses, includes, layout_ast, modes, reactions, keybinds }
    ↓ resolve extensions (scan dirs, read manifests, register views/intents/events)
    ↓ splice includes (verbatim insertion)
    ↓ validate (schema, refs, Rhai compile, handle types, view resolution)
CompiledScene
    ↓ evaluate when= predicates (spawn context)
    ↓ render desired layout KDL (with env ARK_HANDLE wrapper)
    ↓ split
├── layout KDL → ${XDG_RUNTIME_DIR}/ark/layouts/{id}-scene.kdl (for zellij --layout)
├── subscriber registry → one per `on` block (selector + Rhai predicate + ops)
├── keybinds → injected into layout KDL keybinds { } block
└── mode layouts → pre-rendered KDL per mode (for use_mode → override-layout)
```

### At session launch

1. Compile scene. Errors = abort with miette diagnostic.
2. Write rendered layout to runtime dir.
3. Launch zellij with `--layout <path>`.
4. Start extension protocol handlers (subprocess/compiled-in).
5. Register event-bus subscribers.
6. If ACP-capable extension active: start ACP handshake.

### At runtime — event

1. `AgentEvent` broadcasts on bus.
2. Lookup reactions by `EventKind` → candidates.
3. For each: evaluate field patterns + `when=` with context (`event`, `payload`, `agent`, `session`, captured locals). False = skip.
4. For each surviving reaction: render `{Rhai}` holes in op args; dispatch ops through intent registry.

### At runtime — reconciliation

1. Rhai scope changes (event updates `agent.phase`, etc.).
2. Re-evaluate all `when=` predicates.
3. If any truth value flipped: render new desired layout → override-layout (debounced 200ms).
4. Zellij converges: retains matched panes, creates missing, closes extras.

## Out of Scope (v1)

- **`extends` composition** — dropped. Flat-first via `include`.
- **`wasm-component` delivery (WASI p2 sandbox)** — deferred to v0.3+.
- **Multi-agent UX** — multi-`use` of ACP extensions works mechanically; UX design deferred.
- **Auto-merge sidecar fragments** — opt-in `include` only.
- **`stack` (tabbed pane cluster)** — deferred.
- **Reactive state** — modes cover 80%.
- **Swap-tiled-layout exposure** — modes supersede.
- **Multi-pane overlay containers** — single-pane overlay attr for v1.
- **`agent { }` scene-root block** — removed. Agent = extension.
- **`plugin { }` keyword** — removed. Views replace.
- **Minijinja** — removed. Rhai everywhere.
- **CEL** — removed. Rhai expression-only mode replaces it (2026-04-16 revision; see changelog).
- **`ark spawn` verb** — removed. Bare `ark` = default session.

## Cross-References

- cavekit-mux-zellij.md — zellij subprocess integration, layout rendering.
- cavekit-types-state-events.md — `AgentEvent` enum; R4 selectors match on kind + fields.
- cavekit-supervisor.md — scene compilation + event bus registration.
- cavekit-config.md — figment-layered config for scene path defaults.
- cavekit-cli.md — `ark scene` + `ark ext` subcommand trees.
- cavekit-hook-ipc.md — ark-bus plugin IPC surface.
- [Agent Client Protocol](https://agentclientprotocol.com) — ACP reference (extension capability, not core).

## Design Decisions Locked

| Decision | Value | Why |
|---|---|---|
| KDL version | 2.0 | Stable, future-proof |
| Parse stack | `facet` + `facet-kdl` + `kdl` 6.5 (formatter) + `miette` | Active sponsorship; one derive covers parse/reflect/schema/LSP-hover |
| Expression language | Rhai expression-only mode (`rhai` crate). CEL + Minijinja dead. | One language for `when=` + `{expr}` interpolation; Rust-native syntax; `Engine::new_raw` + symbol disables guarantee non-TC |
| View noun | "view" replaces "plugin" / "provider" | What fills a pane |
| View rendering | Trait-based: `CommandView` / `ZellijView` | Type system determines render mode |
| Pane handles | `@handle` required on all tabs + panes | Reconciler identity keys; compile-time validation |
| Handle namespace | Flat, scene-scoped | Simple; clash = compile error |
| Layout vocabulary | `row`/`col`/`span`/`cells`/`overlay` (ark-native) | Zellij is render backend, not vocabulary source |
| Sizing | Spans (relative weight) + cells (fixed) | Compose cleanly; no percentage arithmetic |
| Reconciler | `override-layout` + env `ARK_HANDLE` wrapper | Zellij does reconciliation; ark renders desired state |
| Modes | Named alternate layouts via override-layout | Explicit, not pane-count-triggered |
| Conditional | `when=` on tabs/panes → reconciler creates/removes | K8s desired-state model |
| Extensions | Everything is an extension; no shipped-vs-user distinction | Format parity (VSCode/HA/K8s pattern) |
| Agent protocol | ACP as extension capability | Not privileged; ark is a terminal IDE, not an AI terminal |
| Delivery modes (v1) | compiled-in / subprocess / zellij-wasm | 3 modes; WASI p2 deferred |
| Manifest DX | Code-generated from derives + trait impls; one-crate-per-extension; zero annotation | Cleanest Rust DX |
| Composition | `include` only; no `extends` | Flat-first; Docker Compose lesson |
| Scene fragments | Opt-in via `include "ext:name/fragment"` | No implicit behavior injection |
| Config ownership | Extension owns schema+defaults; scene owns values; ark validates | Clean three-way split (VSCode/K8s pattern) |
| CLI entry | Bare `ark` = default session | Match zellij idiom; no `spawn` verb |
| Default scene | Embedded asset; shell + status bar; no agent | Minimum viable terminal IDE baseline |
| Keybind notation | Zellij-native quoted strings (`"Alt d"`) | Direct pass-through; fewer bugs |
| ACP ops | Sub-namespaced `acp.*` | KDL-legal (dots); consistent; was `acp/` which is invalid KDL |
| Event emission | Own-namespace only; open subscription | Namespace integrity; trace-friendly |
| Cross-ext wiring | Scene-mediated (events → scene → intents) | Extensions stay decoupled |
| Naming | Scene (theatrical); Extension (VSCode/Zed convention); View (what fills a pane) | Industry-aligned |

## Changelog

### 2026-04-16 — v3 Convergence

- **Affected:** R1–R17 (all), plus 3 new requirements (R6 Views, R9 Reconciler, R17 Rust DX)
- **Summary:** Two design sessions converged into a full architectural revision. Major changes: own layout DSL (R3 rewrite), views replace plugins (R6 new), CEL-only interpolation (R8-R9 rewrite), extensions unified with agent-as-capability (R10/R17 rewrite), reconciler via override-layout (R9 new), composition via include-only (R11 rewrite), CLI redesign (R13). See `context/impl/impl-scene-architecture-v3.md` for the full design rationale.
- **Commits:** design-only; no code commits (pre-implementation)
