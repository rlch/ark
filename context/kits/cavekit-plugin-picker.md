---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-16"
---

# Spec: Zellij Plugin — Agent Picker

> **v0.1 (shipped inline, current content):** zellij wasm plugin loaded by the built-in default scene as `plugin "picker" { source "shipped:picker"; mount "floating" }` with a default keybind (e.g., `Alt p`). The R1–R7 acceptance criteria below describe this runtime.
>
> **v0.3 (ported to ark-native extension, per plan T-10.10):** `ark-picker` becomes an ark extension with `ExtensionMetadata` + sidecar scene fragment. Default scene migrates to `use "picker"` form. Inline compat retained indefinitely.
>
> **v0.3 adds: ACP permission-modal surface.** The picker becomes the fallback renderer in the scene R17 5-tier permission-dispatch precedence (security-deny → auto-deny → auto-confirm → auto-allow → picker). On receiving a control-socket-forwarded `UserEvent:ark.acp.permission_requested` (only when no scene rule matches OR `auto-confirm` fires), picker shows a modal with `{tool, params, options}`; user selection routes back via `ark-hook permit --request-id <id> --outcome <…>` per cavekit-hook-ipc R1. Late responses (after ACP timeout) dropped silently by supervisor.

## Scope
`ark-picker.wasm` — interactive zellij plugin modeled on zellij's built-in session-manager. Fuzzy-searchable list of active agents, detail expansion, kill/rename, new-agent form, **ACP permission modals (v0.3+)**. Triggers session switch via zellij-tile actions. Sends administrative commands + `ark-hook permit` to ark host via control socket (see cavekit-hook-ipc).

## Reference
Mirrors zellij's `default-plugins/session-manager/` in Zellij repo. Uses SingleScreenMode pattern (filter + results in one screen). Uses `zellij-tile` primitives + `fuzzy-matcher` (SkimMatcherV2). No ratatui.

## Requirements

### R1: Plugin manifest + permissions
**Description:** Plugin bootstrap.
**Acceptance Criteria:**
- [ ] Crate: `ark-plugin-picker`, `crate-type = ["cdylib"]`
- [ ] Build target: `wasm32-wasip1`
- [ ] Permissions: `ReadCliPipes`, `ChangeApplicationState`, `ReadApplicationState`, `MessageAndLaunchOtherPlugins`
- [ ] Subscribed events: `EventType::Key`, `EventType::Timer`, `EventType::SessionUpdate`, `EventType::ModeUpdate`
- [ ] Dependencies: `zellij-tile`, `serde`, `nucleo-matcher` (NOT `fuzzy-matcher` — nucleo-matcher is faster and has lighter wasm footprint; only deps `memchr`). Avoid `serde_json`, `humantime`, `chrono` in plugin code (use hand-rolled formatters); see cavekit-distribution.md R3 size-reduction stack.
- [ ] `load()` registers pipe target: `ark-picker`
**Dependencies:** cavekit-mux-zellij

### R2: State model
**Description:** Internal UI state machine.
**Acceptance Criteria:**
- [ ] Screen enum mirrors session-manager:
  ```
  enum PickerScreen {
      List,              // main view
      Detail(AgentId),   // expanded row
      NewAgent(NewAgentForm),
      ConfirmKill(AgentId, KillScope),
      Help,
      Error(String),
  }
  ```
- [ ] `List` state: filter string, selected index, scroll offset
- [ ] Agents cache: `BTreeMap<AgentId, AgentSummary>` updated via pipe messages + bootstrap read
- [ ] Resurrectable agents: separate cache for crashed agents (pid dead) found via state dir scan
**Dependencies:** R1

### R3: Bootstrap + updates (no central listener)
**Description:** Plugin populates agent list on load via direct state-dir + socket-dir scan, then stays fresh via incremental pipe updates from supervisors. There is no central `List` command — the picker IS the aggregator (kakoune `kak -l` model).
**Acceptance Criteria:**
- [ ] On `load()`:
  1. Scan `$XDG_STATE_HOME/ark/agents/*/status.json` via WASI fs — gives full agent set including done/crashed
  2. Scan `${XDG_RUNTIME_DIR:-/tmp}/ark-$UID/agents/*.sock` — gives liveness signal (socket present = supervisor still bound)
  3. For each `.sock`, attempt `connect()` with 50ms timeout; on `ECONNREFUSED`/`ENOENT`-during-handshake, `unlink()` the stale socket file (kakoune `kak -l` GC pattern)
  4. Cross-reference: socket present + fresh status = `running`; socket absent + status `Done` = done; socket absent + status not Done = crashed (resurrectable)
- [ ] Incremental updates: supervisors pipe to `ark-picker` target on every event; plugin updates its cache
- [ ] 2s timer: re-render for timing-sensitive fields (stall age, "5m ago" counters) AND re-scan socket dir for liveness changes
- [ ] Per-agent detail (R5): on demand, `connect()` to that agent's socket and send `{"cmd":"Status"}` for the full snapshot (avoids polling every agent)
**Dependencies:** cavekit-types-state-events, cavekit-hook-ipc R4

### R4: List screen (W1 wireframe)
**Description:** Main screen layout, fuzzy search, per-agent summary.
**Acceptance Criteria:**
- [ ] Header line: `ark · agents`
- [ ] Filter input line, cursor at end, typing narrows list (fuzzy match on `{orchestrator}:{name}`)
- [ ] Agents list, 1 row per agent:
  - `{selector} {icon}  {orchestrator}:{name:<24}  {progress:>7}  {extra}  {age}`
  - Icons: ⟳ running, ⏸ stalled, ⚠ findings, ✓ done, ✗ failed, 💀 crashed, 🔍 reviewing
  - Crashed agents show `[R]` tag for resurrect
- [ ] Selected row highlighted via `Text.color_range(...)`
- [ ] Footer: `<↵> switch  <^n> new  <^r> rename  <Del> kill  <?> help`
- [ ] Width-aware: truncate long names with ellipsis; right-align progress column
**Dependencies:** R2, R3

### R5: Detail screen (W2 wireframe)
**Description:** Expanded view of a single agent on `→` or `Tab`.
**Acceptance Criteria:**
- [ ] Shows: session name, cwd (home-relative), orchestrator, engine, phase, iteration, started/last timestamps (humantime), review counts, last event summary
- [ ] Nested under the selected row in the list (session-manager expand-tree pattern) OR a panel on the right (TBD during impl — prefer nested for parity)
- [ ] `←` or `Tab` collapses back to list
**Dependencies:** R4

### R6: New-agent form (W3 wireframe)
**Description:** `Ctrl+n` opens a spawn form; submit `exec`s `ark spawn` as a subprocess (NOT a socket command — agent doesn't exist yet, so no socket exists yet).
**Acceptance Criteria:**
- [ ] Fields:
  - Orchestrator (radio: `cavekit | claude-code`)
  - CWD (text, `Ctrl+f` opens zellij filepicker plugin, echoes selection back)
  - Name (text; default = basename of cwd)
  - Layout (dropdown from shipped + user layouts)
  - Cmd (text; default = `claude --resume`)
- [ ] Tab/Shift+Tab cycle fields; Enter submits
- [ ] Submission: plugin uses zellij-tile `run_command` to exec `ark spawn --orchestrator <o> --cwd <c> --name <n> --layout <l> -- <cmd>` as a detached subprocess. `ark spawn` itself double-forks (cavekit-supervisor.md R1) and returns the new agent-id on stdout
- [ ] On exec failure (binary missing, validation error): plugin transitions to `Error(stderr)` screen
- [ ] On success: plugin returns to List screen; new agent appears once its socket binds and supervisors pipe their first event (typically <500ms)
- [ ] **Why subprocess not socket:** "no agents alive → no socket → can't spawn" deadlock is removed at the cost of fork+exec latency (~10ms, irrelevant for human-triggered actions). Precedent: wezterm `wezterm cli`'s connect-or-spawn pattern, coarsened.
**Dependencies:** cavekit-hook-ipc R4

### R7: Kill + rename + resurrect + detach
**Description:** Administrative actions on selected agent. Live actions (Kill/Rename/Forget) connect to that agent's per-supervisor socket. Resurrect (dead supervisor) execs `ark spawn`.
**Acceptance Criteria:**
- [ ] `Del`: enters `ConfirmKill` screen showing `[y] kill  [Y] kill + remove worktree  [n] cancel`
- [ ] `y`: connect to `${XDG_RUNTIME_DIR}/ark-$UID/agents/{id}.sock`, send `{"cmd":"Kill","args":{"remove_worktree":false}}`; supervisor SIGTERMs itself
- [ ] `Shift+Y`: same as above with `"remove_worktree":true`; supervisor removes worktree before exiting
- [ ] `Ctrl+r`: rename flow — prompt for new name; connect to agent socket, send `{"cmd":"Rename","args":{"new_name":"..."}}`; supervisor rewrites `spec.json.name`
- [ ] `Ctrl+d`: detach — connect to agent socket, send `{"cmd":"Forget","args":{}}`; supervisor writes `{"hide":true}` to status.json. Picker removes from display; agent continues running.
- [ ] `r` on a crashed agent (no live socket): plugin reads `$STATE/agents/{id}/spec.json`, then `run_command`s `ark spawn` with the same params (same path as R6 New-agent). Old agent's state dir archived to `$STATE/archive/{date}/{id}/` first.
- [ ] All confirmations in-place; no leaving the picker
- [ ] On socket connect failure mid-flow (supervisor died between list refresh and action): plugin shows "agent no longer alive — refresh? [y/n]" and re-runs R3 bootstrap on `y`
**Dependencies:** R4, cavekit-hook-ipc R4 + R5

### R8: Switch action
**Description:** `Enter` on a row — switch to that agent's zellij session.
**Acceptance Criteria:**
- [ ] For active agents: `switch_session(Some(session_name))` via zellij-tile
- [ ] For crashed/resurrectable: prompt user "agent crashed — resurrect?" → triggers R7 resurrect path
- [ ] `Esc` at any time: `hide_self()` closes picker
**Dependencies:** R2, cavekit-mux-zellij

### R9: Keybindings
**Description:** Exhaustive key map (matches session-manager patterns where possible).
**Acceptance Criteria:**
- [ ] `↑/↓`, `j/k` — navigate list
- [ ] `→/l` — expand selected / enter Detail
- [ ] `←/h` — collapse
- [ ] `Enter` — switch / confirm
- [ ] Type — filter; `Backspace` edits
- [ ] `Ctrl+n` — new-agent form
- [ ] `Ctrl+r` — rename
- [ ] `Ctrl+d` — detach
- [ ] `Del` — kill flow
- [ ] `Shift+Del` — kill all done/failed agents in view
- [ ] `r` — resurrect (crashed only)
- [ ] `/` — focus filter
- [ ] `?` — help overlay
- [ ] `Esc`, `Ctrl+c` — close
- [ ] `Tab` — cycle status filter preset (all / running / done)
**Dependencies:** R1

## Wireframes

### W1 — List
```
┌──────────────────────────────────────────────────────────────────┐
│                         ark  ·  agents                            │
│                                                                   │
│   filter: _                                                       │
│                                                                   │
│   > ⟳  cavekit:myfeat            5/8   iter 3/10   42s            │
│     ⏸  cavekit:payments          2/4   stall 2m                   │
│     ⟳  claude-code:ui-refresh    —     running                    │
│     ⚠  cavekit:billing-audit     7/8   3 P1         1m            │
│     ✓  cavekit:import-flags      8/8   done         5m            │
│     💀 cavekit:old-attempt       2/4   crashed      2h   [R]      │
│                                                                   │
│                                                                   │
│                                                                   │
│ Help: <↵> switch  <^n> new  <^r> rename  <Del> kill  <?> help     │
└──────────────────────────────────────────────────────────────────┘
```

### W2 — Detail
```
┌──────────────────────────────────────────────────────────────────┐
│                         ark  ·  agents                            │
│                                                                   │
│   filter: _                                                       │
│                                                                   │
│   > ⟳  cavekit:myfeat            5/8   iter 3/10   42s            │
│       ├─ session   ark-cavekit-myfeat                             │
│       ├─ cwd       ~/code/proj/../proj-myfeat                     │
│       ├─ orch      cavekit       engine  claude-code              │
│       ├─ phase     build         iter    3/10                     │
│       ├─ started   8m ago        last    42s ago                  │
│       ├─ review    pending                                        │
│       └─ last event   Edit src/auth.rs                            │
│                                                                   │
│     ⏸  cavekit:payments          2/4   stall 2m                   │
│     ⟳  claude-code:ui-refresh    —     running                    │
│     ...                                                           │
│                                                                   │
│ Help: <↵> switch  <^n> new  <^r> rename  <Del> kill  <?> help     │
└──────────────────────────────────────────────────────────────────┘
```

### W3 — New agent form
```
┌──────────────────────────────────────────────────────────────────┐
│                         ark  ·  new agent                         │
│                                                                   │
│   orchestrator:  [ cavekit ] claude-code                          │
│   cwd:           ~/code/proj-myfeat_               <^f> pick      │
│   name:          myfeat_                                          │
│   layout:        [ builder ] focused  classic  custom             │
│   cmd:           claude --resume_                                 │
│                                                                   │
│                                                                   │
│   <↵> spawn   <Tab> next field   <Esc> back                       │
└──────────────────────────────────────────────────────────────────┘
```

### W4 — Confirm kill
```
┌──────────────────────────────────────────────────────────────────┐
│                         ark  ·  agents                            │
│                                                                   │
│   ⚠  Kill cavekit:myfeat?                                         │
│                                                                   │
│      session will close, claude process terminated, worktree kept │
│                                                                   │
│      [y] kill    [Y] kill + remove worktree    [n] cancel         │
│                                                                   │
│     ⟳  cavekit:myfeat            5/8   iter 3/10   42s            │
│     ⏸  cavekit:payments          2/4   stall 2m                   │
│     ...                                                           │
└──────────────────────────────────────────────────────────────────┘
```

### W5 — Help overlay
```
┌──────────────────────────────────────────────────────────────────┐
│                         ark  ·  help                              │
│                                                                   │
│   navigation                                                      │
│     ↑/↓ or j/k      move selection                                │
│     → or l          expand detail                                 │
│     ← or h          collapse detail                               │
│     /               focus filter                                  │
│                                                                   │
│   actions                                                         │
│     <↵>             switch to agent session                       │
│     ^n              new agent (spawn form)                        │
│     ^r              rename agent                                  │
│     Del             kill agent (with confirmation)                │
│     Shift+Del       kill all done/failed agents                   │
│     ^d              detach (remove from ark, keep session)        │
│     r               resurrect (crashed supervisor only)           │
│                                                                   │
│   misc                                                            │
│     Tab             cycle status filter (all/running/done)        │
│     ?               this help                                     │
│     Esc             close picker                                  │
└──────────────────────────────────────────────────────────────────┘
```

## Distribution
- Embedded in `ark` binary via `include_bytes!`
- `ark doctor --fix` writes to `~/.config/zellij/plugins/ark-picker.wasm`
- Doctor prints a keybinding snippet (user decides; recommended `Ctrl+g a`):
  ```kdl
  shared {
      bind "Ctrl g" "a" {
          LaunchOrFocusPlugin "file:~/.config/zellij/plugins/ark-picker.wasm" { floating true }
      }
  }
  ```
- Wasm size: no hard budget; CI fails on >25% growth vs main. See cavekit-distribution.md R3 for size-reduction stack.

## Out of Scope
- Cross-user / remote agents
- Background agents without a zellij session — picker only lists agents that have a session
- Preview pane of agent content (that's the zellij session itself)
- Customizable chip formats via config — v2

## Cross-References
- cavekit-plugin-status.md — companion plugin
- cavekit-hook-ipc.md — control socket protocol for administrative commands
- cavekit-mux-zellij.md — session switching
- cavekit-types-state-events.md — event shapes
- cavekit-distribution.md — wasm embedding
