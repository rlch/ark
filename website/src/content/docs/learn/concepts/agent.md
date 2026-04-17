---
title: Agents
description: What ark launches — ACP agent processes
---

An **agent** is the AI coding tool that ark orchestrates. Every `ark` launch creates a new zellij session running one agent process.

## What counts as an agent

Any CLI that speaks [ACP (Agent Client Protocol)](https://agentclientprotocol.com) works with ark:

- **claude-code** — Anthropic's CLI coding agent
- **codex** — OpenAI's CLI agent
- **gemini-cli** — Google's CLI agent

ark communicates with agents through its built-in **ACP extension**. The extension manages the JSON-RPC session, maps ACP events (like `session/update`) into ark's event bus as `ark.acp.*` events, and handles permission dispatch.

## Agent lifecycle

When you run `ark` (optionally with `--scene`):

1. ark's **supervisor** forks and daemonizes
2. The supervisor compiles the **scene** to determine the layout
3. A zellij session is created with the compiled layout
4. The agent process starts in the main pane
5. The ACP extension connects to the agent and begins relaying events
6. Reactions fire in response to agent events

When the agent exits (or you run `ark kill`), the supervisor tears down the session and writes final state.

## Agent identity

Each launched agent gets an `AgentId` — a namespaced identifier like `claude/my-project`. The ID is used for:

- State directory path (`$XDG_STATE_HOME/ark/<id>/`)
- Control socket name
- The `ark list` / `ark kill` / `ark scene reload --session` commands

## No adapter pattern

ark does not have per-agent adapters, engine abstractions, or driver layers. Every ACP-speaking agent connects through the same ACP extension. If your agent speaks ACP, it works — no ark-side code needed.
