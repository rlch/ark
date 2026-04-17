---
title: Quick Start
description: Launch your first agent session in under 5 minutes
---

This guide gets you from install to a running agent session in under 5 minutes.

## 1. Launch a session

From any terminal:

```sh
ark
```

ark creates a new zellij session with:
- A **main pane** running the ACP agent (e.g. claude-code)
- A **diff pane** showing live file changes (powered by delta)
- A **git status pane** tracking the working tree
- A **status bar** plugin showing agent state

If you're already inside a zellij session, ark uses `zellij action switch-session` to move your client into the new session — no nesting.

## 2. Pick a scene

Bare `ark` uses the built-in default scene. Use `--scene` to pick another:

```sh
ark --scene myproject                      # resolves to $ARK_CONFIG_DIR/scenes/myproject.kdl
ark --scene ~/.config/ark/scenes/dev.kdl   # explicit path
```

## 3. Name the session

Use `--session` to attach-or-create a named zellij session:

```sh
ark --session work
```

Inside zellij (`$ZELLIJ` set) this switches the current client; outside, it
creates a new session.

## 4. List running agents

```sh
ark list
```

Shows all active agent sessions with their ID, name, orchestrator, phase, and
uptime. Sessions are backed by state files under `$XDG_STATE_HOME/ark/`.

## 5. Stop an agent

```sh
ark kill <agent-id>
```

Sends a graceful SIGTERM with a 10s grace window. Add `--force` to SIGKILL
instead. `ark doctor --fix` sweeps up any orphans afterward.

## Next steps

- **[Tour](/learn/tour/)** — a guided walkthrough building a scene from scratch
- **[Scenes](/scenes/overview/)** — understand the reactive KDL config system
- **[Extensions](/extensions/overview/)** — learn about the extension protocol
