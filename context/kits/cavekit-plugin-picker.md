---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Zellij Plugin — Agent Picker

## Scope
`ark-picker.wasm` — interactive zellij plugin modeled on zellij's built-in session-manager. Fuzzy-searchable list of active agents, detail expansion, kill/rename, new-agent form. Triggers session switch via zellij-tile actions. Sends administrative commands to ark host via control socket (see cavekit-hook-ipc).

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
- [ ] Dependencies: `zellij-tile`, `serde`, `serde_json`, `fuzzy-matcher`, `humantime`, `chrono`
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

### R3: Bootstrap + updates
**Description:** Plugin populates agent list on load and stays fresh.
**Acceptance Criteria:**
- [ ] On `load()`: request bootstrap via `pipe_message_to_plugin` to the ark host sending a `List` command; receive full state dump
- [ ] Alternatively (fallback), scan `$STATE/ark/agents/*/status.json` via WASI fs
- [ ] Incremental updates: supervisors pipe to `ark-picker` target on every event; plugin updates its cache
- [ ] 2s timer re-renders for timing-sensitive fields (stall age, "5m ago" counters)
**Dependencies:** cavekit-types-state-events

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
**Description:** `Ctrl+n` opens a spawn form; submit sends `Spawn` command via control socket.
**Acceptance Criteria:**
- [ ] Fields:
  - Orchestrator (radio: `cavekit | claude-code`)
  - CWD (text, `Ctrl+f` opens zellij filepicker plugin, echoes selection back)
  - Name (text; default = basename of cwd)
  - Layout (dropdown from shipped + user layouts)
  - Cmd (text; default = `claude --resume`)
- [ ] Tab/Shift+Tab cycle fields; Enter submits
- [ ] Submission sends to host via control socket: `{"cmd": "spawn", "args": {...}}`
- [ ] Host validates, performs spawn, returns `{"ok": true, "id": "..."}` or error; picker updates cache
**Dependencies:** cavekit-hook-ipc

### R7: Kill + rename + resurrect + detach
**Description:** Administrative actions on selected agent.
**Acceptance Criteria:**
- [ ] `Del`: enters `ConfirmKill` screen showing `[y] kill  [Y] kill + remove worktree  [n] cancel`
- [ ] `y`: sends `Kill { id, remove_worktree: false }` to host; host SIGTERMs supervisor
- [ ] `Shift+Y`: sends `Kill { id, remove_worktree: true }`; host removes worktree after kill
- [ ] `Ctrl+r`: rename flow — prompt for new name; sends `Rename { id, new_name }` to host; updates `spec.json.name`
- [ ] `Ctrl+d`: detach — picker removes from its display; agent continues running (sends `Forget { id }` to host, host writes `{ hide: true }` marker to status.json)
- [ ] `r` on a crashed agent: sends `Resurrect { id }` to host; host runs `ark spawn` with same spec
- [ ] All confirmations in-place; no leaving the picker
**Dependencies:** R4, cavekit-hook-ipc

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
- Wasm size target: < 800 KB

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
