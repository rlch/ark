---
title: "Launch a Session"
description: "Start an agent session with the default scene"
---

In this step you'll launch a session using ark's default scene.

## Run it

```sh
ark
```

ark does the following:

1. Forks a **supervisor** process (double-fork + setsid — it daemonizes)
2. Compiles the **default scene** (built-in layout with diff pane, git status pane, and status bar)
3. Creates a new **zellij session** and hands your terminal to it
4. Starts the ACP engine (e.g. `claude`) in the main pane
5. Signals readiness back to your shell

You're now inside a zellij session. You should see:

- **Left/top pane** — the agent running
- **Right/bottom pane(s)** — live diff and git status
- **Status bar** — the ark status plugin showing agent state

## What just happened

Launching ark created:

- A supervisor process (check with `ark list`)
- A state directory at `$XDG_STATE_HOME/ark/<agent-id>/`
- A control socket used by `ark kill`, `ark scene reload`, and the picker
- A zellij session you can detach from and reattach to

## Try it

- Detach with `Ctrl+O`, `D` (zellij default)
- Reattach with `ark --session <name>` or `zellij attach`
- List all agents with `ark list`

## Pick a scene

Bare `ark` uses the built-in default scene. To launch with your own scene, pass
`--scene` with either a bare name (resolved against
`$ARK_CONFIG_DIR/scenes/<name>.kdl`) or a path:

```sh
ark --scene tutorial
ark --scene ~/.config/ark/scenes/tutorial.kdl
```

## Next

You've launched a session with the default scene. In the next step, you'll
[write your own scene](/learn/tour/02-scene/) to customize the layout.
