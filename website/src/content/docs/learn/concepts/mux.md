---
title: Mux (Zellij)
description: Zellij as the UI layer
---

ark is **zellij-native**. It doesn't build a custom TUI — zellij *is* the UI.

## Why zellij

ark uses zellij as its terminal multiplexer because:

- **Sessions, tabs, panes** provide natural boundaries for agent workspaces
- **WASM plugins** allow ark to embed status and picker UIs directly in the terminal
- **Pipe protocol** enables bidirectional communication between ark and zellij plugins
- **Floating panes** support overlays for permission prompts and notifications

## Session-per-run

Every `ark` launch creates a new zellij session. Sessions are never reused or nested. This means:

- Each agent has complete isolation — its own tabs, panes, and plugin state
- `ark list` maps 1:1 to zellij sessions
- Switching between agents is just switching zellij sessions

## No abstraction layer

ark wraps zellij through a concrete `ZellijMux` type — not a trait, not an abstraction layer. There is no plan to support other multiplexers. This keeps the integration tight and avoids the overhead of a generic abstraction with a single implementation.

## What the user sees

When you run `ark`, you land in a zellij session with:

- **Main tab** — the agent's working area (panes defined by your scene)
- **Status bar** — a wasm plugin showing agent state, phase, and events
- **Picker** — accessible via keybind, lets you fuzzy-search and switch between all agent sessions

All of this is configured through the [scene](/learn/concepts/scene/). The scene's `layout` block compiles to zellij-compatible KDL at launch time.
