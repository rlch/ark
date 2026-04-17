---
title: "Overview"
description: "Three delivery modes sharing one protocol"
---

Everything in ark is an extension. The status bar, the file picker, and the ACP agent bridge are all extensions — they use the same manifest format, the same intent protocol, and the same lifecycle as anything you install from a third party.

## What extensions provide

An extension contributes any combination of:

- **Views** — content that fills a pane (a terminal command, a zellij wasm plugin)
- **Intents** — named operations that scenes can dispatch (e.g. `status-lite.set_icon`)
- **Events** — typed payloads published on the event bus (e.g. `status-lite.icon_changed`)
- **Scene fragments** — optional KDL snippets (reactions, keybinds, layout pieces) that a scene author can opt into via `include`

## Three delivery modes

| Mode | Language | Transport | Best for |
|------|----------|-----------|----------|
| **Compiled-in** | Rust | In-process trait dispatch | Built-in extensions shipped with ark |
| **Subprocess** | Any | NDJSON JSON-RPC over unix socket | Third-party tools, scripts, polyglot |
| **WASM** | Any (compiled to WASI) | Zellij plugin runtime, piped through ark-bus | Sandboxed, portable plugins |

All three modes speak the same [intent protocol](/extensions/protocol/). An extension does not know or care how it is delivered.

## Resolution order

When a scene declares `use "name"`, ark resolves the extension by scanning:

1. **Compiled-in registry** — auto-registered at boot via `inventory`/`linkme`
2. **User-installed** — `~/.local/share/ark/extensions/<name>/`
3. **Project-local** — `.ark/extensions/<name>/`

First match wins. A missing extension produces `error[ext/missing]` with Levenshtein suggestions.

## Activation

Extensions are lazy. An extension is loaded only when a scene `use`s it. No `use`, no startup cost.

```kdl
scene "my-session" {
  use "claude-code"    // ACP agent — compiled-in
  use "status"         // status bar — compiled-in
  use "my-linter"      // custom subprocess extension
}
```

## Scene fragments

Extensions can ship scene fragments — pre-built reactions, keybinds, or layout snippets. Fragments are never auto-merged. The scene author opts in explicitly:

```kdl
scene "my-session" {
  use "claude-code"
  include "ext:claude-code/default-keybinds"
}
```

Run `ark ext info <name>` to list an extension's available fragments.

## Next steps

- [Intent protocol](/extensions/protocol/) — the JSON-RPC 2.0 contract between ark and extensions
- [ACP extension](/extensions/acp/) — how ark talks to coding agents
- [Authoring guide](/extensions/authoring/compiled-in/) — write your own extension
- [Capabilities](/extensions/capabilities/) — trust model and audit log
- [Extension CLI](/extensions/cli/) — install, inspect, and manage extensions
