---
title: "Subprocess Extensions"
description: "NDJSON stdio contract"
---

A subprocess extension is a standalone binary that speaks the [intent protocol](/extensions/protocol/) over NDJSON (newline-delimited JSON) on a unix socket. Write it in any language.

## File layout

```
~/.local/share/ark/extensions/my-ext/
  extension.kdl    # manifest (hand-written)
  my-ext           # executable binary or script
```

## Manifest

Subprocess extensions ship a hand-written `extension.kdl` alongside the binary:

```kdl
extension {
  name "my-ext"
  version "0.1.0"
  ark-range ">=1.0, <2.0"
  zellij-range ""

  intent "my-ext.greet" {
    args-schema "{\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\"}},\"required\":[\"name\"]}"
  }

  event "my-ext.greeted" {
    payload-schema "{\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\"}}}"
  }

  item "pipe"
}
```

The manifest format is identical to compiled-in and WASM extensions. See [Capabilities](/extensions/capabilities/) for the declared capability vocabulary.

## Protocol

Communication uses JSON-RPC 2.0, one JSON object per line. ark manages the unix socket — the extension reads from stdin and writes to stdout.

### Initialization handshake

ark sends `initialize`, the extension responds, then sends `initialized`:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1.0.0"}}
```

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"1.0.0","capabilities":{}}}
```

```json
{"jsonrpc":"2.0","method":"initialized"}
```

### Handling an intent dispatch

When a scene dispatches one of the extension's intents, ark sends `intent/dispatch`:

```json
{"jsonrpc":"2.0","id":2,"method":"intent/dispatch","params":{"name":"my-ext.greet","args":{"name":"world"}}}
```

The extension processes the request and responds:

```json
{"jsonrpc":"2.0","id":2,"result":{"message":"Hello, world!"}}
```

### Emitting events

The extension can emit events at any time:

```json
{"jsonrpc":"2.0","method":"event/emit","params":{"name":"my-ext.greeted","payload":{"name":"world"}}}
```

## Python example

```python
#!/usr/bin/env python3
"""Minimal ark subprocess extension."""
import json
import sys

def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()

def handle(msg):
    method = msg.get("method")
    msg_id = msg.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg_id, "result": {
            "protocolVersion": "1.0.0", "capabilities": {}
        }})
        send({"jsonrpc": "2.0", "method": "initialized"})

    elif method == "intent/dispatch":
        name = msg["params"]["name"]
        args = msg["params"].get("args", {})
        if name == "my-ext.greet":
            result = {"message": f"Hello, {args['name']}!"}
            send({"jsonrpc": "2.0", "id": msg_id, "result": result})
            # Emit a follow-up event
            send({"jsonrpc": "2.0", "method": "event/emit", "params": {
                "name": "my-ext.greeted", "payload": {"name": args["name"]}
            }})

    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": msg_id, "result": None})
        sys.exit(0)

    elif method == "ping":
        send({"jsonrpc": "2.0", "id": msg_id, "result": "pong"})

for line in sys.stdin:
    handle(json.loads(line.strip()))
```

## Lifecycle

ark launches the protocol handler process when the scene `use`s the extension. If the extension also provides a view, the view command runs separately in a zellij pane — the protocol handler and view process are two components under one name.

### Shutdown sequence

1. ark closes the extension's stdin
2. 2-second grace period
3. `SIGTERM`
4. `SIGKILL`

If the process crashes, ark emits `error[ext/crashed]` on the event bus.

## Using in a scene

```kdl
scene "dev" {
  use "my-ext"

  on "my-ext.greeted" {
    set_status text="Greeted {event.name}" severity="info"
  }

  bind "Alt g" {
    my-ext.greet name="world"
  }
}
```
