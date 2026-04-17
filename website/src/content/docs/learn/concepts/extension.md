---
title: Extensions
description: Three delivery modes and the intent protocol
---

An **extension** is a bundle that provides views, intents, and events to an ark session. Extensions are loaded by `use` statements in a scene and communicate with ark over a single JSON-RPC 2.0 protocol (the **intent protocol**).

## Three delivery modes

Extensions can be delivered in three ways:

| Mode | Language | Transport | Best for |
|------|----------|-----------|----------|
| **Compiled-in** | Rust | In-process function calls | Built-in extensions (status, picker, ACP) |
| **Subprocess** | Any | NDJSON over stdio | Third-party integrations, scripts |
| **WASM component** | Any → WASI p2 | Host-embedded runtime | Sandboxed, portable extensions |

All three modes share the same intent protocol — an extension doesn't know or care how it's delivered.

## The ACP extension

The most important built-in extension is `ark:acp`. It speaks [Agent Client Protocol](https://agentclientprotocol.com) to connect ark with coding agents (claude-code, codex, gemini-cli). It:

- Manages the JSON-RPC session with the agent
- Maps ACP events (`session/update`, etc.) into ark events (`ark.acp.*`)
- Handles permission dispatch via Zed's 5-tier fallback model

## Loading extensions

Extensions are loaded in a scene with `use`:

```kdl
scene "example" {
  use "ark:status"          // compiled-in
  use "ark:acp"             // compiled-in
  use "./my-ext"            // subprocess (local path)
  use "community:lint-pane" // WASM from registry (future)
}
```

## Capabilities and trust

Extensions declare capabilities in their manifest. On first install, ark prompts the user to review and approve the requested capabilities. An audit log records all capability grants.

See [Capabilities](/extensions/capabilities/) for the trust model.
