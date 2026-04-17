---
title: "Reactions & Keybinds"
description: "Event selectors, Rhai predicates, and chord syntax"
---

Reactions and keybinds are the two ways a scene responds to runtime events. Both use the same op grammar in their bodies.

## Reactions (`on` blocks)

A reaction is an event selector + optional Rhai predicate + ordered op list.

```kdl
on "Started" {
    set_status text="Session ready" severity="info" ttl_ms=3000
}

on "Done" {
    close "@agent"
    emit "user.session-done"
}
```

### Event selectors

The selector syntax is `on "EventKind" field=pattern ... { ops }`. The event kind is a quoted string; field patterns are KDL properties.

```kdl
// Match any UserEvent named "user.build-complete"
on "UserEvent" name="user.build-complete" {
    focus "@results"
}

// Match Log events where the line contains "error"
on "Log" line=(glob)"*error*" {
    set_status text="Error detected" severity="error"
}
```

**Field pattern match types:**

| Annotation | Behavior | Default for |
|---|---|---|
| `(glob)` | Glob matching | Path-like fields |
| `(exact)` | Exact string match | String/enum fields |
| `(regex)` | Regex match (RE2, no backrefs) | Explicit opt-in |

Field names are validated against the event variant's fields via facet reflection. Unknown fields produce `error[scene/unknown-event-field]` with suggestions.

### Selector-captured locals

Field patterns bind as locals in the op body. A matched field value is available via `{field_name}` interpolation:

```kdl
on "UserEvent" name="user.file-changed" path="**/*.rs" {
    set_status text="Changed: {path}" severity="info" ttl_ms=2000
}
```

If the event's `path` field matched `src/main.rs`, the status text renders as `"Changed: src/main.rs"`.

### UserEvent field routing

For `UserEvent`, bare field names route into the `payload` map. Reserved top-level keys are `name`, `source`, and `payload`. Use the `payload.` prefix as an explicit escape hatch:

```kdl
on "UserEvent" name="build.done" result="success" {
    // "result" resolves to payload.result
    emit "user.notify" { msg "Build {result}" }
}
```

### `when=` predicates

An optional Rhai expression evaluated per event fire. The reaction is skipped when it returns `false`.

```kdl
on "PhaseTransition" when=#"agent.phase == "review""# {
    focus "@diff"
    rename "@main" to="Review"
}
```

`when=` is also legal on individual ops inside the body for per-op guards:

```kdl
on "Done" {
    set_status text="Done" severity="info"
    close "@agent" when=#"event.outcome == "success""#
}
```

Predicates containing string literals must use KDL raw strings (`#"..."#`) because Rhai uses double quotes internally. `ark scene fmt` auto-promotes plain strings to raw when the body contains `"`.

### Execution rules

- Multiple `on` blocks with overlapping selectors each run (no silent dedup).
- `on` blocks execute in **textual order** within the scene file.
- Op failure logs `error[scene/op-failed]`; remaining ops in that reaction are skipped; the event loop continues.
- `emit` cascades are bounded at depth 4 by default.

## Keybinds (`bind` blocks)

Keybind declarations compile into zellij's `keybinds { }` block. The chord fires a `MessagePlugin` to ark-bus, which dispatches the ops.

```kdl
bind "Alt d" {
    focus "@diff"
}

bind "Alt Shift v" {
    spawn "@preview" overlay pos="center" size="80%x80%" {
        command cmd="glow" args=["README.md"]
    }
}

bind "Ctrl c" {
    acp.cancel
}
```

### Chord syntax

Chords use zellij notation -- quoted, space-separated modifiers:

| Chord | Meaning |
|---|---|
| `"Alt d"` | Alt + d |
| `"Alt Shift v"` | Alt + Shift + v |
| `"Ctrl c"` | Ctrl + c |

Key strings are validated against zellij's key chord lexer at compile time. Invalid chords are a compile error.

### Merge behavior

- Keybinds are added **without** `clear-defaults=true`, so the user's zellij config binds survive.
- **Last-wins per chord** across the scene and included fragments.
- `clear-bind "Alt d"` removes a specific inherited bind.

## Modes

Modes are named alternate whole-tab layouts. They do NOT use zellij's `swap_tiled_layout` -- ark modes are explicit, not pane-count-triggered.

```kdl
mode "review" {
    tab "@main" {
        row {
            pane "@diff" span=2 { diff }
            pane "@agent" span=1 { shell }
        }
    }
}
```

Switch modes from reactions or keybinds:

```kdl
bind "Alt r" {
    use_mode "review"
}

bind "Alt Escape" {
    use_mode "default"
}
```

Handles survive mode switches. The same `@handle` across the base layout and a mode layout preserves the running subprocess. `use_mode "default"` restores the primary layout via a full reconciliation pass.

## Composition overrides

Scenes can selectively remove inherited reactions and keybinds from included fragments:

```kdl
include "ext:claude-code/reactions"

// Remove a specific reaction inherited from the fragment
clear-reactions event="Started"

// Remove a specific keybind inherited from the fragment
clear-bind "Alt p"

// Prevent an extension from activating entirely
disable-extension "some-ext"
```
