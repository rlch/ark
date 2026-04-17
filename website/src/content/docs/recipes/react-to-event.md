---
title: React to an Event
description: Rhai predicate on AgentDone
---

Reactions let your scene respond to events. Here's how to fire an op when the agent finishes successfully.

## Basic reaction

```kdl
on "AgentDone" outcome="success" {
  set_status "Agent finished successfully"
}
```

This updates the status bar when the agent completes with a success outcome.

## With a Rhai predicate

For more complex conditions, use `when`:

```kdl
on "AgentDone" when="outcome == 'success' && artifacts.len() > 0" {
  set_status "Agent produced artifacts"
  spawn "@review" tab="@main" {
    command "ark" args=["pane", "diff", "--full"]
  }
}
```

## React to file changes

```kdl
on "FileChanged" path="src/**/*.rs" {
  pipe "@agent" "A Rust file changed: {path}"
}
```

The `path` field pattern binds as a local — `{path}` in the op body expands to the matched file.

## React to ACP events

```kdl
on "UserEvent" name="ark.acp.permission_requested" {
  set_status "Agent is requesting permission"
}
```

## Multiple ops per reaction

Ops run in order. If one fails, remaining ops in that reaction are skipped:

```kdl
on "AgentDone" outcome="success" {
  set_status "Done!"
  close "@review"
  pipe "@agent" "Run tests"
}
```
