---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-16"
---

# Spec: Zellij Plugin — Status Bar

> **v0.1 (shipped inline, current content):** zellij wasm plugin loaded by the built-in default scene as `plugin "status" { source "shipped:status"; mount "status-bar" }`. The R1–R5 acceptance criteria below describe this runtime.
>
> **v0.3 (ported to ark-native extension, per plan T-10.10):** `ark-status` is repackaged as a first-class ark extension with `ExtensionMetadata` declared via `register_extension!` macro + sidecar scene fragment. The default scene migrates silently from `plugin "status" { source "shipped:status" }` to `use "status"` form. Inline compat retained indefinitely (Rust-editions precedent). Runtime behavior unchanged. The R1–R5 requirements below remain the behavioral contract the ported extension must match.

## Scope
`ark-status.wasm` — a zellij plugin rendering a status bar row with at-a-glance progress per agent. Consumes events via `zellij pipe` from every active supervisor. No shared state with ark's host binary except via pipe.

## Background
Zellij plugins are `wasm32-wasip1` modules using `zellij-tile` primitives (Text, Table, NestedListItem). No ratatui — bloats wasm 1-2MB for no visual gain. Pipes via `zellij pipe --name <target> -- <payload>`. Plugin handles `pipe(msg)` callback.

## Requirements

### R1: Plugin manifest + permissions
**Description:** Plugin requests the minimal zellij permissions needed.
**Acceptance Criteria:**
- [ ] Plugin crate: `ark-plugin-status`, `crate-type = ["cdylib"]`
- [ ] Build target: `wasm32-wasip1`
- [ ] Dependencies: `zellij-tile`, `serde`, `serde_json`
- [ ] Permissions requested on load: `ReadCliPipes`
- [ ] `load()` subscribes to `EventType::Timer` (1s tick for freshness), `EventType::PermissionRequestResult`
- [ ] Plugin registers name: `ark-status`
**Dependencies:** cavekit-mux-zellij

### R2: Ingestion from pipe
**Description:** Receive AgentEvent-derived messages from supervisors.
**Acceptance Criteria:**
- [ ] Handle `pipe(message: PipeMessage)` callback; filter by `message.source.name == "ark-status"`
- [ ] Payload is a JSON object with shape:
  ```
  { "id": "<AgentId>", "name": "<label>", "orchestrator": "<slug>",
    "phase": "running|idle|prompting|reviewing|done|failed|crashed",
    "progress": [done, total],
    "findings": { "p0": N, "p1": N, "p2": N, "p3": N },
    "stalled_since_secs": N | null }
  ```
- [ ] Maintain in-memory map `BTreeMap<AgentId, StatusSummary>` (ordered → deterministic render)
- [ ] Keep last 60 minutes of done/crashed agents, then evict; active agents never evicted
- [ ] Render redraw triggered on every pipe message (or 1s timer if stale)
**Dependencies:** cavekit-types-state-events

### R3: Render format
**Description:** Single-row status bar output.
**Acceptance Criteria:**
- [ ] Render output fits the 2-row borderless status pane convention zellij uses
- [ ] Row content: space-separated agent chips, e.g. `⟳ auth 5/8  ⏸ pay 2/4  ⚠ billing:3P1  ✓ import`
- [ ] Chip format: `{icon} {orchestrator}:{name} {extra}`:
  - `⟳` running, cyan
  - `⏸` stalled, yellow
  - `⚠` finding(s), orange/red (severity-colored)
  - `✓` done, green
  - `✗` failed, red
  - `💀` crashed, magenta
  - `🔍` reviewing, purple
- [ ] `extra`: progress `N/M` if available, else phase text, else age (`5m ago`)
- [ ] Truncate when row exceeds terminal width: elide middle agents with `…` marker; current orchestrator/name of focused session always visible (based on zellij-tile `get_focused_session_name`)
- [ ] Uses `Text::new(...).color_range(Color::Fixed(N), range)` per-chip coloring
**Dependencies:** R2

### R4: Fallback for missing pipe data
**Description:** Graceful if status dir is more up-to-date than pipe.
**Acceptance Criteria:**
- [ ] On 1s timer: scan `$XDG_STATE_HOME/ark/agents/*/status.json` (via WASI fs if permitted) for any agents not in pipe cache
- [ ] Add them to chip map with `phase: "running"` and progress if found
- [ ] If plugin lacks WASI fs permission, skip fallback — pipe-only operation
- [ ] Pipe-only operation is fine; fallback is best-effort
**Dependencies:** cavekit-types-state-events

### R5: Distribution
**Description:** How the wasm reaches users.
**Acceptance Criteria:**
- [ ] Compiled wasm is embedded in the `ark` binary via `include_bytes!`
- [ ] `ark doctor --fix` writes the embedded wasm to `~/.config/zellij/plugins/ark-status.wasm` (overwrites existing; doctor says so)
- [ ] Doctor prints a KDL snippet users paste into their zellij config default layout to enable the plugin
- [ ] Wasm size: no hard budget (zellij-tile floor is ~480 KB after wasm-opt; comparable status plugins land 1-2 MB). CI fails on >25% growth vs main — see cavekit-distribution.md R3 for the size-reduction stack.
**Dependencies:** cavekit-distribution, cavekit-cli R5

## Example rendered status bar (main cockpit session)
```
╔══ zellij session: main (user cockpit) ════════════════════════════════╗
║ [cockpit] [editor] [scratch]                                            ║
║ ┌──────────────────────────────────┬──────────────────────────────────┐║
║ │ ark list --watch                 │ shell                            │║
║ │                                  │                                  │║
║ │ ID          RUNNER    TASKS      │ $ ark spawn --orchestrator       │║
║ │ ─────────── ───────── ─────      │   cavekit --cwd ../proj-myfeat   │║
║ │ ⟳ myfeat    cavekit   5/8        │ spawned cavekit-myfeat-01JX…     │║
║ │ ⏸ payments  cavekit   2/4        │ → Ctrl+o w to switch             │║
║ │ ⟳ ui-refr.  claude-c. —          │                                  │║
║ │ ✓ imports   cavekit   8/8        │ $ _                              │║
║ │                                  │                                  │║
║ │ ⚠ 3 P1 findings on myfeat        │                                  │║
║ └──────────────────────────────────┴──────────────────────────────────┘║
║ ⟳ myfeat 5/8  ⏸ payments 2/4  ⟳ ui-refresh  ✓ imports  ⚠ myfeat:3P1     ║
╚═════════════════════════════════════════════════════════════════════════╝
                ↑ ark-status plugin renders this row
```

Icon semantics:
- `⟳` running (cyan)
- `⏸` stalled (yellow)
- `⚠` has findings (orange → red by severity)
- `✓` done (green)
- `✗` failed (red)
- `💀` crashed (magenta)
- `🔍` reviewing (purple)

## Example KDL snippet (user pastes into zellij config)
```kdl
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="file:~/.config/zellij/plugins/ark-status.wasm"
        }
        children
    }
}
```

## Graceful degradation
- If plugin is not installed: supervisors still work; progress shown only by tab renames (mux `rename_tab` with format `{role} {done}/{total}`).
- If pipe fails: mux logs warning, supervisor continues; plugin displays stale but known data.

## Out of Scope
- User interaction inside status bar (clicks, keys) — v1 is read-only
- Customizable color themes — use terminal palette
- Per-agent detail overlay (that's the picker plugin)
- Cross-session aggregation UI (single-row aggregate only)

## Cross-References
- cavekit-mux-zellij.md R4 — pipe invocation from supervisor
- cavekit-plugin-picker.md — companion wasm for interactive picker
- cavekit-types-state-events.md — event shapes fed via pipe
- cavekit-distribution.md — wasm embedding
