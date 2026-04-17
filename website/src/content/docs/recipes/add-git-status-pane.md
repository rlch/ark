---
title: Add a Git Status Pane
description: Drop ark pane git into your scene
---

ark ships a built-in git status pane that watches your working tree and shows uncommitted changes.

## Add it to your scene

```kdl
scene "with-git" {
  use "ark:acp"
  use "ark:status"

  layout {
    tab "@main" focus=true {
      col {
        pane "@agent" { command "claude" }
        row span=30 {
          pane "@diff" { command "ark" args=["pane", "diff"] }
          pane "@git"  { command "ark" args=["pane", "git"] }
        }
      }
    }
  }
}
```

The `@git` pane runs `ark pane git`, which:
- Watches the working tree for file changes
- Renders staged/unstaged/untracked files
- Auto-refreshes on filesystem events (debounced)

## Alongside the diff pane

The `@diff` pane runs `ark pane diff`, showing a live `delta`-rendered diff of uncommitted changes. Together, `@diff` and `@git` give you full visibility into what the agent is changing.

## Sizing

`span=30` gives the bottom row 30% of the tab's height. Adjust to taste — `span=40` for more room, `span=20` for a compact view.
