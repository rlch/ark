---
title: Reload Without Respawn
description: Edit scene KDL and hot-reload
---

You can modify your scene while the agent is running and apply changes instantly.

## The workflow

1. Edit your scene KDL file
2. Validate it: `ark scene check my-scene.kdl`
3. Apply it: `ark scene reload`

Or bind reload to a key in the scene itself:

```kdl
bind "Ctrl r" {
  reload_scene
}
```

## What reloads in place

These changes apply without restarting anything:
- Adding, removing, or changing **reactions** (`on` blocks)
- Adding, removing, or changing **keybinds** (`bind` blocks)
- Loading new extensions (`use` added)
- Unloading extensions (`use` removed)
- Pane sizing changes (`span=`, `cells=`)

## What triggers respawn

These require the affected pane or tab to restart:
- Changing a pane's `command` or `args`
- Adding or removing a tab
- Structural layout changes (reordering panes, changing split direction)

## Turn-inflight gate

If the agent is actively generating (mid-turn), the reload waits until the current turn completes. This prevents disrupting work in progress.

## Validate first

Always validate before reloading a running session:

```sh
ark scene check my-scene.kdl
```

This catches parse errors, scope violations, and handle conflicts without touching the running session.
