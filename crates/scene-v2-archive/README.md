# ark-scene

Reactive KDL configuration for ark — the user-facing layer that
declares zellij layout, plugin lifecycle, reactions, keybinds, and
extension composition for each `ark spawn`. Preprocessed superset of
zellij's layout KDL: the `layout { … }` body stays
zellij-parseable; everything else (`on`, `keybind`, `plugin`, `use`,
`extends`, `include`) is ark-native and compiles into runtime
artefacts (rendered zellij layout, reaction registry, plugin
lifecycle manifest, ACP engine spec).

## What is a scene?

A scene file is a single `scene "<name>" { … }` KDL document. It is
the one configuration artefact a user writes by hand. Parsed at
`ark spawn`, validated, and lowered into the runtime registries the
supervisor consumes.

## One-screen example

```kdl
scene "default" {
    layout {
        tab "agent" {
            pane name="agent" command="claude"
        }
    }

    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }

    on "Started" {
        set_status text="agent up"
    }

    keybind "Alt p" intent="ark.core.show_picker"
}
```

## CLI surface

```sh
ark spawn --scene NAME -- claude              # rung 1: --scene NAME
ARK_SCENE=NAME  ark spawn -- claude           # rung 2: env var
ARK_APPNAME=ark ark spawn -- claude           # XDG appname override
```

`ark scene check`, `ark scene fmt`, `ark scene graph`, `ark scene
explain`, and `ark scene dry-run` are specified in
`cavekit-scene.md` R13 and arrive in T-12.x; until then the scene
parser surfaces every error with full miette span + caret via the
`ark spawn` failure path.

## Path precedence

Resolved by `ark_scene::path::resolve_scene_path` (T-8.0). First
match wins:

1. `--scene NAME` flag → `${CONFIG}/scenes/NAME.kdl`
2. `ARK_SCENE=NAME`    → `${CONFIG}/scenes/NAME.kdl`
3. `./.ark/scene.kdl`  (project-local)
4. `${XDG_CONFIG_HOME}/<appname>/scenes/default.kdl`
   (`<appname>` defaults to `ark`; override via `ARK_APPNAME=…`)
5. Built-in default compiled into the binary
   (`crates/scene/src/default_scene.kdl`)

Rules 1 + 2 surface a name (caller resolves to a path). Rules 3 + 4
require the file to exist on disk. Rule 5 is always available.

## ACP engine resolution (R17)

Every `ark spawn` resolves exactly one ACP engine launch spec,
walking these rungs in order and taking the first match:

1. `--engine NAME` CLI flag
2. Scene `engine { }` block
3. `use "engine-*"` extension with an `agent { engine { speaks "acp" } }`
   capability
4. `[engines.<name>]` in `config.toml` (keyed by `defaults.engine`)
5. Hardcoded default: `claude --acp`

A scene with both an inline `engine { }` block AND a `use
"engine-*"` extension trips `error[scene/engine-conflict]` (R17).

### Shipped engine launch specs (T-ACP.8)

Three engine names resolve without any `[engines.<name>]` block in
config — they ship baked into the supervisor:

| name         | argv                  |
|--------------|-----------------------|
| `claude`     | `claude --acp`        |
| `codex`      | `codex --acp`         |
| `gemini-cli` | `gemini --acp`        |

Override the argv by adding an `[engines.<name>]` block to
`config.toml`:

```toml
[engines.claude]
command = "claude"
args    = ["--acp", "--verbose"]

[engines.claude.env]
ANTHROPIC_API_KEY = "…"
```

Or declare an engine inline in a scene:

```kdl
scene "custom" {
    engine {
        name    "claude"
        command "claude"
        args    "--acp"
        env {
            ANTHROPIC_MODEL "claude-opus-4-5"
        }
    }
    # …
}
```

## Spec

`../../context/kits/cavekit-scene.md` (R1–R17) is authoritative.
