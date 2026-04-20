---
created: "2026-04-15"
last_edited: "2026-04-18"
supersedes: cavekit-scene.md
---

# Spec: Scene — Reactive KDL Configuration + Extension System (v3)

## Scope

Ark is a **terminal IDE**. AI is one extension capability, not the core.

Ark's extensibility has **two layers**:

| Layer | What | Audience | Lifecycle |
|---|---|---|---|
| **Scene** (this spec) | User-facing KDL config declaring layout + reactions + keybinds + extension activation + composition | Scene author (end user) | Parsed at `ark` launch; reconciled at runtime |
| **Extension** (this spec) | Bundles providing views, intents, events. Three delivery modes: compiled-in, subprocess, zellij-wasm. | Extension author | Session-long; loaded when `use`d |

The scene is the one artifact a user writes by hand. Extensions provide capabilities — views, intents, events, supervisor-side lifecycle hooks (see cavekit-soul.md Phase 2). Ark is agnostic to agent protocols; any engine integrates via the same extension pattern.

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
- [ ] `row`, `col`, `pane`, `stack` legal inside `tab` or nested inside another `row`/`col`/`stack`.
- [ ] `when=` attribute legal on `tab`, `pane`, `row`, `col`, `stack`, and individual op nodes inside `on`/`bind` bodies.
- [ ] `@handle` required on every `tab`, `pane`, and `stack` node. Compile error if missing.
- [ ] Handle namespace is flat and scene-scoped. Tab + pane + stack handles share one namespace. Duplicate handle = `error[scene/handle-clash]`.
- [ ] Scope violation produces `error[scene/misplaced-node]` with parent-node context.

### R3: Layout DSL

**Description:** Ark owns the layout DSL. Zellij is a rendering backend, not a vocabulary source. The `layout { }` block compiles to zellij-compatible KDL at spawn time.

**Acceptance Criteria:**

**Structure:**
- [ ] Layout body contains only `tab @handle { … }` nodes. No bare panes/rows/cols at root.
- [ ] `tab @handle` attributes: `cwd` (string, Rhai interp), `name` (string), `focus` (bool, exactly one per layout), `when` (Rhai predicate).
- [ ] `row { … }` = horizontal split. `col { … }` = vertical split. Compile to zellij `pane split_direction="horizontal"/"vertical"`.
- [ ] `pane @handle` = leaf node. Must contain exactly one view child node (the content). Compile error if zero or >1. See R6 for view-type declaration semantics.
- [ ] `stack @handle { … }` = dynamic container legal inside `tab`, `row`, `col`. Accepts static children at compile (optional) AND dynamic children spawned at runtime via the `spawn_into` op or a view's `StackHandle::spawn_pane` (R17). Empty body (`stack @h {}`) is legal; it declares a container that starts empty. Compiles to a zellij pane stack (one child expanded, siblings collapsed as header rows). Sized per `span=`/`cells=` just like `row`/`col`. Child expansion follows zellij-default behaviour (user-controlled, last-focused expands).

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

**Description:** A **view** is what fills a pane. Replaces the prior "plugin" concept. The view alias inside a `pane` or `stack` brace is *both* a content declaration AND a type declaration — the scene compiler produces typed handles (`Pane<V>` / `Stack<V>`, see R17) that downstream ops and view attrs can constrain.

**Acceptance Criteria:**
- [ ] Three tiers, same namespace:
  - **Primitives:** `command`, `shell`, `edit` — kernel-builtin, map 1:1 to zellij native content types.
  - **Shipped:** `diff`, `status`, `picker` — compiled-in extensions bundled with ark.
  - **User-installed:** `nvim`, `glow`, `lazygit` — via `ark ext add`.
- [ ] View child node syntax: `pane @handle { <view-alias> <attrs> }`. Attrs are view-specific config, schema-validated against the view's facet SHAPE.
- [ ] `command` primitive: `pane @x { command cmd="X" args=["A","B"] }` → zellij `pane { command "env" "ARK_HANDLE=x" "X"; args "A" "B" }`.
- [ ] `shell` primitive: `pane @x { shell }` → zellij `pane { command "env" "ARK_HANDLE=x" "$SHELL" }`.
- [ ] `edit` primitive: `pane @x { edit path="file.rs" }` → zellij `pane { edit "file.rs" }`. Opens in `$EDITOR`.
- [ ] View rendering mode is determined by which marker trait the view's Rust impl wears:
  - `impl CommandView for V` → pane runs a command binary (zellij native subprocess). Affords `Pane<V>::{env(), write_stdin(), pid()}`.
  - `impl ZellijView for V` → pane loads a zellij wasm plugin. Affords `Pane<V>::pipe()`.
  - Affordances are gated by the trait impl at the type layer — calling `pane.pipe()` on a `Pane<CommandSomething>` is a compile error in ext code, not a runtime check.
- [ ] **Pane view-type declaration:** `pane @h { foo attrs… }` declares the pane's static type as `Pane<FooView>`. This type flows into any intent, view attr, or op that references `@h` and enables compile-time view-type validation.
- [ ] **Stack view-type declaration:** `stack @h { foo }` declares the stack homogeneous in `FooView`: children added dynamically must be `Pane<FooView>`. `stack @h {}` declares the stack heterogeneous (`Stack<dyn View>`); any view may spawn inside. Mixed constraint: `stack @h { foo | bar }` declares `Stack<OneOf<FooView, BarView>>` (children may be either alias).
- [ ] **Pane view-type union:** `pane @h { foo | bar }` declares `Pane<OneOf<FooView, BarView>>` — the pane's view may be `replace_view`d between the declared members at runtime. Outside the union = compile error at the call site.
- [ ] **View-type attr references (R17):** view attrs may accept typed handle references — e.g. `pi logs=@logs subagents=@subagents`. The view's facet SHAPE declares the expected handle class and view-type bound. Mismatch = `error[scene/view-type-mismatch]` at `ark scene check`.

### R7: Op vocabulary

**Description:** Canonical op set. Each op maps to a named intent. Extensions register additional namespaced intents.

**Acceptance Criteria:**

**Pane/tab/stack ops (polymorphic via typed handles):**
- [ ] `focus @handle` — tab, pane, or stack child (compiler resolves from handle type).
- [ ] `close @handle` — tab or pane.
- [ ] `rename @handle to="name"` — tab only.
- [ ] `resize @handle direction=<dir> by=<inc|dec>` — pane or stack.
- [ ] `move @handle to=<anchor>` — pane only.
- [ ] `pin @handle` / `unpin @handle` — overlay pane only.
- [ ] `clear @stack` — close every dynamic child of the stack. Static children declared in scene are preserved. Stack-handle only.

**Spawn ops:**
- [ ] `spawn @handle { <view> }` — create tiled pane.
- [ ] `spawn @handle overlay pos=<pos> size=<WxH> { <view> }` — create overlay pane.
- [ ] `spawn_into @stack { <view> attrs… }` — spawn a dynamic child into the stack. The declared child constraint (R6) validates at compile; inserting a view outside the stack's declared type = `error[scene/view-type-mismatch]`.
- [ ] `new_tab @handle name="name" cwd="path"` — create tab.

**Mode ops:**
- [ ] `use_mode "name"` — switch active tab to named mode layout.
- [ ] `use_mode "default"` — revert to primary layout.

**Messaging ops:**
- [ ] `pipe from=@handle to=@handle payload="…"` — multi-target, both panes.
- [ ] `emit "<event-name>" { <kv payload> }` — emit UserEvent.
- [ ] `set_status text="…" severity=<level> ttl_ms=<int>` — global, status bar extension.

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

**Description:** Everything is an extension. No shipped-vs-user distinction in format or API. Extensions provide views, intents, events, and supervisor-side lifecycle hooks.

> **SUPERSEDED (2026-04-20) for the `wasm` delivery mode** by `cavekit-plugin-protocol.md` (the new `wasm-component` mode replaces the old `wasm` mode entirely). The `compiled-in` and `subprocess` delivery modes are unchanged. Wasm extensions are now ark-native components loaded by ark-host's own wasmtime runtime, not zellij; manifest format moves from `ark.metadata` (KDL via facet-kdl) to `ark-meta:v1` + `ark-caps:v1` postcard custom sections (no parser in guest); capability gating moves from runtime modal to install-time refusal against user grants in `ark.kdl`. See plugin-protocol R1-R14.

**Acceptance Criteria:**

**Format parity:**
- [ ] Identical manifest format for compiled-in, subprocess, and wasm-component extensions (the wasm-component manifest is the `ark-meta:v1` postcard payload per plugin-protocol R9, conceptually equivalent to the other modes' manifests).
- [ ] For compiled-in Rust extensions: manifest code-generated via `#[derive(Extension)]` + facet SHAPE. Zero manifest file.
- [ ] For subprocess extensions: hand-written `extension.kdl` alongside binary.
- [ ] For wasm-component extensions: manifest emitted at build time via `#[derive(ArkPlugin)]` + `#[derive(Plugin)]` into `ark-caps:v1` + `ark-meta:v1` custom sections (no facet-kdl in guest). Per plugin-protocol R3 + R9.

**Delivery modes (3 for v1):**
- [ ] `compiled-in` — Rust crate in workspace. In-process trait dispatch. Registered via `inventory`/`linkme` at boot.
- [ ] `subprocess` — any-language binary. NDJSON JSON-RPC over unix socket. Ark spawns protocol handler; view process runs in pane separately.
- [ ] `wasm-component` — wasm component-model component. Loaded by ark-host's own wasmtime runtime (NOT zellij). Capability-gated via `ark:cap/*` imports. Per `cavekit-plugin-protocol.md` R1-R14.

**Resolution (no central registry file):**
- [ ] `use "<name>"` resolves by scanning (project-local wins, XDG convention):
  1. Project-local: `.ark/extensions/<name>/`.
  2. User-installed: `~/.local/share/ark/extensions/<name>/`.
  3. System directories.
  4. Compiled-in registry (auto-registered at boot).
- [ ] Missing extension = `error[ext/missing]` with Levenshtein suggestions.

**Activation:**
- [ ] Lazy. Extension loaded only when scene `use`s it.

**Agent as extension capability:**
- [ ] No `agent { }` scene-root block. No privileged agent protocol.
- [ ] Extensions integrate engines via supervisor-side hooks (see cavekit-soul.md Phase 2): `on_session_start`, `control_verbs`, `register_intents`, scene-registered views.
- [ ] Scene activates extensions via `use "<name>"`. Each ext sets up its own engine protocol handling in `on_session_start` (e.g. pi-core opens its bridge socket per cavekit-pi.md R2).

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
- [ ] Include path sandboxing: include targets MUST resolve within the scene file's directory tree. Absolute paths and `..`-escaping paths rejected with `error[scene/include-escape]`. `ext:` includes exempt (resolved separately).

### R12: Diagnostics

**Description:** Every compile-time and runtime error surfaces via `miette`.

**Acceptance Criteria:**
- [ ] Error codes namespaced: `scene/*`, `ext/*`, `op/*`. Rhai errors nest under `scene/rhai-*` (`scene/rhai-scope-mismatch`, `scene/rhai-eval`, `scene/rhai-oom`).
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
- [ ] `ark doctor` — diagnostics (verify default scene, loaded extensions; per-ext preflight checks fan in per cavekit-soul.md Phase 2).
- [ ] No `ark spawn`. Bare `ark` = default session (matches zellij idiom).

### R14: Hot reload

**Description:** Scene file changes re-parsed and reconciled without restarting the session.

**Acceptance Criteria:**
- [ ] `reload_scene` op + `ark scene reload --session <name>` CLI both enter reconcile path.
- [ ] **Turn-inflight gate:** extensions may register a reload-gate via Phase 2 hooks (a pending engine turn blocks scene reload until the ext reports quiescent). Bare sessions + sessions with no gate-registering ext always reload immediately.
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
- [ ] Engine-lifecycle methods are extension-private; each extension speaks whatever protocol fits its engine (e.g. pi-core's unix-socket bridge per cavekit-pi.md R2). No privileged agent protocol in the scene layer.
- [ ] Per-call timeout 5s default; extensions extend via `$/progress` heartbeats.
- [ ] Supervision: subprocess shutdown sequence stdin-close → 2s → SIGTERM → SIGKILL. Crash → `error[ext/crashed]` event.

### R17: Rust DX — code-generated manifest + typed handles

**Description:** Extension authors in Rust declare everything via derives + trait impls. Zero manifest files. Pane and stack handles are parametric on their declared view type (`Pane<V>`, `Stack<V>`); the derive macro + scene compiler co-operate to produce compile-time-checked intent signatures.

**Acceptance Criteria:**

**Derives:**
- [ ] One crate = one extension. All derives in the same crate auto-group via `module_path!()`.
- [ ] `#[derive(Facet, Extension)] #[extension(name = "…")]` — extension identity + config schema.
- [ ] `#[derive(Facet, View)]` — view config schema. Exactly one view per pane.
- [ ] `#[derive(Facet, Event)]` — event payload schema. Event name auto-derived from struct name (snake_case).
- [ ] `#[ark::intent]` on methods:
  - On `impl ExtensionStruct` → global intent (no pane target).
  - On `impl ViewStruct` → targeted intent (pane handle required in scene).

**Handle kinds (structural — no view-type info):**
- [ ] `HandleKind = { Tab, Pane, Stack }`. Retired: `Command`, `Plugin` (these were view render modes, not reference kinds — they move onto the view's trait impl, below).

**Typed handles (parametric on view type):**
- [ ] `struct TabHandle(Handle)` — tab identity; non-parametric.
- [ ] `struct Pane<V: View>` — pane whose content is a `V`. Wraps a `Handle` + `PhantomData<V>`.
- [ ] `struct Stack<V: View>` — container of `Pane<V>` children (compiles to zellij pane stack; one child expanded at a time per zellij default). `Stack<dyn View>` for heterogeneous. Affords: `spawn_pane(attrs) -> Pane<V>`, `close_child(&Pane<V>)`, `children() -> Vec<Pane<V>>`, `clear()`.
- [ ] Common trait: `trait PaneLike { fn handle(&self) -> &Handle; fn emit<E: Event>(&self, e: E); }` implemented by every typed handle.

**View marker traits (gate affordances):**
- [ ] `trait View` — base marker; every view impls it.
- [ ] `trait CommandView: View` — subprocess-rendered views. `impl<V: CommandView> Pane<V> { fn env() -> String; fn write_stdin(&self, bytes: &[u8]); fn pid(&self) -> Option<u32>; }`.
- [ ] `trait ZellijView: View` — wasm-plugin-rendered views. `impl<V: ZellijView> Pane<V> { fn pipe(&self, msg: Value); }`.
- [ ] Extensions may define additional view traits for capability groups (e.g. `trait DiffView: View`) and gate intents on them.

**Typed intent signatures:**
- [ ] `#[ark::intent]` methods on `impl ViewStruct` receive `&Pane<Self>` as the target argument by default. Example: `impl PiView { #[ark::intent] fn focus_subagent(&self, pane: &Pane<Self>, id: &str) { … } }`.
- [ ] Intents may declare additional typed handles from scene as args: `fn scroll_to_hunk(&self, pane: &Pane<DiffView>, hunk: usize)`. Compiler validates the `@handle` passed in the scene resolves to `Pane<DiffView>`; otherwise `error[scene/view-type-mismatch]`.
- [ ] Typed handles flow through the registry via code-generated stubs: the `#[ark::intent]` macro emits a type-erased `dyn Intent` adapter that re-materialises `Pane<V>` from the CompiledScene's view-table at dispatch entry, then calls the typed method. `IntentRegistry` stays object-safe and keyed on `(op-name, Handle)`.
- [ ] View attrs that take handle refs (e.g. `pi logs=@logs subagents=@subagents`) declare their expected typed-handle class in the facet SHAPE. Scene compiler validates each attr against the referenced handle's declared view type.

**Events:**
- [ ] Events emitted via `ctx.emit(E)` (extension-scoped) or `pane.emit(E)` (view-scoped + source handle). Auto-namespaced by extension name.
- [ ] Extensions can only emit own events. Open subscription to any event. Scene-mediated cross-extension wiring.
- [ ] Extension dependencies: normal crate deps. Import event/intent types from other extension crates for type-safe subscription.

**Config:**
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
6. Extensions receive `on_session_start` (soul Phase 2); engine-speaking exts (e.g. pi-core) bring up their own protocol wiring at this point.

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
- **`wasm-component` delivery (WASI p2 sandbox)** — deferred.
- **Auto-merge sidecar fragments** — opt-in `include` only.
- **Reactive state** — modes cover 80%.
- **Swap-tiled-layout exposure** — modes supersede.
- **Multi-pane overlay containers** — single-pane overlay attr for v1.
- **`agent { }` scene-root block** — removed. Agent = extension.
- **`plugin { }` keyword** — removed. Views replace.
- **Minijinja** — removed. Rhai everywhere.
- **CEL** — removed. Rhai expression-only mode replaces it (2026-04-16 revision; see changelog).
- **`ark spawn` verb** — removed. Bare `ark` = default session.
- **ACP (`acp.*` ops, ACP extension capability)** — removed 2026-04-18. Engines integrate as extensions via the ext-hook surface; ACP is not privileged in ark.
- **Cavekit orchestrator** — removed 2026-04-18. No extension rehoming; gone outright.
- **Claude Code ark-side crates** — removed 2026-04-18 but **rehomed same day** into `extensions/claude-code/` (hook-based integration, not ACP). See `cavekit-claude-code.md`.

## Cross-References

- cavekit-soul.md — supersedes cavekit-architecture.md; phase plan for the v0.1 migration.
- cavekit-mux-zellij.md — zellij subprocess integration, layout rendering.
- cavekit-types-state-events.md — SessionSpec/SessionStatus + `CoreEvent` after soul Phase 1; R4 selectors match on kind + fields.
- cavekit-supervisor.md — scene compilation + event bus registration.
- cavekit-config.md — figment-layered config for scene path defaults.
- cavekit-cli.md — `ark scene` + `ark ext` subcommand trees.
- cavekit-claude-code.md — claude-code extension (v0.1's first consumer of the ext-hook surface + typed handles + stacks).
- cavekit-pi.md — pi extension family (DEFERRED v0.2; second consumer pattern).

## Design Decisions Locked

| Decision | Value | Why |
|---|---|---|
| KDL version | 2.0 | Stable, future-proof |
| Parse stack | `facet` + `facet-kdl` + `kdl` 6.5 (formatter) + `miette` | Active sponsorship; one derive covers parse/reflect/schema/LSP-hover |
| Expression language | Rhai expression-only mode (`rhai` crate). CEL + Minijinja dead. | One language for `when=` + `{expr}` interpolation; Rust-native syntax; `Engine::new_raw` + symbol disables guarantee non-TC |
| View noun | "view" replaces "plugin" / "provider" | What fills a pane |
| View rendering | Trait-based: `CommandView` / `ZellijView`; affordances gated by trait impl on `Pane<V>` | Type system determines render mode + exposed methods |
| Typed pane handles | `Pane<V: View>` (generic over declared view); `Stack<V>` for dynamic containers | View-type validation at compile time; replaces runtime `HandleKind::{Command, Plugin}` |
| `HandleKind` | `{ Tab, Pane, Stack }` — structural only | View-type info lives on the typed wrapper, not the kind |
| Pane handles | `@handle` required on all tabs + panes + stacks | Reconciler identity keys; compile-time validation |
| Stacks | `stack @h { <view> }` layout primitive (= zellij pane stack); dynamic children spawned via `spawn_into` op or `StackHandle::spawn_pane` | Lets views act as coordinators with typed sinks; avoids handle-interpolation (`@sub-{id}`) patterns; reuses zellij's native stack UX |
| Handle namespace | Flat, scene-scoped | Simple; clash = compile error |
| Layout vocabulary | `row`/`col`/`span`/`cells`/`overlay` (ark-native) | Zellij is render backend, not vocabulary source |
| Sizing | Spans (relative weight) + cells (fixed) | Compose cleanly; no percentage arithmetic |
| Reconciler | `override-layout` + env `ARK_HANDLE` wrapper | Zellij does reconciliation; ark renders desired state |
| Modes | Named alternate layouts via override-layout | Explicit, not pane-count-triggered |
| Conditional | `when=` on tabs/panes → reconciler creates/removes | K8s desired-state model |
| Extensions | Everything is an extension; no shipped-vs-user distinction | Format parity (VSCode/HA/K8s pattern) |
| Agent protocol | None privileged. Each engine-speaking extension owns its protocol (e.g. pi-core's unix-socket bridge). | Ark is a terminal IDE; engines are ordinary extensions |
| Delivery modes (v1) | compiled-in / subprocess / zellij-wasm | 3 modes; WASI p2 deferred |
| Manifest DX | Code-generated from derives + trait impls; one-crate-per-extension; zero annotation | Cleanest Rust DX |
| Composition | `include` only; no `extends` | Flat-first; Docker Compose lesson |
| Scene fragments | Opt-in via `include "ext:name/fragment"` | No implicit behavior injection |
| Config ownership | Extension owns schema+defaults; scene owns values; ark validates | Clean three-way split (VSCode/K8s pattern) |
| CLI entry | Bare `ark` = default session | Match zellij idiom; no `spawn` verb |
| Default scene | Embedded asset; shell + status bar; no agent | Minimum viable terminal IDE baseline |
| Keybind notation | Zellij-native quoted strings (`"Alt d"`) | Direct pass-through; fewer bugs |
| Event emission | Own-namespace only; open subscription | Namespace integrity; trace-friendly |
| Cross-ext wiring | Scene-mediated (events → scene → intents) | Extensions stay decoupled |
| Naming | Scene (theatrical); Extension (VSCode/Zed convention); View (what fills a pane) | Industry-aligned |

## Changelog

### 2026-04-18 — Typed view-parametric handles + `stack` primitive; scope cut (ACP + cavekit orchestrator removed; claude-code rehomed to extension)

- **Affected:** R3 (Layout DSL — adds `stack`), R6 (Views — view alias becomes type declaration; union syntax), R7 (Op vocabulary — adds `spawn_into`, `clear`), R17 (Rust DX — `CommandPane`/`PluginPane` retire, replaced with parametric `Pane<V>`, `Stack<V>`; `HandleKind` narrows), Design Decisions table. **Out-of-scope:** `stack` un-deferred; ACP ops (`acp.*`) deleted entirely.
- **Summary (typed handles):** `HandleKind::{Command, Plugin}` conflated reference kind with runtime render mode. Split: `HandleKind` narrows to `{Tab, Pane, Stack}` (structural); render-mode + affordances move to marker traits on the view (`CommandView`, `ZellijView`). Typed handles become parametric (`Pane<V: View>`, `Stack<V: View>`). View aliases inside `pane`/`stack` braces now *declare* the view type; scene compiler validates handle-typed view attrs (e.g. `pi logs=@logs`) against the referenced handle's declared view. Ext intents bind precisely — `fn scroll(pane: &Pane<DiffView>)` is a compile check. `#[ark::intent]` macro stays erased at the registry boundary by re-materialising typed handles from the CompiledScene view-table.
- **Summary (stack):** New `stack @h { … }` layout primitive = zellij pane stack (one child expanded, rest collapsed). Un-defers the `stack` out-of-scope note. Supports dynamic containers — essential for views that coordinate fan-out (pi-core's subagent stack). Un-deferring is the right call because pi is the first dynamic-fanout consumer and `row`/`col` can't model variable-membership without ugly splits.
- **Summary (scope cut):** ACP + Cavekit orchestrator deleted from ark as part of soul Phases 3 + 4. Extension-owned ACP ops (`acp.*`) gone. Claude Code ark-side crates also deleted but **rehomed same day** into `extensions/claude-code/` as v0.1's first engine integration (hook-based, not ACP). Pi deferred to v0.2. `cavekit-engine-claude-code.md`, `cavekit-orchestrator-*.md`, `cavekit-hook-ipc.md` kit files deleted — superseded by consolidated `cavekit-claude-code.md`. See cavekit-soul.md Phase 4 + cavekit-overview.md milestones + context/plans/handoff-2026-04-18-claude-code-first-pivot.md.
- **Driven by:** pi-extension planning session (cavekit-pi.md DRAFT); scope cut 2026-04-18 focused v0.1 on a single engine integration.
- **Commits:** design-only; no code commits (pre-implementation).

### 2026-04-16 — Rhai expression-only mode (supersedes CEL)

- **Affected:** R3 (when= attr), R4 (reactions), R7 (op interpolation), R8 (expression language — rewritten), R12 (error codes).
- **Summary:** Swapped expression engine from CEL (`cel-interpreter`) to Rhai (`rhai` crate) in expression-only mode (`Engine::new_raw` + symbol disables for `fn`/loops/assignment = non-TC). Rationale: better Rust-native syntax (method-chain form `xs.len()` vs `size(xs)`; `if/else` expression replaces `?:`), more active crate, built-in resource limits (`set_max_operations`, memory caps), ergonomic custom-fn registration. Cost: Rhai strings are double-quoted — predicates containing string literals require KDL raw strings (`when=#"agent.phase == "review""#`); `ark scene fmt` auto-promotes plain → raw when body contains `"`. Single-quoted strings unsupported (Rhai uses `'x'` for `char`); raw-string rule is the mitigation.
- **Commits:** design-only; no code commits (pre-implementation)

### 2026-04-16 — v3 Convergence

- **Affected:** R1–R17 (all), plus 3 new requirements (R6 Views, R9 Reconciler, R17 Rust DX)
- **Summary:** Two design sessions converged into a full architectural revision. Major changes: own layout DSL (R3 rewrite), views replace plugins (R6 new), CEL-only interpolation (R8-R9 rewrite), extensions unified with agent-as-capability (R10/R17 rewrite), reconciler via override-layout (R9 new), composition via include-only (R11 rewrite), CLI redesign (R13). See `context/impl/impl-scene-architecture-v3.md` for the full design rationale.
- **Commits:** design-only; no code commits (pre-implementation)
