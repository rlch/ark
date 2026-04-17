---
title: "Hot Reload"
description: "What reloads vs what requires respawn"
---

Scene files can be re-parsed and reconciled without restarting the session. Two entry points trigger a reload:

- **`reload_scene` op** -- from a reaction or keybind inside the running scene.
- **`ark scene reload --session <name>`** -- from the CLI, targeting a running session.

Both enter the same reconcile path.

## What happens on reload

1. **Re-parse + validate.** The scene file is re-read and fully validated. On failure: the old scene is kept, the error surfaces via `set_status`, and the session continues. Nothing tears down.

2. **Re-evaluate `when=` predicates.** All predicates run against the current Rhai scope.

3. **Render new desired layout.** The filtered layout compiles to zellij KDL.

4. **Issue `override-layout`.** Zellij reconciles: retains matched panes, creates missing, closes extras. Flags: `--retain-existing-terminal-panes --retain-existing-plugin-panes`.

5. **Diff subscriptions.** New `on` blocks are registered. Removed `on` blocks are dropped.

6. **Diff keybinds.** Changed bindings issue `rebind_keys` for deltas.

## What reloads vs what respawns

| Change | Effect |
|---|---|
| New/removed `on` block | Subscription added/dropped. No pane impact. |
| New/removed `bind` | Keybind delta applied via `rebind_keys`. |
| Pane `when=` predicate now true | Pane created by reconciler. |
| Pane `when=` predicate now false | Pane closed by reconciler. |
| New `use` (extension added) | Extension activated fresh. |
| Removed `use` (extension dropped) | Extension shut down (subprocess: stdin-close, SIGTERM, SIGKILL). |
| Same `use`, config changed | Extension receives `workspace/configuration` with new values. No restart. |
| `@handle` still present, same view | **Pane survives.** Process continues running. |
| `@handle` removed | Pane closed by reconciler. |
| Layout sizing changed (`span`, `cells`) | Override-layout applies new geometry. Process continues. |

The key principle: **handles are identity.** If a pane's `@handle` still exists in the reloaded scene with the same view, the running process is preserved.

## Turn-inflight gate

If any ACP session has an in-flight turn (a `session/prompt` awaiting a response), the reload is **queued**, not applied immediately. It fires once every active session receives a `stopReason`.

This prevents tearing down panes mid-conversation. The queued reload applies the scene as it was when the reload was requested.

## Re-entry guard

Concurrent `reload_scene` requests while a reload is already in progress are dropped with a debug log. Only one reload runs at a time (single-slot guard).

## File watcher

An optional file watcher triggers reload on scene file save. Enable it in `config.toml`:

```toml
[scene]
watch = true
```

The watcher uses the `notify` crate, debounced at 200ms, and ignores temp files (editor swap files, `.tmp`, etc.). The same 200ms debounce window applies to all reconciliation triggers.

## Telemetry

Each reload emits an `ark.scene.reloaded` event:

```json
{
  "duration_ms": 42,
  "status": "ok"
}
```

## Reload from a keybind

A common pattern -- bind a key to reload the scene for rapid iteration:

```kdl
bind "Alt r" {
    reload_scene
    set_status text="Scene reloaded" severity="info" ttl_ms=2000
}
```
