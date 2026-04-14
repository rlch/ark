---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: Multiplexer — Zellij

## Scope
The `ZellijMux` implementation of the `Multiplexer` trait. Creates zellij sessions, manages tabs from KDL layouts, pipes events to plugins, detects `$ZELLIJ` for in-vs-out-of-zellij spawn paths. Session-per-run model.

## Requirements

### R1: Session-per-run
**Description:** Every `ark spawn` creates a new zellij session named `ark-{orchestrator}-{name}`. Never nests or joins existing sessions.
**Acceptance Criteria:**
- [ ] Session name derived from `AgentSpec.session` (set at spawn time per cavekit-types-state-events R1)
- [ ] If session name collides with existing session, append `-{short-ulid}`
- [ ] Detect existing zellij context via `$ZELLIJ` env var:
  - **outside zellij** (`$ZELLIJ` unset): spawn new session via `zellij -s {session} --layout {path.kdl}` wrapped in `setsid` (POSIX) or double-fork to detach. Layout file MUST end in `.kdl` (zellij issue #4994: non-`.kdl` extensions silently fail with `--layout`)
  - **inside zellij** (`$ZELLIJ` set): ask current client to switch via `zellij action switch-session {session} [--layout {path.kdl}]` (zellij ≥ 0.44.1). Note: `switch-session` create-if-missing is the DEFAULT behavior; there is no `--create` flag (that flag exists on `attach`, not `switch-session`)
- [ ] Under no circumstance nest zellij clients (no `zellij attach` inside a running zellij)
- [ ] Switching sessions returns control to the caller; supervisor continues independently in its own process
**Dependencies:** cavekit-types-state-events R1

### R2: Tab creation from KDL layouts
**Description:** Orchestrator calls `create_tab` with a rendered KDL path; mux invokes zellij to materialize.
**Acceptance Criteria:**
- [ ] `create_tab(session, name, layout_path)` writes the KDL file path to a temp file if needed, then calls `zellij --session {session} action new-tab --layout {path} --name {name}` (not via plugin)
- [ ] For the first tab in a new session, the session is created directly with `--layout`; no extra action needed
- [ ] Returns `TabHandle { session, tab_index, name }` — tab_index retrieved by querying zellij's session state
- [ ] Tab names default to role slug (`builder`, `review`, `log`) — session name disambiguates
- [ ] If the layout file references `{{cwd}}`, `{{agent_cmd}}`, `{{agent_args}}`, mux templates them before calling zellij
- [ ] Templating uses handlebars or minijinja (bounded surface); never shells out
**Dependencies:** cavekit-layouts, cavekit-types-state-events

### R3: Tab close and rename
**Description:** Cleanup operations on TabHandles.
**Acceptance Criteria:**
- [ ] `close_tab(handle)` invokes `zellij --session {session} action close-tab-at-index {index}` or similar
- [ ] `rename_tab(handle, name)` invokes `zellij --session {session} action rename-tab --tab-index {index} --name {name}`
- [ ] Close is idempotent; closing a nonexistent tab returns Ok with a debug log
- [ ] Rename used for fallback progress display when status plugin is not installed (e.g., `builder 5/8`)
**Dependencies:** R2

### R4: Pipe to plugins
**Description:** Supervisor pushes events to the status-bar plugin and the picker plugin via `zellij pipe`.
**Acceptance Criteria:**
- [ ] `pipe(target_name, payload)` invokes `zellij pipe --name {target_name} -- {payload}`
- [ ] Target names used by v1:
  - `ark-status` — sent to the status-bar plugin
  - `ark-picker` — sent to the picker plugin (for incremental state updates)
- [ ] Payload is UTF-8 JSON string, one AgentEvent-derived message per pipe call
- [ ] Pipe failures are non-fatal; logged at warn. A missing plugin degrades to tab-rename fallback (see R3)
- [ ] Pipes are fire-and-forget; no response expected
**Dependencies:** cavekit-plugin-status, cavekit-plugin-picker

### R5: Layout rendering
**Description:** Turn a layout stem (`builder`) or path (`~/x.kdl`) into a concrete zellij-acceptable KDL file.
**Acceptance Criteria:**
- [ ] Layout stem resolution order:
  1. User override: `~/.config/ark/layouts/{stem}.kdl`
  2. Shipped: `{binary-dir}/share/ark/layouts/{stem}.kdl` (or embedded via `include_str!` and extracted on first use)
- [ ] Layout path: used verbatim after template substitution
- [ ] Template variables: `{{cwd}}`, `{{agent_cmd}}`, `{{agent_args}}` (as KDL array), `{{id}}`, `{{name}}`
- [ ] Rendered output written to `$XDG_RUNTIME_DIR/ark/layouts/{id}-{tab-name}.kdl` (temp file, cleaned on tab close). MUST use `.kdl` extension — zellij issue #4994 silently fails for other extensions when invoked with `--layout`
- [ ] Rendering validates KDL syntax before calling zellij (reject malformed with clear error)
**Dependencies:** cavekit-layouts

### R6: Preflight and diagnostics
**Description:** Fail fast when zellij is absent or wrong version.
**Acceptance Criteria:**
- [ ] Preflight (called by `ark doctor` and `ark spawn`):
  - `zellij --version` present
  - Version ≥ 0.44.1 (requires wasmi plugin host + switch-session action; 0.44.1 picks up web-client + scrollback fixes vs 0.44.0)
  - Required plugins locatable at configured paths
- [ ] Clear error messages: tells user exact install command (e.g., `brew install zellij` on macOS)
- [ ] Commands spawned with `zellij` use `tokio::process::Command`, capture stderr for error reporting
- [ ] All zellij invocations run with PATH only; no fancy shell expansion
**Dependencies:** cavekit-cli (ark doctor)

## Interaction with supervisor

Supervisor obtains `Arc<dyn Multiplexer>` from a factory based on config. For v1 the factory always returns `ZellijMux`. Inside supervisor loop:

```rust
let mux: Arc<dyn Multiplexer> = Arc::new(ZellijMux::new(config.mux.zellij.clone()));
mux.ensure_session(&spec.session).await?;
let tab = mux.create_tab(&spec.session, "builder", &layout_path).await?;
```

The orchestrator can call `mux.create_tab` further at any time (e.g., review tab on phase transition).

## Out of Scope
- Zellij plugin installation from mux code — handled by `ark doctor`
- Manipulating pane splits within a tab at runtime — KDL layout is authoritative; orchestrator-driven live re-splitting deferred to v2
- Headless / no-UI zellij — v1 assumes a TTY; CI tests mock the executor

## Cross-References
- cavekit-architecture.md — Multiplexer trait definition (R4)
- cavekit-layouts.md — KDL templates and shipped layouts
- cavekit-plugin-status.md — consumes `ark-status` pipes
- cavekit-plugin-picker.md — consumes `ark-picker` pipes
- cavekit-supervisor.md — owns the Mux Arc
