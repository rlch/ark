---
title: "Intent Protocol"
description: "JSON-RPC 2.0 intent API specification"
---

The intent protocol is the JSON-RPC 2.0 contract between ark core and every running extension. The same message shapes apply across all three delivery modes — only the transport differs (in-process calls, unix socket, zellij pipe).

## Method surface

### Lifecycle

| Method | Direction | Purpose |
|--------|-----------|---------|
| `initialize` | ark -> ext | Version negotiation + capability exchange |
| `initialized` | ext -> ark | Acknowledgement; extension is ready |
| `shutdown` | ark -> ext | Graceful teardown request |
| `ping` | ark -> ext | Liveness check |

Version negotiation uses a dual scheme: a semver `protocolVersion` field plus capability flags for fine-grained feature detection.

### Events

| Method | Direction | Purpose |
|--------|-----------|---------|
| `event/subscribe` | ext -> ark | Register interest in an event pattern |
| `event/unsubscribe` | ext -> ark | Remove a subscription |
| `event/emit` | ext -> ark | Publish an event on the bus |
| `event/notify` | ark -> ext | Deliver a matched event to the extension |

Extensions can only emit events in their own namespace. Subscriptions are open — any extension can listen to any event.

### Intents

| Method | Direction | Purpose |
|--------|-----------|---------|
| `intent/register` | ext -> ark | Declare a callable intent |
| `intent/unregister` | ext -> ark | Remove an intent |
| `intent/dispatch` | ark -> ext | Execute a registered intent |

Intent names are namespaced: `<ext-name>.<intent>`. The reserved `ark.core.*` prefix is blocked for extension registrations (`error[ext/reserved-namespace]`).

### UI

| Method | Direction | Purpose |
|--------|-----------|---------|
| `ui/keybind/register` | ext -> ark | Add a runtime keybind |
| `ui/keybind/unregister` | ext -> ark | Remove a keybind |
| `ui/status/push` | ext -> ark | Push a status bar message |

### Workspace

| Method | Direction | Purpose |
|--------|-----------|---------|
| `workspace/applyEdit` | ext -> ark | Apply a workspace edit |
| `workspace/configuration` | ark -> ext | Push configuration to the extension |
| `workspace/showMessage` | ext -> ark | Display a message to the user |

## Request/response shape

All messages follow [JSON-RPC 2.0](https://www.jsonrpc.org/specification):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "intent/dispatch",
  "params": {
    "name": "status-lite.set_icon",
    "args": { "icon": "check" }
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": { "ok": true }
}
```

Notifications (no `id`) are used for `event/notify` and `initialized`.

## Timeouts and heartbeats

Every call has a 5-second default timeout. Extensions that need longer can send `$/progress` heartbeat notifications to extend the deadline. A heartbeat resets the timeout clock.

## Supervision

Subprocess extensions follow a strict shutdown sequence:

1. ark closes the extension's stdin
2. 2-second grace period
3. `SIGTERM`
4. `SIGKILL`

If a subprocess crashes, ark emits `error[ext/crashed]` on the event bus and logs the exit code. Compiled-in extensions run in-process and cannot crash independently.

## Transport by delivery mode

| Mode | Transport | Notes |
|------|-----------|-------|
| Compiled-in | In-process trait dispatch | Zero serialization overhead |
| Subprocess | NDJSON over unix socket | One JSON object per line, newline-delimited |
| WASM | Zellij pipe through ark-bus | Messages routed through the ark-bus zellij plugin |
