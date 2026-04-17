---
title: "Scene Ops"
description: "All scene operations reference"
---

Ops are the actions that run inside `on` reactions and `bind` keybinds. Each op maps to a named intent in ark's registry. All op string attributes support `{Rhai}` interpolation.

## Pane and tab ops

Handle-addressed ops that target an existing pane or tab. All are idempotent on absent handles (silently succeed if the target is gone).

| Op | Syntax | Target | Description |
|---|---|---|---|
| `focus` | `focus "@handle"` | Tab or pane | Transfer focus. Polymorphic -- resolved from handle type at compile time. |
| `close` | `close "@handle"` | Tab or pane | Close the target. |
| `rename` | `rename "@handle" to="name"` | Tab only | Rename a tab. |
| `resize` | `resize "@handle" direction="up" by="inc"` | Pane only | Resize a pane. Direction: `up`, `down`, `left`, `right`. By: `inc`, `dec`. |
| `move` | `move "@handle" to="top-right"` | Pane only | Reposition a pane. |
| `pin` | `pin "@handle"` | Overlay pane | Pin overlay so it survives tab switch. |
| `unpin` | `unpin "@handle"` | Overlay pane | Unpin a previously pinned overlay. |

Handle type mismatches are compile errors: `error[scene/handle-type-mismatch]`.

## Spawn ops

Create new panes or tabs. Both follow a **check-then-create-else-focus** policy: if the handle already exists, the op focuses the existing target instead of spawning a duplicate.

| Op | Syntax | Description |
|---|---|---|
| `spawn` | `spawn "@handle" { <view> }` | Create a tiled pane with the given view. |
| `spawn` (overlay) | `spawn "@handle" overlay pos="center" size="60%x40%" { <view> }` | Create a floating overlay pane. |
| `new_tab` | `new_tab "@handle" name="review" cwd="/path"` | Create a new tab. |

```kdl
on "Started" {
    spawn "@logs" overlay pos="bottom-right" size="80x20" {
        command cmd="ark" args=["pane", "log", "--id", "{id}"]
    }
}
```

## Mode ops

| Op | Syntax | Description |
|---|---|---|
| `use_mode` | `use_mode "review"` | Switch the active tab to the named mode layout. |
| `use_mode` | `use_mode "default"` | Revert to the primary layout. |

Modes are named alternate whole-tab layouts declared at scene root with `mode "name" { }`. Handles survive mode switches -- same `@handle` across base and mode preserves the subprocess.

## Messaging ops

Always produce side effects (no idempotency shortcuts). Absence of a bus or mux at runtime produces a clean error.

| Op | Syntax | Description |
|---|---|---|
| `pipe` | `pipe from=@src to=@dst payload="data"` | Forward a payload between two panes. |
| `emit` | `emit "event.name" { key "value" }` | Emit a UserEvent on the event bus. |
| `set_status` | `set_status text="Ready" severity="info" ttl_ms=5000` | Push a message to the status bar. |

```kdl
on "Done" {
    emit "user.build-complete" {
        result "success"
    }
    set_status text="Build done" severity="info" ttl_ms=3000
}
```

`emit` cascade depth is bounded at 4 by default (configurable via `scene "name" max-cascade-depth=N`).

## ACP ops

Sub-namespaced `acp.*` ops for controlling ACP-capable agents. All no-op with a warning if no ACP-capable extension is active.

| Op | Syntax | Description |
|---|---|---|
| `acp.prompt` | `acp.prompt text="Fix the failing test"` | Send a user message into an ACP agent session. |
| `acp.cancel` | `acp.cancel` | Cancel the in-flight turn. |
| `acp.permit` | `acp.permit request_id="abc" outcome="allow"` | Respond to a permission request. Outcomes: `allow`, `reject_once`, `reject_always`. |
| `acp.set_mode` | `acp.set_mode mode="plan"` | Set the agent's mode (plan, edit, etc.). |

## Control ops

| Op | Syntax | Description |
|---|---|---|
| `exec` | `exec script="cargo test" shell="bash" timeout_ms=30000` | Run a shell script. Default shell is `sh`. Default timeout is 30s. Returns the exit code. |
| `reload_scene` | `reload_scene` | Re-parse the scene and reconcile. Honors the turn-inflight gate and re-entry guard. |

```kdl
bind "Alt r" {
    reload_scene
    set_status text="Scene reloaded" severity="info" ttl_ms=2000
}
```

## Op failure behavior

When an op fails, ark logs `error[scene/op-failed]` and **skips the remaining ops in that reaction**. The event loop continues processing subsequent events. Unknown ops produce `error[scene/unknown-op]` with "did you mean ...?" suggestions at compile time.

## Idempotency summary

| Op | Policy |
|---|---|
| `focus`, `close`, `rename`, `resize`, `move`, `pin`, `unpin` | No-op on absent handle |
| `spawn`, `new_tab` | Check-then-create-else-focus |
| `pipe`, `emit`, `set_status`, `exec` | Always side-effect |
| `reload_scene` | No-op when no reloader installed |
