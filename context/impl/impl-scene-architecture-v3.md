---
created: "2026-04-16"
status: "converged — all decisions locked across two design sessions, ready for cavekit revision"
supersedes:
  - context/impl/impl-scene-dx-v2.md (D1–D7 + Q1–Q6 — subsumed and extended here)
  - memory/project_scene_plugin_rethink.md (paused rethink — fully resolved here)
related:
  - context/kits/cavekit-scene.md (R1–R17 — this doc overturns R3, R6, R9, R10, R17 and adds new requirements)
---

# Scene Architecture v3 — Converged Design

Two design sessions produced this document. Every decision below is locked unless marked otherwise. A fresh agent should be able to pick this up cold and revise `cavekit-scene.md` to encode these decisions.

## Foundational principles

1. **Ark is a terminal IDE, not an AI terminal.** AI (ACP) is one extension capability among many. No privileged `agent { }` block.
2. **Scene KDL = desired state.** Ark reconciles zellij toward it (Kubernetes model).
3. **Zellij is a rendering backend, not a vocabulary source.** Ark owns the DSL; zellij is a compile target.
4. **Extensions are the universal abstraction.** Views, intents, events, agents — all provided by extensions. No shipped-vs-user distinction in format or API.
5. **Composition over inheritance.** `include` only; no `extends`. Flat-first.
6. **One expression language.** CEL everywhere. Minijinja is dead.

---

## CLI

```
ark                              # launch default session
ark --scene <name-or-path>       # named scene
ark --session <name>             # attach-or-create named session

ark scene check|fmt|graph|dry-run|reload|schema-dump
ark ext add|list|info|inspect|remove|update
ark doctor
```

No `ark spawn`. Bare `ark` = default session (matches zellij idiom).

## Default scene

Embedded in ark binary as asset. User overrides via `~/.config/ark/scenes/default.kdl`.

```kdl
scene "default" {
    use "status"

    layout {
        tab @main cwd="{cwd}" {
            col {
                pane @shell { shell }
                pane @status cells=1 { status }
            }
        }
    }
}
```

One tab, one shell pane, status bar. No agent. No reactions. Pure terminal IDE baseline.

---

## Scene DSL

### Layout structure

- `layout { }` body contains ONLY `tab @handle { ... }` nodes. No bare panes/rows/cols at root.
- Tabs always require `@handle`. Panes always require `@handle`. Handles are reconciler identity keys.
- Rows/cols (`row { }`, `col { }`) are structural containers — no handles, not targetable.
- Flat handle namespace, scene-scoped. Tab + pane handles share one namespace. Collision = compile error.

```kdl
scene "dev" {
    layout {
        tab @work cwd="{cwd}" focus=true {
            row {
                pane @editor span=3 { nvim }
                col span=2 {
                    pane @diff span=1 { diff cwd="{cwd}" }
                    pane @shell span=1 { shell }
                }
            }
        }
        tab @logs cwd="{cwd}" {
            pane @ci { command cmd="cargo" args=["watch","-x","test"] }
        }
    }
}
```

### Tab attributes

| Attr | Type | Description |
|---|---|---|
| `cwd` | string (CEL) | Working directory; inherited by panes |
| `name` | string | Display name in tab bar (defaults to handle) |
| `focus` | bool | Initial focus; exactly one per layout |
| `when` | string (CEL) | Conditional existence (see reconciler) |

### Sizing — spans

- `span=N` — relative weight within container. Siblings normalize to 100%.
- `cells=N` — fixed N cells (status bar: `cells=1`).
- `min=N` / `max=N` — bounds in cells.
- Compiles to zellij `size="N%"` or `size=N`.

### Overlays (floating panes)

Overlay is an attribute on `pane`, not a separate block. Tab-scoped (matches zellij).

```kdl
pane @preview overlay pos=top-right size=60x20 sticky=true {
    glow file="README.md"
}
```

- `pos=top-right|top-left|bottom-right|bottom-left|center|X%xY%`
- `size=WxH` (cells) or `W%xH%`
- `sticky=true` → survives tab switch (zellij `pinned=true`)

### Modes

Named alternate whole-tab layouts. Switch via `use_mode "name"`.

```kdl
mode "review" {
    tab @work {
        row {
            pane @editor span=1
            pane @diff span=1
        }
    }
}
```

- Compiles to: render mode KDL → `zellij action override-layout --retain-existing-terminal-panes --retain-existing-plugin-panes --apply-only-to-active-tab`.
- Handles survive swap. `@editor` subprocess preserved; tree restructured around it.
- Modes DO NOT use zellij's swap_tiled_layout (pane-count-triggered). Ark modes are explicit.

### Conditional rendering (when=)

`when="<CEL>"` on tabs and panes. Condition false at eval = node omitted from rendered layout. Condition true = node included.

Reconciler (see below) handles transitions: re-renders desired layout → override-layout. Zellij converges.

```kdl
pane @plan when="{agent.phase == 'planning'}" {
    glow file="{cwd}/.ark/plan.md"
}
tab @tests when="{agent.phase == 'testing'}" {
    pane @runner { command cmd="cargo" args=["nextest","run"] }
}
```

---

## Reconciler — override-layout

Scene KDL = desired state. Ark reconciles zellij toward it.

### Mechanism

```
CEL context changes (event fires, agent state updates)
    ↓
re-evaluate all when= predicates
    ↓
render COMPLETE desired layout KDL
(include/exclude panes+tabs based on when= truth values)
    ↓
zellij action override-layout <path>
    --retain-existing-terminal-panes
    --retain-existing-plugin-panes
    ↓
zellij reconciles internally
```

### Pane identity via env wrapper

Every pane command wrapped with `env ARK_HANDLE=@name <cmd>`. Makes commands unique even when two panes run the same binary (e.g., two shell panes). Zellij's override-layout matches by `invoked_with()` (command + args) — different `ARK_HANDLE` = different match = correct pane retained.

### Reconciliation triggers

1. `when=` predicate input changes → re-eval → override-layout
2. Scene file edit + save → re-read → override-layout
3. `use_mode` op → render mode layout → override-layout

### Debounce

200ms debounce on context changes. Same as file-watcher.

### Drift tolerance

User-initiated changes (manually close pane, add tab) are tolerated. Reconciler only forces convergence on `when=` transitions and mode switches.

---

## Views

A **view** is what fills a pane. Every pane has exactly one view — a child node inside `pane @handle { ... }`.

```kdl
pane @editor { nvim file="x.rs" }     // nvim view
pane @shell  { shell }                 // shell view (primitive)
pane @diff   { diff cwd="{cwd}" }     // diff view
pane @build  { command cmd="cargo" args=["watch"] }  // command view (primitive)
```

### Three tiers

| Tier | Examples | Source |
|---|---|---|
| Primitives | `command`, `shell`, `edit` | Kernel-builtin; map 1:1 to zellij native types |
| Shipped | `diff`, `status`, `picker` | Compiled-in extensions bundled with ark |
| User-installed | `nvim`, `glow`, `lazygit` | `ark ext add`; resolved from extension dir |

Same namespace. Same scene syntax. User doesn't see the tier.

### View rendering — determined by trait impl

| View struct implements | Pane renders as |
|---|---|---|
| `CommandView` | Zellij runs a command binary |
| `ZellijView` | Zellij loads a wasm plugin |
| Neither | No pane rendering (data-only view) |

---

## Intents

A named, callable action. Registered by extensions. Called from scene reactions and keybinds.

### Two scopes — determined by impl location

| Intent on | Scope | Scene syntax |
|---|---|---|
| `impl Nvim` (Extension struct) | Global — no pane target | `nvim.reload_configs` |
| `impl NvimView` (View struct) | Targeted — needs `@handle` | `nvim.save @editor` |

### Typed pane handles

| View trait | Intent receives | Methods available |
|---|---|---|
| `CommandView` | `&CommandPane` | `.env()`, `.write_stdin()`, `.pid()` |
| `ZellijView` | `&PluginPane` | `.pipe()` |
| Both | `.emit()`, `.handle()` | Common across all |

### Handle targeting rules

- Intent schema declares target type (Global / Tab / Pane / Either).
- Compiler validates at `ark scene check`: handle type must match.
- Polymorphic ops: `focus @x` / `close @x` accept tab or pane — compiler resolves from declaration.
- `set_status` → global intent on status extension (singleton view, no handle).

### ACP intents — sub-namespaced

ACP ops live under `acp.*` sub-namespace: `acp.prompt`, `acp.cancel`, `acp.permit`, `acp.set_mode`. All global (session-scoped). Provided by whichever extension speaks ACP.

---

## Events

Emitted by extensions. Scene reactions subscribe.

### Emission — own namespace only

```rust
ctx.emit(BufferSaved { path: "x.rs".into() });  // → "nvim.buffer_saved"
pane.emit(BufferSaved { path: ... });            // → "nvim.buffer_saved" + source=@handle
```

`ctx.emit()` / `pane.emit()` auto-prefix with extension name. Extensions cannot emit other extensions' events.

### Subscription — open

Any extension or scene can subscribe to any event:

```kdl
on nvim.buffer_saved { git_ui.refresh @diff }
on ark.acp.turn_started { set_status text="thinking…" }
```

### Cross-extension wiring — scene-mediated

Extensions don't call each other's intents directly. Scene wires events → intents. Extensions stay decoupled.

---

## Reactions

Event → op dispatch. Field-pattern selectors + `when=` predicate + op list.

```kdl
on FileEdited path="**/*.md" {
    spawn @preview overlay pos=top-right size=60x20 {
        glow file="{event.path}"
    }
}
```

### Selector — field patterns as KDL props

```kdl
on FileEdited path="**/*.md" { ... }
on ark.acp.tool_call tool=edit_file { ... }
on ark.acp.permission_requested tool=write_file when="!starts_with(payload.input.path, cwd)" { ... }
```

- Field names from event variant fields (facet SHAPE validates).
- Default match: glob for path-like, exact for strings. Override via `(glob)`, `(exact)`, `(regex)`.
- `when="<CEL>"` — bare CEL expression, no braces. Same context as op args.

### Selector-captured locals

Selector field patterns bind as locals in the op body:

```kdl
on ark.acp.tool_call tool=edit_file {
    set_status text="editing: {tool}"                 // {tool} = captured local
    nvim.open_at_line @editor
        path="{payload.input.file_path}"              // not captured → full CEL path
}
```

### UserEvent payload — hybrid access

For UserEvent, bare field names route into payload. Reserved top-level keys: `name`, `source`, `payload`.

```kdl
on ark.acp.tool_call tool=Bash { ... }           // bare → payload.tool
on ark.acp.tool_call payload.tool=Bash { ... }   // explicit → same thing
```

### `when=` on ops

`when=` legal on any op node (not just reaction root). Per-op guards:

```kdl
on FileEdited {
    focus @diff when="{size(event.changes) > 10}"
    set_status text="small edit" when="{size(event.changes) <= 10}"
}
```

---

## Keybinds

```kdl
bind "Alt d"       { focus @diff }
bind "Alt Shift v" { use_mode "review" }
bind "Ctrl c"      { acp.cancel }
```

- Quoted strings with space-separated modifiers (matches zellij notation).
- Same op vocabulary as reactions.
- Last-wins per chord across scene + includes.

---

## Interpolation

`{...}` = CEL expression. Always. No minijinja anywhere.

### Two scopes

| Scope | Bindings | Where used |
|---|---|---|
| Spawn context | `cwd`, `id`, `name`, env vars | `layout { }` values |
| Event context | `event`, `payload`, `agent`, `session` + captured locals | `on { }` / `bind { }` op args, `when=` predicates |

### Rules

- `{expr}` in string → evaluate CEL, substitute.
- `when="expr"` → bare CEL, no braces (it's always CEL).
- Zero holes → verbatim string, no CEL.
- Single-hole whole-value (`"{expr}"`) → typed pass-through.
- Mixed (`"text {expr} more"`) → coerce to string, concat.

---

## Extensions

### Core principle

Everything is an extension. No shipped-vs-user distinction in format or API. Differences are ONLY delivery mode and install source.

### Agent is an extension

ACP is an extension capability. No `agent { }` scene-root block. Scene activates via `use "claude-code"`. Extension declares `capabilities { agent { speaks "acp" } }` + launch spec.

### No central registry file

Resolution by scanning:
1. Compiled-in (auto-registered at boot via `inventory`)
2. User-installed (`~/.local/share/ark/extensions/<name>/`)
3. Project-local (`.ark/extensions/<name>/`)

### Activation

Lazy. Loaded when scene `use`s it. No always-on cost.

### Delivery modes (3 for v1)

| Mode | Runtime | Protocol | Rendering |
|---|---|---|---|
| `compiled-in` | Rust in-process | Direct trait calls | Pseudo-PTY or ZellijView |
| `subprocess` | Any language binary | NDJSON-RPC over unix socket | CommandView (process in pane) |
| `wasm` | Zellij plugin runtime | Pipe via ark-bus | ZellijView (wasm in pane) |

`wasm-component` (WASI p2 ark-hosted sandbox) deferred to v0.3+.

### Extension can have both protocol + views

One extension, one `use`. Two runtime components:

```
extension "nvim"
├── protocol: subprocess (ark-ext-nvim binary)
└── view: CommandView (runs `nvim` binary in pane)
```

Ark starts protocol handler. When pane mounts view, ark runs the view command. Protocol handler connects to the view process via app-native RPC (nvim socket, etc.).

### Scene fragments — opt-in include

Extensions ship scene fragments. NOT auto-merged on `use`. User opts in:

```kdl
use "nvim"
include "ext:nvim/defaults"
include "ext:nvim/acp-integration"
```

`ark ext info <name>` lists available fragments.

---

## Manifest — code-generated (Rust DX)

One crate = one extension. Zero annotation. Derives + trait impls = full manifest.

```rust
// crates/ext-nvim/src/lib.rs

#[derive(Facet, Extension)]
#[extension(name = "nvim")]
pub struct Nvim {
    #[facet(default = "~/.config/nvim/init.lua")]
    pub init_file: String,
    #[facet(default = true)]
    pub plugins: bool,
}

// Global intents — methods on Extension struct
impl Nvim {
    #[ark::intent]
    pub fn reload_configs(&self) -> Result<()> { ... }
}

// View — what pane @x { nvim file="x.rs" } accepts
#[derive(Facet, View)]
pub struct NvimView {
    pub file: Option<String>,
    pub line: Option<usize>,
}

// View rendering — trait impl determines how pane renders
impl CommandView for NvimView {
    fn command(&self) -> Command {
        Command::new("nvim")
            .args(self.build_args())
            .env("NVIM_LISTEN_ADDRESS", self.socket_path())
    }
}

// Targeted intents — methods on View struct, typed pane handle
impl NvimView {
    #[ark::intent]
    pub fn open_at_line(&self, pane: &CommandPane, path: String, line: usize) -> Result<()> {
        let nvim = NvimRpc::connect(pane.env("NVIM_LISTEN_ADDRESS")?)?;
        nvim.command(&format!(":e +{line} {path}"))?;
        Ok(())
    }

    #[ark::intent]
    pub fn save(&self, pane: &CommandPane) -> Result<()> {
        let nvim = NvimRpc::connect(pane.env("NVIM_LISTEN_ADDRESS")?)?;
        nvim.command(":w")?;
        pane.emit(BufferSaved { path: ... });
        Ok(())
    }
}

// Events — just structs, auto-namespaced by crate
#[derive(Facet, Event)]
pub struct BufferSaved { pub path: String }
```

### Auto-collection

- `#[derive(Extension)]` → registers ExtensionMeta via `inventory`
- `#[derive(View)]` → registers ViewMeta (auto-grouped by `module_path!()`)
- `#[derive(Event)]` → registers EventMeta (auto-grouped)
- `#[ark::intent]` → extracts param schema via facet SHAPE, registers IntentMeta
- `CommandView` / `ZellijView` trait impl → determines render mode

Zero manual listing. Types = manifest.

### Non-Rust extensions

Subprocess extensions ship `extension.kdl` manifest alongside binary (hand-written or build-tool generated). Same schema, different source.

---

## Config ownership

- **Extension** owns: schema + defaults (struct fields + `#[facet(default)]`)
- **Scene author** owns: values (`use "nvim" config { init-file "~/custom.lua" }`)
- **Ark** owns: validation (at `ark scene check`, against facet schema)
- Extension receives validated config at activation

---

## Composition

- `use "<ext>"` — activates extension API (views, intents, events)
- `include "<path-or-ext:fragment>"` — splices fragment verbatim (copy-paste semantics)
- **No `extends`.** Flat-first. Composition via `include`.

Conflicts in included fragments = compile error (loud, Nix-style).

---

## Ops vocabulary

Core ops registered by ark-core extension. All in `ark.core.*` namespace.

### Pane/tab ops (polymorphic via typed handles)

```
focus @handle              # tab or pane
close @handle              # tab or pane
rename @handle to="name"   # tab only
resize @handle direction=right by=inc   # pane only
move @handle to=<anchor>   # pane
pin @handle / unpin @handle             # overlay pane
```

### Spawn ops

```
spawn @handle { <view> }                           # tiled pane
spawn @handle overlay pos=top-right size=WxH { <view> }  # overlay
new_tab @handle name="name" cwd="path" { <layout> }      # new tab
```

### Mode ops

```
use_mode "name"            # switch active tab to named mode layout
use_mode "default"         # revert to primary layout
```

### Messaging ops

```
pipe from=@handle to=@handle payload="..."
emit "user.my_event" { key "value" }
set_status text="..." severity=info ttl_ms=2000
```

### ACP ops (sub-namespaced)

```
acp.prompt text="..."
acp.cancel
acp.permit request_id="..." outcome=allow
acp.set_mode mode=plan
```

### Control ops

```
exec script="..." shell="bash" timeout_ms=5000
reload_scene
```

---

## Canonical example — complete scene

```kdl
scene "rust-dev" {
    // ── Extensions ──────────────────────────────────────────
    use "claude-code"       // ACP agent extension
    use "nvim"              // editor with intents
    use "diff"              // git diff view
    use "status"            // status bar

    // Opt-in to extension-shipped fragments
    include "ext:nvim/acp-integration"
    include "ext:claude-code/permission-defaults"

    // ── Layout ──────────────────────────────────────────────
    layout {
        tab @work cwd="{cwd}" focus=true {
            row {
                pane @agent span=3 { claude-code }
                col span=2 {
                    pane @diff span=1 { diff cwd="{cwd}" }
                    pane @editor span=1 { nvim }
                }
            }
        }
        tab @logs cwd="{cwd}" {
            pane @ci { command cmd="cargo" args=["watch","-x","test"] }
        }
        tab @tests when="{agent.phase == 'testing'}" {
            pane @runner { command cmd="cargo" args=["nextest","run"] }
        }
    }

    // ── Modes ───────────────────────────────────────────────
    mode "review" {
        tab @work {
            row {
                pane @agent span=1
                pane @diff span=1
            }
        }
    }

    // ── Reactions ───────────────────────────────────────────
    on ark.acp.turn_started {
        set_status text="thinking…" severity=info
    }
    on ark.acp.turn_finished {
        set_status text="ready" severity=success ttl_ms=2000
    }

    on ark.acp.permission_requested tool=read_file {
        acp.permit request_id="{request_id}" outcome=allow
    }
    on ark.acp.permission_requested tool=write_file
        when="!starts_with(payload.input.path, cwd)"
    {
        acp.permit request_id="{request_id}" outcome=reject_always
    }

    on FileEdited path="**/*.md" {
        spawn @preview overlay pos=top-right size=60x20 {
            glow file="{event.path}"
        }
    }

    on ark.acp.tool_call tool=edit_file {
        nvim.open_at_line @editor
            path="{payload.input.file_path}"
            line="{payload.input.line}"
    }

    // ── Keybinds ────────────────────────────────────────────
    bind "Alt 1"       { focus @work }
    bind "Alt 2"       { focus @logs }
    bind "Alt d"       { focus @diff }
    bind "Alt e"       { focus @editor }
    bind "Alt Shift v" { use_mode "review" }
    bind "Alt Shift n" { use_mode "default" }
    bind "Alt c"       { acp.cancel }
    bind "Alt s"       { nvim.save @editor }
}
```

---

## Zellij capability mapping

Every ark construct has a confirmed zellij compile target.

| Ark construct | Zellij |
|---|---|
| `row { }` / `col { }` | `pane split_direction="horizontal"/"vertical"` |
| `tab @h cwd=X` | `tab cwd="X"` |
| `pane @h span=N` | sibling pane, `size="N%"` (computed) |
| `pane @h cells=N` | `pane size=N` |
| `pane @h { command cmd=X }` | `pane name="h" { command "env" "ARK_HANDLE=h" "X" }` |
| `pane @h { shell }` | `pane name="h" { command "env" "ARK_HANDLE=h" "$SHELL" }` |
| `pane @h { <plugin-alias> }` | `pane { plugin location="<resolved>" }` |
| `pane @h overlay pos=P size=WxH` | `floating_panes { pane name="h" x y width height }` |
| `when="<CEL>"` | dead-branch elim + override-layout on transition |
| `mode "name"` + `use_mode` | override-layout --retain-existing-terminal-panes |
| `focus @h` | `action focus-pane-id <id>` (pane) / `action go-to-tab <idx>` (tab) |
| `close @h` | `action close-pane --pane-id <id>` / `action close-tab` |
| `spawn @h { ... }` | `action new-pane --direction <d> --pane-name h` |
| `bind "Alt x" { ops }` | keybinds block → `MessagePlugin "ark-bus"` |

---

## What the cavekit revision must change

| Cavekit section | Change |
|---|---|
| R1 (grammar) | Add: tab/pane handles required; `row`/`col` keywords; `mode` blocks; `include` replaces `extends`; drop `keybind` → `bind`; drop `plugin` keyword |
| R2 (scope) | Update for new node types (row, col, mode, overlay, when=) |
| R3 (layout) | FULL REWRITE: ark-native DSL, spans, handles, overlays, view resolution, env ARK_HANDLE wrapper |
| R4 (reactions) | Update: field-pattern KDL props, selector captures as locals, `when=` on ops |
| R5 (keybinds) | Update: `bind` keyword, zellij notation |
| R6 (plugins) | REMOVE: `plugin` keyword dead. Views replace. ZellijView trait. |
| R7 (ops) | Update: new op vocabulary, typed handle targets, `acp.*` sub-namespace |
| R8 (CEL) | Update: `when=` bare CEL; `{expr}` in strings; drop `if=` |
| R9 (templating) | REWRITE: kill minijinja; CEL in two scopes (spawn + event) |
| R10 (extensions) | REWRITE: unified format; 3 delivery modes; code-generated manifest; CommandView/ZellijView traits; opt-in fragments |
| R11 (composition) | REWRITE: drop `extends`; `include` only; flat namespace |
| R14 (hot reload) | Update: reconciler via override-layout; debounce; drift tolerance |
| R17 (ACP) | REWRITE: agent is extension capability; no `agent { }` block; `acp.*` intent namespace |
| NEW (reconciler) | New section: override-layout reconciliation, env wrapper, when= re-eval |
| NEW (views) | New section: view concept, tiers, rendering traits |
| NEW (CLI) | Bare `ark` = default; `--scene`/`--session`; drop `spawn` |

---

## Out of scope for v1

- Multi-agent (multiple ACP extensions in one scene) — works mechanically via multi-`use`, but UX/event routing design deferred
- `extends` composition — dropped entirely; revisit only if real demand
- `wasm-component` (WASI p2 sandbox) delivery — deferred to v0.3+
- `stack` (tabbed pane cluster) — deferred
- Reactive state (React-style re-render) — modes cover 80%
- Swap-tiled-layout exposure — modes supersede
- Multi-pane overlay containers — single-pane overlay attr for v1
- Auto-merge sidecar fragments — opt-in `include` only

---

## What a fresh agent should do next

1. Read this file end-to-end.
2. Read `context/kits/cavekit-scene.md` for current state being revised.
3. Revise `cavekit-scene.md` to encode all decisions above (R1–R17 updates + new sections).
4. Draft build-site migration tasks for the ~60-task scene build plan.
5. Do NOT start implementation. Cavekit revision must land and be reviewed first.
