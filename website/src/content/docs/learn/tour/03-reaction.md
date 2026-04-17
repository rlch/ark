---
title: "Add a Reaction"
description: "React to agent events with Rhai predicates"
---

Scenes are reactive. An `on` block runs ops when a matching event fires.

## Add a reaction

Add this to your scene, after the `layout` block:

```kdl
on "AgentDone" {
  set_status "Agent finished — check the diff pane"
}
```

Now when the agent completes its task, ark updates the status bar.

## Event selectors

The `on` keyword takes a quoted event kind followed by optional field patterns:

```kdl
// Fire on any AgentDone event
on "AgentDone" { ... }

// Fire only when the outcome is "success"
on "AgentDone" outcome="success" { ... }

// Fire on file change events matching a glob
on "FileChanged" path="src/**/*.rs" { ... }
```

Field patterns bind as locals in the op body — you can reference `{path}` in op arguments.

## Conditional execution with `when`

The `when` attribute adds a Rhai predicate:

```kdl
on "AgentDone" when="outcome == \"success\"" {
  exec "echo" args=["Great work!"]
}
```

`when` is evaluated per-fire. If it returns false, the reaction is skipped. You can also put `when` on individual ops inside the body:

```kdl
on "AgentDone" {
  set_status "Agent done"
  emit "ReviewReady" when="outcome == \"success\""
}
```

## Multiple reactions

Multiple `on` blocks with overlapping selectors each run independently:

```kdl
on "AgentDone" {
  set_status "Agent finished"
}

on "AgentDone" outcome="success" {
  spawn "@review" tab="@main" {
    command "ark" args=["pane", "diff", "--full"]
  }
}
```

## Scene so far

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
  }

  on "AgentDone" {
    set_status "Agent finished"
  }

  on "AgentDone" outcome="success" {
    spawn "@review" tab="@main" {
      command "ark" args=["pane", "diff", "--full"]
    }
  }
}
```

## Next

You've added reactions to your scene. Next, you'll [wire an extension](/learn/tour/04-extension/) to add custom functionality.
