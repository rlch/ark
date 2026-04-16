---
created: "2026-04-16"
status: "design-handoff — locked-in decisions from brainstorm, ready for cavekit revision"
related:
  - context/kits/cavekit-scene.md (R1–R15 — currently says layout body is zellij-KDL pass-through; this handoff overturns R3)
  - memory/project_scene_plugin_rethink.md (paused rethink — field-pattern selectors, plugin aliases, {var} interpolation — this handoff is consistent with and extends it)
  - memory/project_scene_system.md (pointer to cavekit + build site)
---

# Scene DX v2 — Layout DSL Redesign (Handoff)

## Why this exists

Conversation started as "do we bundle zellij / depend on zellij crates" and evolved into a full rethink of ark's layout DX. Key shift: **ark should own the layout DSL, not pass zellij KDL through with minijinja templating on top.**

This handoff captures the research, the locked design decisions, the canonical example, and the zellij-capability mapping. A fresh agent should be able to pick this up cold and either:
1. Revise `cavekit-scene.md` (particularly R3 "Layout compilation" and R1 "Scene file grammar") to encode these decisions.
2. Draft a migration plan for the 6 shipped layouts in `crates/mux/zellij/layouts/` and their scene ports in `crates/mux/zellij/scenes/`.

## Research findings

### 1. Zellij as a library — NOT viable

Cloned `zellij-org/zellij` at `/Users/rjm/Coding/Personal/zellij` (v0.45.0). Investigated three tiers:

**`zellij-utils` crate** (published, used by zellij-client/server internally):
- Real modules: `kdl` (layout/config parser), `input::layout` (typed `Layout`/`TiledPaneLayout`/`FloatingPaneLayout`/`Run`/`RunPlugin`), `ipc` (`ClientToServerMsg`/`ServerToClientMsg`), `data` (pane/tab info), `consts` (socket paths).
- Only 2 types marked `#[non_exhaustive]` (`Event`, `Permission`). Everything else exhaustive → every zellij minor version risks compile breaks in consumers.
- Doc coverage ~21%. No CHANGELOG per crate; only workspace-level.
- Transitive deps: tokio, log4rs, interprocess, isahc, prost (no wasmtime, good).
- **Zero external Rust consumers found** via GitHub search (`"zellij-utils" language:Rust NOT zellij-org` returns nothing). Strong "workspace-private by practice" signal.

**IPC direct** (skip subprocess, talk to zellij-server unix socket):
- Protobuf + length-prefix over `interprocess::LocalSocketStream`. Unix only; no Windows transport.
- **Layout resolution happens client-side**. `zellij action new-tab --layout X` reads and parses the file in the client, ships typed `TiledPaneLayout` struct in the IPC message. So bypassing subprocess means ark must parse KDL itself regardless.
- No version negotiation on the wire; enums not `#[non_exhaustive]`.
- `.proto` files have low churn (single commit for `ipc.rs` in recent history).
- Net: trades one brittle surface (CLI output parsing) for another (internal protobuf schema).

**Embedding `zellij-server` — NOT VIABLE**:
- `start_server()` forks + daemonizes on Unix immediately. Destroys ark's parent process state.
- Global `OnceCell<Runtime>` for tokio → can't re-init; singleton.
- Installs SIGINT/SIGTERM handlers; conflicts with ark's.
- ~15 threads per session (std::thread + crossbeam, not tokio).
- wasmi mandatory; no feature flag.
- Requires controlling TTY (`ioctl TIOCSCTTY`); no headless mode.
- ~1300–2000 LOC refactor across 8–10 files to expose library-shaped API. **= fork**.

**Upstream signals**:
- Plugin APIs broke in 0.38, 0.41, 0.44. Source-level churn regular.
- No "zellij-core" extraction initiative in release notes or issues.
- imsnif (lead) dominant committer, continuity today but low bus factor long-term.
- Zellij architecturally "is the terminal," not a crate.

### 2. Alternative library-shaped muxes

- **wezterm `mux`** (`github.com/wezterm/wezterm/blob/main/mux`) — architecturally cleaner, headless server exists. **Not published to crates.io** (issues #27 and #6663 open for years). Git-dep only.
- **`cockpit`** (`crates.io/crates/cockpit`) — small published Ratatui-oriented multiplexer lib.
- **libghostty-rs** (mitchellh) — 12–24 months out, VT extracted but mux not yet.
- tmux (C, shell-out), kitty (Python), warp (closed) — not Rust lib candidates.

### 3. Verdict

Stay on zellij-via-subprocess for v1. **Don't depend on zellij crates.** The high-leverage move is not "depend on zellij differently" — it's **decouple ark's DSL from zellij's KDL grammar**. Today's coupling forces minijinja templating (because the layout body must be valid zellij KDL after render), and leaks zellij primitives into user-facing syntax.

## Strategic decision

**Own the layout DSL. Zellij is a rendering backend, not a vocabulary source.**

Consequences:
- Ark's layout grammar can use the same conventions as scene reactions (field-pattern selectors, `{var}` interpolation, facet-kdl typed schema, miette errors). One DSL, not two.
- If ark later embeds wezterm mux, adopts libghostty, or forks zellij, scenes don't rewrite.
- Minijinja and `validate_kdl()` brace-scanner both disappear. KDL LSP works on scene files.
- Ark can express concepts zellij can't (intent-aware panes, provider abstraction, mode-level `extends`).

## Locked DSL decisions

Every item below was argued through in conversation and landed.

### D1. Span-based sizing (not percentages)

- `span=N` is a relative weight within a container. All children's spans normalize to 100% at render.
- Compose cleanly: add a new pane with `span=1`, others rebalance automatically. No 60+40=100 arithmetic.
- Each container has its own span space (local, not global).
- Escape hatches for absolute sizing:
  - `cells=N` — fixed N cells (status bar: `cells=1`)
  - `min=N` / `max=N` — bounds in cells
- Compiles to zellij `size="N%"` (after normalization) or `size=N` (fixed).

### D2. Provider abstraction

A **provider** is what fills a pane. One provider per pane, always. Declared as a single child node inside the `pane { … }` block:

```kdl
pane @agent { command cmd={agent_cmd} args={agent_args} }
pane @shell { shell }
pane @file  { edit path=x.rs line=42 }
pane @diff  { diff cwd={cwd} }
pane @docs  { glow args=[README.md] }
pane @nvim  { nvim file=x.rs line=42 }
```

- **Node name = provider alias.**
- **Attrs = provider config** (schema'd per provider).
- One `{ … }` child per pane. Compiler error if zero or >1 provider nodes.

**Three tiers in the registry, same namespace:**

| Tier | Examples | Resolution |
|------|----------|------------|
| **Primitive** | `command`, `shell`, `edit` | Hardcoded codegen → zellij native content types (no plugin infrastructure) |
| **Shipped plugin** | `diff`, `git`, `picker`, `status` | Ark-bundled WASM → `pane { plugin location="shipped:…" }` |
| **Extension** | `glow`, `nvim`, `lazygit` | User-installed via `plugins.kdl` registry → `pane { plugin location="<resolved>" }` |

User writes `{ foo … }` — registry dispatches. User never cares about the backend.

**Shell is a primitive, not an extension.** Decided explicitly:
- Zero code to ship (it's $SHELL in a subprocess).
- It's the zero-config default; making it an extension would force `use shell` boilerplate.
- No plausible replacement provider (users swap $SHELL, not a "shell provider").
- Three primitives (`command`, `shell`, `edit`) all map 1:1 to zellij's native content types — the line is drawn by implementation, not philosophy.

**Providers vs intents (orthogonal):**
- **Provider** = pane filler (content).
- **Intent** = named, callable action (by keybind, reaction, or AI).
- A fancy provider can expose intents to manipulate itself. Example: `nvim` provider exposes `nvim.open_at_line(path, line)` intent. Reaction fires: `on FileEdited { nvim.open_at_line path={path} line={line} }`.
- A dumb provider (e.g. `glow`) ships no intents. Just renders.
- Intent registry is independent of provider registry.

### D3. `@handle` sigil for pane references

- **Handle**: stable name for a pane, scoped to one scene file.
- **Sigil**: `@` prefix marks a handle reference.
- Declaration: `pane @diff { … }` — declares `@diff`.
- Reference: `focus @diff`, `close @diff`, `resize @diff direction=right` — targets.

**Why handles are required:**
1. Readable refs in reactions and binds.
2. Identity across mode swaps (same `@agent` in base + `mode review` = same subprocess; ark uses `override-layout --retain-existing-terminal-panes`).
3. Compile-time checking (typo `@dff` is a compile error with did-you-mean suggestion).
4. Cross-scene composition (`extends=base` scene can redefine `@agent`'s provider without restructuring).

**Why `@` specifically:**
- Distinct from KDL attribute syntax (no conflict with `cwd=…`, etc.).
- One keystroke, greppable, familiar from mentions.
- Doesn't collide with `{var}` interpolation syntax.

**Handle scope**: one scene file (including anything it `extends` / `include`s). No global ambient namespace.

**Handle collisions = compile error.** `@agent` declared twice in same scope is rejected.

**Handle references without provider block** (in mode/alternate layouts):
```kdl
mode review {
    row {
        pane @agent span=1   // no { … } — means "reuse existing @agent"
    }
}
```
Subtle but powerful. Open question below on whether to make this implicit or use an explicit `keep @agent span=1` verb.

### D4. Un-hug zellij in vocabulary

Ark DSL uses ark-domain concepts. Zellij primitives are translation targets, not user-facing names.

| Zellij | Ark DSL | Why |
|---|---|---|
| `pane split_direction="horizontal"` | `row { … }` | Spatial, obvious |
| `pane split_direction="vertical"` | `col { … }` | " |
| `stacked=true` | `stack { … }` (v1.5 — deferred) | Tabbed cluster concept (renamed from `group` per user preference) |
| `floating_panes { pane x y w h }` | `overlay pos=top-right size=60x20 { … }` on a pane | Semantic placement, not absolute coords |
| `pinned=true` (floating) | `overlay sticky=true` | Flag on overlay, not pane-type |
| `borderless=true` | `chrome=none` vs `chrome=border` | Visual mode, not feature flag |
| `hold_on_close` / `close_on_exit` | `exit=keep` / `exit=auto` / `exit=prompt` | Explicit policy verbs |
| `start_suspended=true` | `launch=manual` vs `launch=auto` | Named strategy |
| `swap_tiled_layout` (pane-count trigger) | — not exposed; use `mode` + `use_mode` | Modes are explicit, not magic |

### D5. Modes for coarse reshape (via `override-layout`)

- Scene declares named alternate layouts: `mode review { … }`, `mode debug { … }`.
- `use_mode review` op compiles to: render the mode's KDL → `zellij action override-layout <path> --retain-existing-terminal-panes --apply-only-to-active-tab`.
- Handles survive the swap. `@agent` subprocess preserved; tree is restructured around it.
- **Modes DO NOT use zellij's swap_tiled_layout** (pane-count-triggered). Ark modes are event-triggered, user-invoked, or reaction-dispatched — explicit control, not pane-count magic.

### D6. Ephemeral panes via `spawn` op

- `spawn @handle overlay pos=top-right size=60x20 { glow args=[{path}] }` creates an ad-hoc pane.
- Optional `ttl=` and close triggers for auto-cleanup (ttl grammar TBD; `ttl=idle:30s` is a sketch).
- Compiles to zellij `action new-pane` (tiled) or `launch-plugin --floating` (overlay plugin).
- Ark stores returned zellij pane_id in handle→id map; subsequent ops on `@handle` dispatch by id.

### D7. Single `{var}` interpolation (kills minijinja)

- Matches scene reactions' interpolation syntax (from paused rethink memory).
- Compile-time substitution from spawn context: `cwd`, `agent_cmd`, `agent_args`, `id`, `name`.
- Inside `on` / `bind` op args, `{var}` also references event fields (e.g. `{path}` from `FileEdited path=…`).
- **No minijinja, no `{{ … }}`, no `{% for %}` / `{% if %}`**:
  - `{% for %}` over `agent_args`: replaced by accepting array values in KDL (`args={agent_args}` where the substituted value is already a list, expanded into multiple `args "…"` nodes during rendering).
  - `{% if %}`: replaced by `when=<CEL>` on tree nodes (same mechanism as reactions' predicates, already in cavekit R3 / paused rethink locked decisions).
- `validate_kdl()` brace scanner in `layout_template.rs` deleted. `kdl` crate validates natively.

## Canonical example (builder scene)

```kdl
scene builder extends=base {

    // ── Declarative skeleton (what exists on spawn) ───────────
    layout {
        tab cwd={cwd} {
            row {
                pane @agent span=3 { command cmd={agent_cmd} args={agent_args} }
                col span=2 {
                    pane @diff { diff cwd={cwd} }
                    pane @git  { git  cwd={cwd} }
                }
            }
        }
    }

    // ── Named alternate layouts (coarse mode switching) ───────
    mode review {
        tab cwd={cwd} {
            row {
                pane @agent span=1       // reuse existing handle; no { … }
                pane @diff  span=1
            }
        }
    }
    mode debug {
        tab cwd={cwd} {
            col {
                pane @agent
                pane @log { command cmd=ark args=[pane, log] }
            }
        }
    }

    // ── Reactions (event → ops) ───────────────────────────────
    on FileEdited path="**/*.md" {
        spawn @preview overlay pos=top-right size=60x20 {
            glow args=[{path}]
        }
    }
    on PhaseTransition to=review { use_mode review }
    on BuildFailed                { focus @log; use_mode debug }

    // ── Binds (user → ops) ────────────────────────────────────
    bind Alt+g       { focus @git }
    bind Alt+d       { focus @diff }
    bind Alt+Shift+r { use_mode review }
}
```

Reading top-to-bottom answers: what panes exist, what alternate shapes exist, what fires automatically, what user can trigger. Five sections, five jobs.

## Zellij capability mapping (ground truth confirmed)

Every ark construct has a confirmed zellij compile target. No synthesis gaps.

### Containers + panes

| Ark | Zellij |
|---|---|
| `row { … }` | `pane split_direction="horizontal" { … }` |
| `col { … }` | `pane split_direction="vertical" { … }` |
| `tab cwd=X { … }` | `tab cwd="X" { … }` (pass-through) |
| `pane @h span=N` | sibling pane, size computed from normalized span |
| `pane @h cells=N` | `pane size=N` (fixed) |
| `pane @h { command cmd=X args=[A,B] }` | `pane name="h" { command "X"; args "A" "B" }` |
| `pane @h { shell }` | `pane name="h"` (shell inherited) |
| `pane @h { edit path=P }` | `pane name="h" { edit "P" }` |
| `pane @h { <plugin-alias> config={…} }` | `pane { plugin location="<resolved>" { <config> } }` |
| `pane @h overlay pos=P size=WxH` | `floating_panes { pane name="h" x="…" y="…" width="…" height="…" … }` |
| `when=<CEL>` on pane/row/col | static → dead-branch elim; dynamic → re-emit `override-layout` (covered in cavekit R3) |
| `{var}` interpolation | compile-time substitute from context |

### Ops (in reactions + binds)

| Ark op | Zellij CLI |
|---|---|
| `focus @h` | `action focus-pane-id <h.pane_id>` |
| `close @h` | `action close-pane --pane-id <h.pane_id>` |
| `resize @h direction=right by=inc` | `action resize increase right --pane-id <h.pane_id>` |
| `move @h to=top-left` | `action change-floating-pane-coordinates --pane-id <h.pane_id> --x … --y …` |
| `spawn @new command=X mount=right` | `action new-pane --direction right --pane-name new` (ark stores returned id) |
| `spawn @new overlay pos=P { <provider> }` | `action new-pane --floating` or `launch-plugin --floating` |
| `write @h text=X` | `action write-chars --pane-id <h.pane_id>` |
| `pipe @h payload=X` | `action pipe --name <alias>` (plugin) or `write-chars` (terminal fallback) |
| `dump @h` | `action dump-screen --pane-id <h.pane_id>` → captured payload |
| `use_mode <name>` | render mode KDL → `action override-layout <path> --retain-existing-terminal-panes --apply-only-to-active-tab` |
| `<provider>.<intent>` (e.g. `nvim.open_at_line`) | ark-native → dispatches via extension protocol or pipe to provider's plugin |

### Zellij caps that force specific design choices

- **Pane placement is relative to focused pane only**, not absolute tree position. `spawn @new mount=right` = "right of focused." For handle-relative spawn, ark must `focus @target` first. Sugar candidate: `spawn … near=@target mount=right`.
- **Pane command immutable after spawn**. No `set_command @h` op. To change: `close @h; spawn @h command=new`.
- **No retrofit plugin onto terminal**. Plugins are their own pane-kind. Once a pane is a terminal, it stays a terminal.
- **Swap layouts trigger on pane count only**. Ark doesn't use swap_tiled_layout; uses `override-layout` under `mode` abstraction for semantic control.
- **Suspended panes unblock only on user keypress**. No programmatic `unblock-suspended` op.
- **`dump-layout` returns KDL text**, not structured tree. If ark needs structured state, ark must parse the KDL itself (using the `kdl` crate, which replaces minijinja everywhere).

## Interaction with paused rethink (memory)

The paused rethink (`memory/project_scene_plugin_rethink.md`, 2026-04-18) locked these decisions about scene REACTIONS that this handoff builds on:

- **Field-pattern selectors**: `on FileEdited path="**/*.md"` (native KDL, no CEL wrapping for common cases).
- **Unified `{var}` interpolation** in op args.
- **Plugin alias indirection**: `plugins.kdl` registry, `use <alias>` in scenes.
- **Schema-typed fields via facet SHAPE**: unknown field = compile error, typo suggestions.
- **Escape hatches**: `(exact)"x"`, `(glob)"x*"`, `(regex)"^x$"`, `when="<CEL>"`.

The paused rethink had 5 open questions before cavekit revision could start. **This handoff adds a 6th**: *should layout body use ark-native DSL instead of zellij-KDL pass-through?* — and answers **yes** with the full design above.

Cavekit revision should land these together. R3 ("Layout compilation") needs full rewrite, not just reaction-side changes.

## Open questions (need user input before cavekit revision)

### Q1. Handle reuse in mode blocks — implicit or explicit?

```kdl
mode review {
    pane @agent span=1           // Option A: implicit (no { … } means "reuse")
}
mode review {
    keep @agent span=1           // Option B: explicit verb
}
```

Option A is terser but relies on the reader knowing `@agent` was declared elsewhere. Option B is unambiguous. User instinct TBD.

### Q2. Provider-block vs flat sugar

```kdl
pane @agent { command cmd=X args=[A,B] }      // canonical
pane @agent command=X args=[A,B]              // sugar for single-provider case?
```

Sugar reduces indent on the 90% case but splits mental model. Recommend **no sugar for v1** — consistency > brevity.

### Q3. Overlay as pane-attr vs container-node

```kdl
pane @pop overlay pos=top-right size=60x20 { glow … }     // attr on pane
// vs
overlay pos=top-right size=60x20 { pane @pop { glow … } } // dedicated container
```

Former cleaner for single-pane overlays (99% case). Latter works for multi-pane overlays. Recommend **attr-on-pane** for v1; add container in v1.5 only if multi-pane overlays prove real.

### Q4. Provider intent namespace

```kdl
on FileEdited { nvim.open_at_line path={path} line={line} }   // dotted
on FileEdited { nvim:open_at_line path={path} line={line} }   // colon
on FileEdited { nvim/open_at_line path={path} line={line} }   // slash
```

Need to match whatever the paused rethink converged on for `UserEvent:<namespaced-name>` (which uses colon). Likely answer: colon for consistency.

### Q5. Fixed sizing spelling

`cells=N` vs `fixed=N` vs `rows=N`/`cols=N` (context-dependent). `cells` is ambiguous for rows vs cols. Probably want direction-aware: inside a `col`, fixed size = rows; inside a `row`, fixed size = cols. User needs to pick surface syntax.

### Q6. `ttl=` grammar

`ttl=idle:30s`, `ttl=event:BuildDone`, `ttl=keypress:q`. Not specified in detail. Defer to v1.1 or earlier if needed for `spawn` op.

## Out of scope for v1 (v1.5+)

- **Reactive state** (React-style `state { show_diff=false }` with auto-re-render). Mode-switching covers 80%.
- **`stack` (tabbed cluster)** — renamed from `group` per user preference, but defer implementation.
- **Multi-pane overlays** (dedicated `overlay { }` container node).
- **Zellij swap_tiled_layout exposure** (pane-count-triggered). Modes supersede for coarse reshape.
- **Dynamic tree restructuring within a single mode** — use modes or spawn/close.

## Migration work (if moving forward)

Files affected:

### Delete / rewrite
- `crates/mux/zellij/src/layout_template.rs` (338 LOC) — minijinja renderer + `validate_kdl()`. Replaced by provider-dispatching KDL AST builder.
- `crates/mux/zellij/src/layout_resolver.rs` (602 LOC) — needs rethink to dispatch by provider registry.
- `crates/mux/zellij/src/layout_writer.rs` (181 LOC) — stays (writes rendered KDL to $XDG_RUNTIME_DIR).

### Rewrite
- `crates/mux/zellij/layouts/*.kdl` (6 shipped layouts) — convert to ark-DSL.
- `crates/mux/zellij/scenes/*.kdl` (scene-shaped ports) — update to new syntax.
- `crates/scene/src/*` — parser and compiler rewrites, facet-kdl schema updates.

### New
- Provider registry crate or module (builtins + shipped plugin manifest + extension resolver).
- Span normalizer (computes `size="N%"` from spans during render).
- Handle → zellij pane_id runtime map (on top of existing supervisor tracking).

### Keep
- `Cargo.toml` zellij dep (none — confirmed we stay CLI-bound).
- `ZellijMux` in `crates/mux/zellij/src/mux.rs` — still the subprocess shell-out layer; only the input format to `new-tab --layout` changes (ark-DSL → ark-rendered zellij KDL).
- `crates/plugins/*` — shipped plugin binaries unchanged (WASM boundary preserved).

## What a fresh agent should do next

1. **Read this file end-to-end.**
2. **Read `context/kits/cavekit-scene.md`** especially R1, R3, R4, R11, R15 — this handoff overturns R3 entirely and amends R1 + R15.
3. **Read `memory/project_scene_plugin_rethink.md`** for the paused rethink context this builds on.
4. **Read `memory/project_scene_system.md`** for the overall scene system pointer.
5. **Either**:
   - Draft a cavekit revision that incorporates D1–D7 above into the existing cavekit-scene.md (probably R1 updated, R3 fully rewritten, new Rs for provider registry and spans), OR
   - Draft a build-site revision updating the ~60 tasks to reflect the new scope, OR
   - Propose answers to Q1–Q6 open questions so user can confirm before cavekit revision starts.
6. **Do NOT** start implementation. Cavekit revision must land first per project workflow.
