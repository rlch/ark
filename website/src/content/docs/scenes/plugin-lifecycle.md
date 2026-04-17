---
title: "Plugin Lifecycle"
description: "Diff-based lifecycle management"
---

Extensions are activated by `use` declarations in a scene. This page covers how ark manages extension lifecycle across scene loads and reloads.

## Activation

Extensions are **lazy** -- they load only when a scene `use`s them. A `use` declaration brings the extension's views, intents, and events into scope:

```kdl
scene "dev" {
    use "claude-code"
    use "status"

    layout {
        tab "@main" focus=true {
            pane "@agent" { claude-code }
            pane "@bar"   { status }
        }
    }
}
```

## Resolution order

`use "name"` resolves by scanning three locations in order (first match wins):

1. **Compiled-in** -- Rust extensions auto-registered at boot via `inventory`/`linkme`.
2. **User-installed** -- `~/.local/share/ark/extensions/<name>/`.
3. **Project-local** -- `.ark/extensions/<name>/`.

Missing extensions produce `error[ext/missing]` with Levenshtein-distance suggestions.

## Delivery modes

Extensions have three delivery modes, all using an identical manifest format:

| Mode | Transport | Manifest |
|---|---|---|
| **compiled-in** | In-process trait dispatch | Code-generated from `#[derive(Extension)]` |
| **subprocess** | NDJSON JSON-RPC over unix socket | Hand-written `extension.kdl` alongside binary |
| **wasm** | Zellij plugin runtime, pipe through ark-bus | Embedded as `ark.metadata` custom section |

The delivery mode is transparent to scene authors -- `use "name"` works regardless.

## Diff model on reload

When a scene reloads (via `reload_scene` op or `ark scene reload`), ark diffs the old and new extension sets. Four cases:

### 1. Add (new `use` in reloaded scene)

The extension is freshly activated:
- Resolution scan runs.
- Protocol handler starts (subprocess) or loads (compiled-in/wasm).
- Views become available for layout rendering.
- Extension events and intents enter scope.

### 2. Remove (`use` deleted from reloaded scene)

The extension is deactivated:
- Subprocess extensions receive `shutdown` RPC, then the 2s/SIGTERM/SIGKILL supervision sequence.
- Views from this extension are removed from the registry.
- Panes rendering removed views are closed by the reconciler on the next layout override.
- Subscriptions for extension-scoped events are dropped.

### 3. Update (same `use`, config changed)

The extension stays alive. Ark sends `workspace/configuration` with the new config values. The extension handles the delta internally.

```kdl
// Before reload
use "claude-code" config {
    model "sonnet"
}

// After reload -- extension receives new config, no restart
use "claude-code" config {
    model "opus"
}
```

### 4. No change (same `use`, same config)

No action. The extension continues running undisturbed.

## Scene fragments

Extensions may ship scene fragments (reactions, keybinds, layout snippets). Fragments are **not** auto-merged on `use`. The scene author opts in explicitly:

```kdl
use "claude-code"
include "ext:claude-code/reactions"
include "ext:claude-code/keybinds"
```

`ark ext info <name>` lists available fragments for an extension.

## Supervision

Subprocess extensions follow this shutdown sequence:

1. Close stdin.
2. Wait 2 seconds.
3. Send SIGTERM.
4. Send SIGKILL.

A crash emits `error[ext/crashed]` on the event bus. The extension is not auto-restarted -- the scene author can react to the crash event if desired.

## Agent as extension capability

ACP (Agent Client Protocol) is not a special layer. Any extension can speak ACP by declaring the capability in its manifest:

```kdl
capabilities {
    agent {
        speaks "acp"
    }
}
```

The scene activates it with a plain `use`. ACP events emit as `ark.acp.*` on the bus -- any ACP-speaking extension emits there.
