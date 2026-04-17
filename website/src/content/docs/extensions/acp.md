---
title: "ACP Extension"
description: "How ark talks to ACP agents"
---

The ACP extension is how ark communicates with coding agents. It speaks [Agent Client Protocol](https://agentclientprotocol.com) — a JSON-RPC 2.0 standard for AI coding tools — and bridges ACP events into ark's event bus as `ark.acp.*` events.

There is one ACP extension. There are no per-agent adapters, no engine abstractions, no driver layers. Any CLI that speaks ACP works with ark.

## Supported agents

- **claude-code** — Anthropic's CLI coding agent
- **codex** — OpenAI's CLI agent
- **gemini-cli** — Google's CLI agent

## How it works

The ACP extension is compiled-in and activated by `use` in a scene:

```kdl
scene "dev" {
  use "claude-code"

  layout {
    tab "@main" focus=true {
      pane "@agent" { command cmd="claude" }
      pane "@status" { status }
    }
  }
}
```

At session start:

1. ark loads the extension and starts its protocol handler
2. The agent process launches in the designated pane
3. The ACP extension connects to the agent over JSON-RPC
4. ACP events (`session/update`, permission requests, etc.) flow into ark's event bus as `ark.acp.*` events
5. Scene reactions fire in response

## ACP events on the bus

The ACP extension maps every ACP notification into an ark event under the `ark.acp.*` namespace:

| ACP notification | ark event | Payload |
|------------------|-----------|---------|
| `session/update` (plan) | `ark.acp.plan` | Agent execution plan |
| `session/update` (agent message) | `ark.acp.agent_message_chunk` | Streamed model output |
| `session/update` (tool call) | `ark.acp.tool_call` | Tool-call request from agent |
| `session/update` (other) | `ark.acp.session_update` | Generic session update |
| `session/request_permission` | `ark.acp.permission_requested` | Tool name, request ID, options |

Scenes react to these like any other event:

```kdl
scene "dev" {
  use "claude-code"

  on "ark.acp.permission_requested" {
    set_status text="Permission requested: {event.tool}" severity="warn"
  }

  on "ark.acp.agent_message_chunk" {
    set_status text="Agent responding..." severity="info"
  }
}
```

## ACP scene ops

Four ops interact with the running agent session:

```kdl
// Send a prompt to the agent
acp.prompt text="Fix the failing test in src/lib.rs"

// Cancel the current turn
acp.cancel

// Respond to a permission request
acp.permit request_id="req-42" outcome="allow"

// Switch agent mode (plan, edit, etc.)
acp.set_mode mode="plan"
```

Valid `outcome=` values for `acp.permit`: `"allow"`, `"reject_once"`, `"reject_always"`. Any other value is a compile-time error.

All ACP ops no-op with a warning if no ACP-capable extension is active.

## Permission dispatch

When an agent requests permission (tool use, file write, etc.), the ACP extension uses a 5-tier fallback model inspired by Zed:

1. **Scene rule** — explicit `on "ark.acp.permission_requested"` reaction with an `acp.permit` op
2. **Always-allow list** — tools the user pre-approved in config
3. **Always-deny list** — tools the user blocked in config
4. **Interactive prompt** — ask the user in the terminal
5. **Default deny** — if nothing else matches, reject

Scene authors wire up their preferred policy through reactions. The interactive prompt is the safety net for anything not covered by an explicit rule.

## Extension manifest

The ACP extension declares the `agent` capability in its manifest:

```kdl
extension {
  name "claude-code"
  capabilities {
    item "agent"
  }
}
```

Any extension that declares `capabilities { item "agent" }` participates in ACP. The protocol is the same regardless of which agent backs it.
