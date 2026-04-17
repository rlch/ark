---
title: Pin Agent Version
description: Lock an agent to a specific binary path
---

By default, ark launches whatever `claude` is on your `PATH`. To pin a specific
build, override the engine's launch spec in your user config.

## Pin via `config.toml`

The `[engines.<name>]` section in `$XDG_CONFIG_HOME/ark/config.toml` defines
how an ACP engine is launched. Set `command` to an absolute path:

```toml
[engines.claude]
command = "/usr/local/bin/claude-3.2.1"
args    = []

[engines.claude.env]
# Optional extra env for this engine
ANTHROPIC_MODEL = "claude-sonnet-4-5"
```

This applies to every session launched with the default scene. Launch with:

```sh
ark                         # default session, pinned claude
ark --scene myproject       # any scene that activates `ark:acp`
```

## Per-project override

Drop a `.ark/config.toml` at your project root with the same shape. Project
config layers on top of user config:

```toml
# .ark/config.toml
[engines.claude]
command = "/opt/claude-nightly/bin/claude"
```

Any `ark` invocation from that directory picks up the override.

## Pin via environment variable

Any config key can be overridden with the `ARK_` prefix and `__` as a section
separator:

```sh
ARK_ENGINES__CLAUDE__COMMAND=/usr/local/bin/claude-3.2.1 ark
```

Use this for one-shot runs without touching the config file.

## Verify the pin

```sh
ark config get engines.claude.command
ark doctor                  # reports the resolved claude binary
```
