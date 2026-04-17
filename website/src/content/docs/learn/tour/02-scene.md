---
title: "Author a Scene"
description: "Write a scene with panes and keybinds"
---

Now that you've seen the default scene, let's write a custom one.

## Create the scene file

Create `~/.config/ark/scenes/tutorial.kdl`:

```kdl
scene "tutorial" {
  use "ark:status"
  use "ark:picker"
  use "ark:acp"

  layout {
    tab "@main" name="Agent" focus="true" {
      col {
        pane "@agent" {
          command "claude"
        }
        row span=30 {
          pane "@diff" {
            command "ark" args=["pane", "diff"]
          }
          pane "@git" {
            command "ark" args=["pane", "git"]
          }
        }
      }
    }
  }
}
```

## What's in this scene

**`use` statements** load extensions. `ark:status` is the status bar plugin, `ark:picker` is the session switcher, and `ark:acp` connects to the agent via ACP.

**`layout`** defines the pane arrangement. It contains `tab` nodes, each with a quoted handle string. Inside a tab, `col` splits vertically, `row` splits horizontally, and `pane` is a leaf that runs a command.

**Handles** are required on every tab and pane. They are quoted string arguments to the `tab` and `pane` KDL nodes. Handles are used by reactions and the hot-reload system to identify nodes.

**`span=30`** allocates 30% of the parent container's height to the bottom row. Siblings normalize to 100%.

## Launch with your scene

```sh
ark --scene ~/.config/ark/scenes/tutorial.kdl
```

Or, since the file sits at `$ARK_CONFIG_DIR/scenes/tutorial.kdl`, use the bare name:

```sh
ark --scene tutorial
```

You should see the same layout as before, but now you own the config.

## Add a second tab

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
      pane "@log" { command "tail" args=["-f", "$XDG_STATE_HOME/ark/*/events.jsonl"] }
    }
  }
}
```

Now your session has two tabs: **Agent** and **Logs**.

## Next

Your scene defines layout and loads extensions. In the next step, you'll [add a reaction](/learn/tour/03-reaction/) that fires when the agent completes.
