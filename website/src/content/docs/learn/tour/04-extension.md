---
title: "Wire an Extension"
description: "Register a compiled-in extension"
---

Extensions add capabilities to your scene. In this step you'll load the built-in extensions and see how they interact with your scene.

## Built-in extensions

ark ships three compiled-in extensions:

| Extension | What it does |
|-----------|-------------|
| `ark:status` | Status bar plugin — shows agent state, phase, and recent events |
| `ark:picker` | Session picker — fuzzy-search and switch between agent sessions |
| `ark:acp` | ACP bridge — connects ark to the agent via Agent Client Protocol |

You've already loaded them with `use` statements:

```kdl
use "ark:status"
use "ark:picker"
use "ark:acp"
```

## What `use` does

When ark compiles your scene, each `use` statement:

1. Resolves the extension by name (compiled-in extensions use the `ark:` prefix)
2. Loads its manifest (capabilities, provided views, intents, events)
3. Registers it with the session's intent registry
4. Starts the extension's runtime (in-process for compiled-in)

## Extension events

Extensions emit events into the event bus. The ACP extension emits `ark.acp.*` events that you can react to:

```kdl
on "UserEvent" name="ark.acp.session_update" {
  set_status "Agent state changed"
}

on "UserEvent" name="ark.acp.permission_request" {
  // Permission prompts are handled by the picker plugin automatically
  // but you can add custom reactions here
}
```

## Subprocess extensions

You can also load subprocess extensions — any executable that speaks NDJSON over stdio:

```kdl
use "./my-extension"  // local path to executable
```

See [Authoring a Subprocess Extension](/extensions/authoring/subprocess/) for how to write one.

## Next

Your scene now has layout, reactions, and extensions. In the final step, you'll [hot-reload](/learn/tour/05-reload/) the scene to see changes applied instantly.
