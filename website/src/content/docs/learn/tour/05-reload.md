---
title: "Hot-Reload"
description: "Edit your scene and see changes instantly"
---

Scenes are hot-reloadable. Edit the KDL file and apply changes without restarting the agent.

## Reload the scene

With your agent running, edit `tutorial.kdl` and then:

```sh
ark scene reload
```

Or add a keybind to your scene so you can reload from inside the session:

```kdl
bind "Ctrl r" {
  reload_scene
}
```

## What gets reloaded

ark diffs the old and new scene ASTs and applies only the changes:

| Change type | Behavior |
|-------------|----------|
| Reactions added/removed/changed | Applied in place |
| Keybinds added/removed/changed | Applied in place |
| Extension `use` added | Extension loaded |
| Extension `use` removed | Extension stopped |
| Pane command changed | Pane respawned |
| Layout structure changed (new tab/pane) | Structural change — may require respawn |
| Sizing (`span=`, `cells=`) | Applied in place |

## Turn-inflight gate

If the agent is mid-turn (actively generating), hot-reload waits until the current turn completes before applying changes. This prevents disrupting the agent during work.

## Validate before reloading

```sh
ark scene check tutorial.kdl
```

This parses and validates the scene without applying it. Catch errors before they hit a running session.

## Complete scene

Here's the final `tutorial.kdl` from this tour:

```kdl
scene "tutorial" {
  use "ark:status"
  use "ark:picker"
  use "ark:acp"

  layout {
    tab "@main" name="Agent" focus="true" {
      col {
        pane "@agent" { command "claude" }
        row span=30 {
          pane "@diff" { command "ark" args=["pane", "diff"] }
          pane "@git"  { command "ark" args=["pane", "git"] }
        }
      }
    }
    tab "@logs" name="Logs" {
      pane "@log" {
        command "tail" args=["-f", "$XDG_STATE_HOME/ark/*/events.jsonl"]
      }
    }
  }

  on "AgentDone" {
    set_status "Agent finished"
  }

  on "AgentDone" outcome="success" {
    spawn "@review" tab="@main" {
      command "ark" args=["pane", "diff", "--full"]
    }
  }

  bind "Ctrl r" {
    reload_scene
  }
}
```

## What's next

You've completed the tour. From here:

- **[Scenes reference](/scenes/overview/)** — deep dive into the scene system
- **[Extensions](/extensions/overview/)** — explore the extension protocol
- **[Recipes](/recipes/pin-agent-version/)** — task-oriented how-to guides
