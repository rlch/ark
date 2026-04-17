---
title: Configuration
description: ark.kdl schema and figment layering
---

ark uses a layered configuration system powered by [figment](https://crates.io/crates/figment).

## Layering precedence

Config is resolved by merging sources from lowest to highest precedence:

1. **Compiled-in defaults** — shipped with the binary
2. **User config** — `$XDG_CONFIG_HOME/ark/config.toml`
3. **Project config** — `.ark/config.toml` in the working directory
4. **Environment variables** — `ARK_*` keys
5. **CLI flags** — highest priority

Higher layers only override keys they set. Unset keys fall through to lower layers.

## Config file location

```
~/.config/ark/config.toml        # user config
.ark/config.toml                  # project config (per-repo)
```

Missing files are silently skipped. Malformed files produce errors with line/column information.

## Schema

### `[defaults]`

```toml
[defaults]
session_prefix = "ark"
auto_close_on_done = true
auto_close_on_fail = false
stall_timeout_secs = 120
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `session_prefix` | string | `"ark"` | Prefix for zellij session names |
| `auto_close_on_done` | bool | `true` | Close session when agent finishes successfully |
| `auto_close_on_fail` | bool | `false` | Close session on agent failure |
| `stall_timeout_secs` | int | `120` | Seconds of inactivity before marking agent as stalled |

### `[diff]`

```toml
[diff]
command = "delta --paging=never --side-by-side --line-numbers"
debounce_ms = 300
```

### `[mux.zellij]`

```toml
[mux.zellij]
status_plugin_path = "~/.config/zellij/plugins/ark-status.wasm"
picker_plugin_path = "~/.config/zellij/plugins/ark-picker.wasm"
default_layout_dir = "~/.config/ark/layouts"
```

### `[acp]`

```toml
[acp]
permission_timeout_ms = 300000  # 5 minutes, interactive
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `permission_timeout_ms` | int | `300000` | Milliseconds to wait for interactive permission approval before auto-denying |

### `[engines.<name>]`

```toml
[engines.claude]
command = "claude"
args = []
env = {}
```

ACP engine launch spec. Each named section under `[engines]` defines how to launch a specific engine binary. Used as the resolution source in the engine selection chain (after `--engine` flag and scene `engine {}` block).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `command` | string | — | Engine binary to invoke |
| `args` | array | `[]` | Additional arguments prepended to every invocation |
| `env` | map | `{}` | Environment variables injected into the engine process |

### `[scene]`

```toml
[scene]
watch = false
watch_debounce_ms = 200
```

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `watch` | bool | `false` | Enable file-system watching of the active scene file |
| `watch_debounce_ms` | int | `200` | Milliseconds to wait after a file change before triggering a reload |

### `[[hooks]]`

```toml
[[hooks]]
event = "AgentDone"
command = "notify-send 'Agent finished'"
```

Hooks are user-defined commands fired on event matches. The `event` field is an event kind; the `command` runs in a shell.

## Environment variables

Any config key can be overridden via environment variable with the `ARK_` prefix and `__` as a section separator:

```sh
ARK_DEFAULTS__STALL_TIMEOUT_SECS=300 ark --scene myproject
```

See [CLI Reference](/reference/cli/) for the full list of environment variables.
