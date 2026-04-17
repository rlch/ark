---
title: Tour
description: A guided walkthrough of ark's core features
---

This tour walks you through ark's main workflow end-to-end. By the end, you'll have:

1. **Launched** a claude-code session with the default scene
2. **Authored** a custom scene with panes and keybinds
3. **Added** a reaction that triggers on agent events
4. **Wired** a compiled-in extension
5. **Hot-reloaded** your scene without restarting the session

Each step builds on the previous one, and the complete scene file is shown at the end.

## Before you start

Make sure you've [installed ark](/learn/install/) and run `ark doctor --fix`. You should have `ark`, `zellij`, and `delta` on your PATH.

You'll also need a working claude-code installation. Any ACP-speaking agent works — we use claude-code in this tour because it's the most common.

## The scene file

Throughout this tour, you'll build up a scene file at `~/.config/ark/scenes/tutorial.kdl`. A scene is a KDL 2.0 document that declares your agent workspace — layout, reactions, keybinds, and extensions.

```kdl
scene "tutorial" {
  // We'll fill this in step by step
}
```

Ready? Start with [Launch a Session](/learn/tour/01-launch/).
