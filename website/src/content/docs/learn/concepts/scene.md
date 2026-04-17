---
title: Scenes
description: Reactive KDL config — the one artifact you write by hand
---

A **scene** is a KDL 2.0 document that declaratively defines everything about an agent workspace: the layout, reactions, keybinds, and which extensions to load.

## One file, one session

Each `ark` launch compiles one scene into a running zellij session. The scene is the single artifact you author — everything else (extensions, agents, the mux) is configured through it.

```kdl
scene "my-workflow" {
  use "ark:status"
  use "ark:picker"
  use "ark:acp"

  layout {
    tab "@main" focus="true" {
      col {
        pane "@agent" { command "claude" }
        row span=30 {
          pane "@diff" { command "ark" args=["pane", "diff"] }
          pane "@git"  { command "ark" args=["pane", "git"] }
        }
      }
    }
  }

  on "AgentDone" {
    set_status "Agent finished"
  }

  bind "Ctrl r" {
    reload_scene
  }
}
```

## Three responsibilities

A scene covers three concerns:

1. **Layout** — tabs, panes, splits, sizing. Compiles to zellij-compatible KDL at launch time.
2. **Reactions** — `on` blocks that fire ops in response to events (agent events, user events, extension events).
3. **Composition** — `use` statements that load extensions, `bind` statements that map keybinds to intents.

## Reactive, not static

Unlike a plain zellij layout, a scene is reactive. When events fire, reactions execute ops that can modify the session — create panes, close panes, exec commands, reload the scene itself. The `when` attribute on any op provides conditional execution via Rhai predicates.

## Hot-reloadable

Edit the scene file and run `ark scene reload --session <id>` (or use a keybind). ark diffs the old and new scenes and applies only the changes — reactions, keybinds, and plugin lifecycle are updated in place. Layout changes that require structural modification trigger a full relaunch only when necessary.

See [Hot Reload](/scenes/hot-reload/) for details on what reloads vs. what requires respawn.
