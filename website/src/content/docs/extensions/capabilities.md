---
title: "Capabilities"
description: "Trust model and audit log"
---

Extensions declare what system access they need. ark enforces these declarations at install time and at runtime, and logs every capability grant for auditability.

## Capability vocabulary

| Capability | Grants |
|------------|--------|
| `exec` | Spawn subprocesses |
| `fs-read` | Read files outside the extension's install directory |
| `fs-write` | Write files outside the extension's install directory |
| `pipe` | Send pipe messages to other zellij panes/plugins |
| `network` | Open outbound TCP/UDP/HTTP sockets |
| `hook` | Register scene reactions (event hooks) |

Extensions declare capabilities in their manifest as `item` entries:

```kdl
extension {
  name "my-ext"
  version "1.0.0"
  ark-range ">=1.0, <2.0"
  zellij-range ""

  item "fs-read"
  item "exec"
}
```

## Trust model

### Install-time prompt

When you run `ark ext add`, ark reads the extension's manifest and prompts you to approve the requested capabilities before installation completes:

```
my-ext v1.0.0 requests the following capabilities:
  - fs-read   Read files outside install directory
  - exec      Spawn subprocesses

Allow? [y/N]
```

Compiled-in extensions (shipped with ark) are implicitly trusted. The prompt applies to subprocess and WASM extensions installed from external sources.

### Runtime enforcement

An extension that attempts an operation outside its declared capabilities is blocked. For example, a WASM extension that does not declare `network` cannot open sockets — the runtime sandbox prevents it.

### Unknown capabilities

New capability names may be added in future ark releases. If an extension declares a capability that the current ark version does not recognize:

- `ark ext inspect` emits `warning[ext/unknown-capability]`
- `ark scene check --v1-strict` upgrades the warning to an error

This allows forward compatibility — newer extensions can declare newer capabilities, and older ark versions gracefully warn rather than fail.

## Audit log

Every capability grant is recorded in the audit log at `~/.local/share/ark/audit.log`. Each entry includes:

- Timestamp
- Extension name and version
- Capabilities granted
- Who approved (user or implicit trust)

The audit log is append-only. It provides a clear trail for security review: which extensions have access to what, and when they were approved.

## Updating capabilities

When an extension update adds new capabilities, ark re-prompts on next load:

```
my-ext upgraded from 1.0.0 to 1.1.0
New capabilities requested:
  - network   Open outbound sockets

Allow? [y/N]
```

Previously granted capabilities carry forward. Only new capabilities require approval.

## Revoking access

Remove capability grants by reinstalling the extension or removing it entirely:

```bash
ark ext remove my-ext
ark ext add path:./my-ext   # re-prompts for capabilities
```
