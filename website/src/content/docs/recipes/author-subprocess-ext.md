---
title: Author a Subprocess Extension
description: Minimal NDJSON extension in Python
---

Subprocess extensions are executables that speak NDJSON (newline-delimited JSON) over stdin/stdout. Any language works — here's a minimal example in Python.

## The protocol

ark launches your extension as a subprocess. Communication is JSON-RPC 2.0 over stdin (requests from ark) and stdout (responses + events from your extension).

## Minimal Python extension

Create `my-ext.py`:

```python
#!/usr/bin/env python3
import json
import sys

def handle(request):
    method = request.get("method")
    id = request.get("id")

    if method == "initialize":
        return {"jsonrpc": "2.0", "id": id, "result": {
            "name": "my-ext",
            "version": "0.1.0",
            "capabilities": {}
        }}

    if method == "intent/handle":
        intent = request["params"]["intent"]
        return {"jsonrpc": "2.0", "id": id, "result": {
            "handled": True,
            "message": f"Handled intent: {intent}"
        }}

    return {"jsonrpc": "2.0", "id": id, "error": {
        "code": -32601, "message": f"Unknown method: {method}"
    }}

for line in sys.stdin:
    request = json.loads(line.strip())
    response = handle(request)
    print(json.dumps(response), flush=True)
```

Make it executable:

```sh
chmod +x my-ext.py
```

## Load it in a scene

```kdl
scene "with-custom-ext" {
  use "./my-ext.py"
  use "ark:acp"

  layout {
    tab "@main" focus=true {
      pane "@agent" { command "claude" }
    }
  }
}
```

## Key rules

- One JSON object per line on stdout (NDJSON)
- Respond to every request with a matching `id`
- Implement at least `initialize` — ark calls it on load
- `flush=True` (Python) or equivalent — buffered stdout breaks the protocol
- stderr is captured to the supervisor log for debugging

See [Intent Protocol](/extensions/protocol/) for the full JSON-RPC spec.
