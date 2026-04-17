---
title: "Mux"
description: "Concrete ZellijMux and session-per-run"
---

`ZellijMux` is ark's concrete integration with zellij. There is no `Multiplexer` trait, no dyn dispatch, no mux abstraction layer. Ark ships zellij-only, and `ZellijMux` is the type consumers hold directly. This page covers the session-per-run model, the inherent API, layout rendering, and the StubExecutor testing approach.

## Design principle: concrete over trait

Ark follows the "concrete abstraction" principle: a trait exists only when a second production implementation exists or is concretely planned. A second terminal multiplexer is not planned. Test-only traits with a single production implementation are explicitly rejected. Instead, ark stubs at the subprocess boundary using `StubExecutor` (see [Testing](/architecture/testing/)).

This means:

- No `Multiplexer`, `TabOps`, `PluginPipe`, or `StatusChannel` traits exist in the workspace.
- Downstream code holds `Arc<ZellijMux>` directly.
- New zellij capabilities are added as inherent methods on `ZellijMux` when a consumer motivates them.

## Session-per-run model

Every `ark` launch creates a new zellij session. Sessions are never nested or reused.

```d2
direction: right

launch: "ark launch" {
  shape: oval
}

session: "zellij session\nark-cavekit-auth" {
  builder: "builder tab" {
    agent-pane: "agent pane\n(claude)"
    log-pane: "log pane"
  }
  review: "review tab" {
    review-pane: "review pane\n(codex)"
  }
  status: "status bar\n(ark-status.wasm)" {
    shape: rectangle
    style.stroke-dash: 3
  }
}

launch -> session: "mux.ensure_session()"
```

### Session naming

Session names follow the pattern `ark-{orchestrator}-{name}`, derived from `AgentSpec.session` at launch time. If the name collides with an existing session, ark appends `-{short-ulid}`.

### Inside vs. outside zellij

The launch path depends on whether `$ZELLIJ` is set:

**Outside zellij** (`$ZELLIJ` unset): Ark allocates a pty pair, spawns `zellij -s {session} --layout {path.kdl}` with the slave fd wired as stdin/stdout/stderr, and issues `TIOCSCTTY` so the slave is the child's controlling terminal. The launch helper calls `setsid(2)` in a `pre_exec` so zellij becomes the session leader of the pty. After a startup-grace poll confirms zellij did not exit non-zero, the master fd is dropped.

Null-stdio + setsid is forbidden. Zellij has no `--daemonize` mode -- its TUI client exits with code 2 when started without a real TTY.

**Inside zellij** (`$ZELLIJ` set): Ark asks the current client to switch via `zellij action switch-session {session}`. This is IPC-only over the caller's live zellij socket -- no pty, no setsid, no stdio nullification. The `switch-session` command creates the session if it does not exist (this is the default behavior; there is no `--create` flag).

Under no circumstance does ark nest zellij clients.

## Inherent methods

`ZellijMux` exposes these methods directly (no trait indirection):

```rust
impl ZellijMux {
    /// Returns "zellij".
    pub fn kind(&self) -> &'static str;

    /// Creates a session if it does not already exist.
    pub async fn ensure_session(&self, name: &str) -> Result<()>;

    /// Opens a tab from a KDL layout file.
    pub async fn create_tab(
        &self,
        session: &str,
        name: &str,
        layout_path: &Path,
    ) -> Result<TabHandle>;

    /// Closes a tab by handle. Idempotent.
    pub async fn close_tab(&self, handle: &TabHandle) -> Result<()>;

    /// Renames a tab (fallback progress display).
    pub async fn rename_tab(
        &self,
        handle: &TabHandle,
        name: &str,
    ) -> Result<()>;

    /// Sends a payload to a named plugin pipe target.
    pub async fn pipe(
        &self,
        target_name: &str,
        payload: &str,
    ) -> Result<()>;
}
```

### TabHandle

```rust
pub struct TabHandle {
    pub session: String,
    pub tab_index: u32,
    pub name: String,
}
```

Tab names default to role slugs: `builder`, `review`, `log`. The session name disambiguates when multiple agents run concurrently.

## Tab creation from KDL layouts

Orchestrators call `create_tab` with a rendered KDL path. For the first tab in a new session, the session is created directly with `--layout`. Subsequent tabs use `zellij --session {session} action new-tab --layout {path} --name {name}`.

### Layout rendering

A layout starts as either a stem (e.g., `builder`) or an absolute path. Stem resolution order:

1. User override: `~/.config/ark/layouts/{stem}.kdl`
2. Shipped: embedded via `include_str!` and extracted on first use

Template variables are substituted before the KDL is written:

| Variable | Expansion |
|---|---|
| `{{cwd}}` | Working directory for the agent |
| `{{agent_cmd}}` | Primary agent command |
| `{{agent_args}}` | Agent arguments as a KDL array |
| `{{id}}` | Agent ID |
| `{{name}}` | Human-friendly name |

Rendering writes the output to `$XDG_RUNTIME_DIR/ark/layouts/{id}-{tab-name}.kdl`. The `.kdl` extension is mandatory -- zellij silently fails for other extensions when invoked with `--layout` (zellij issue #4994). Ark validates KDL syntax before calling zellij.

Templating uses a bounded template engine (handlebars or minijinja). It never shells out.

## Pipe to plugins

The supervisor pushes events to zellij plugins via `pipe`:

```rust
// Status bar plugin
mux.pipe("ark-status", &serde_json::to_string(&event)?).await?;

// Picker plugin
mux.pipe("ark-picker", &serde_json::to_string(&event)?).await?;
```

Pipe calls invoke `zellij pipe --name {target_name} -- {payload}` under the hood. They are fire-and-forget: failures are logged at warn level but are non-fatal. If the status plugin is missing, the supervisor falls back to `rename_tab` for progress display (e.g., `builder 5/8`).

## Preflight diagnostics

Before first use, ark validates the zellij installation:

- `zellij --version` present on PATH
- Version >= 0.44.1 (required for wasmi plugin host + switch-session)
- Required plugins locatable at configured paths

Preflight runs during both `ark doctor` and session launch. Clear error messages tell the user the exact install command for their platform.

All zellij invocations use `tokio::process::Command`, capture stderr for error reporting, and run with PATH only (no shell expansion).

## Interaction with supervisor

The supervisor constructs `ZellijMux` directly from config. No factory, no indirection:

```rust
let mux = Arc::new(ZellijMux::new(config.mux.zellij.clone()));
mux.ensure_session(&spec.session).await?;

// First tab comes from session creation
let tab = mux.create_tab(&spec.session, "builder", &layout_path).await?;

// Orchestrator opens more tabs as needed
let review_tab = mux.create_tab(&spec.session, "review", &review_layout).await?;
```

The `Arc<ZellijMux>` is shared via the `World` struct. The orchestrator can call `create_tab` at any point during its `run` method.

## Testing with StubExecutor

`ZellijMux` takes an executor parameter that determines how zellij CLI commands are dispatched. In production, commands run via `tokio::process::Command`. In tests, a `StubExecutor` records command sequences for assertion:

```rust
#[test]
fn create_tab_invokes_correct_zellij_args() {
    let executor = StubExecutor::new();
    let mux = ZellijMux::with_executor(executor.clone());

    // ... call mux methods ...

    let cmds = executor.recorded_commands();
    assert_eq!(cmds[0].program, "zellij");
    assert_eq!(cmds[0].args, &[
        "--session", "ark-cavekit-auth",
        "action", "new-tab",
        "--layout", "/tmp/ark/layouts/abc-builder.kdl",
        "--name", "builder",
    ]);
}
```

This avoids the need for a mux trait. The stub operates at the subprocess boundary, not the method boundary. See [Testing](/architecture/testing/) for more on this approach.
