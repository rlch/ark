---
title: Custom Keybind
description: Chord to intent to action
---

Keybinds map key chords to scene ops, letting you trigger actions without leaving the terminal.

## Basic keybind

```kdl
bind "Ctrl r" {
  reload_scene
}
```

Press `Ctrl+R` to hot-reload the scene.

## Multi-op keybind

```kdl
bind "Ctrl t" {
  spawn "@test" tab="@main" {
    command "cargo" args=["test"]
  }
}
```

## Chord syntax

Keybinds use space-separated chord notation:

| Chord | Meaning |
|-------|---------|
| `Ctrl r` | Control + R |
| `Alt s` | Alt + S |
| `Ctrl Shift p` | Control + Shift + P |

## Conditional keybinds

Use `when` to make a keybind context-dependent, with a Rhai predicate:

```kdl
bind "Ctrl d" when="agent.status == 'done'" {
  close "@agent"
}
```

The keybind only fires when the predicate is true.

## Modes

Group keybinds into modes for different contexts:

```kdl
mode "review" {
  bind "a" { pipe "@agent" "approve" }
  bind "r" { pipe "@agent" "reject" }
  bind "q" { reload_scene }
}
```

Activate a mode via a reaction or another keybind.
