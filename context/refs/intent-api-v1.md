---
created: "2026-04-16"
last_edited: "2026-04-16"
status: frozen — v1.0
---

# ark.core.* Intent API — v1.0 (frozen)

> This document describes the frozen `ark.core.*` intent surface that
> ships with ark v1.0. Once v1.0 is cut, the shapes below are
> compatibility-frozen for the entire 1.x line under the contract in
> §"Version compatibility contract".
>
> Sources of truth:
> - Op implementations: `crates/scene/src/ops/` (tabs, panes, plugins,
>   messaging, control, acp).
> - Typed args structs: `#[derive(Facet)]` on each `<Op>Args` (facet
>   SHAPE reflection is the canonical schema).
> - Registry: `crates/scene/src/ops/mod.rs` — `CORE_OP_NAMES`.

## Scope

v1.0 freezes **17 ops across 6 subject groups** under the
`ark.core.*` namespace. Four of those are ACP-interaction ops
(`prompt`, `acp_cancel`, `acp_permit`, `set_mode`); the other 13 are
the "classic" scene ops. This document covers all 17 — they share one
compatibility contract and one freeze event.

Extensions contribute ops under their own namespace (e.g.
`my-ext.do_thing`); extension-authored ops are NOT subject to this
freeze — their stability is the extension author's concern. The scene
compiler rejects extension registrations that land inside the reserved
`ark.core.*` prefix (`error[ext/reserved-namespace]`).

## Op vocabulary at a glance

| Group     | Op                           | KDL verb          | Idempotency                    |
|-----------|------------------------------|-------------------|--------------------------------|
| tabs      | `ark.core.open_tab`          | `open_tab`        | if-absent-focus-else-create    |
| tabs      | `ark.core.close_tab`         | `close_tab`       | idempotent-noop-on-absent      |
| tabs      | `ark.core.rename_tab`        | `rename_tab`      | idempotent-noop-on-absent      |
| tabs      | `ark.core.focus_tab`         | `focus_tab`       | idempotent-noop-on-absent      |
| panes     | `ark.core.split_pane`        | `split_pane`      | always-side-effect             |
| panes     | `ark.core.close_pane`        | `close_pane`      | idempotent-noop-on-absent      |
| plugins   | `ark.core.mount_plugin`      | `mount_plugin`    | launch-or-focus                |
| plugins   | `ark.core.unmount_plugin`    | `unmount_plugin`  | idempotent-noop-on-absent      |
| messaging | `ark.core.pipe`              | `pipe`            | always-side-effect             |
| messaging | `ark.core.emit`              | `emit`            | always-side-effect             |
| messaging | `ark.core.set_status`        | `set_status`      | always-side-effect             |
| control   | `ark.core.exec`              | `exec`            | always-side-effect             |
| control   | `ark.core.reload_scene`      | `reload_scene`    | idempotent-noop-on-absent      |
| acp       | `ark.core.prompt`            | `prompt`          | always-side-effect             |
| acp       | `ark.core.acp_cancel`        | `acp_cancel`      | always-side-effect             |
| acp       | `ark.core.acp_permit`        | `acp_permit`      | always-side-effect             |
| acp       | `ark.core.set_mode`          | `set_mode`        | always-side-effect             |

Scene authors write the short KDL verb (`open_tab`). The registry
dispatches through the fully-qualified `ark.core.*` name; both forms
are frozen.

## Op schemas

Each entry lists the KDL shape, every arg (type + required/optional),
the return value, and side effects. Defaults and enum values come
straight from the `<Op>Args` struct's `#[derive(Facet)]` definition.

### `ark.core.open_tab`

```kdl
open_tab name=<str> [layout=<str>] [focus=<bool>]
```

| Arg      | Type    | Req | Notes                                                   |
|----------|---------|-----|---------------------------------------------------------|
| `name`   | string  | yes | Tab name. Matched against existing tabs first.          |
| `layout` | string  | no  | Zellij layout name to apply when CREATING a new tab.    |
| `focus`  | bool    | no  | Focus tab after create/focus. Default: true (R7).       |

**Returns:** `None`. **Side effects:** creates or focuses a tab.

### `ark.core.close_tab`

```kdl
close_tab (name=<str> | index=<int>)
```

Exactly one of `name=` / `index=` required. **Returns:** `None`.

### `ark.core.rename_tab`

```kdl
rename_tab (name=<str> | index=<int>) to=<str>
```

Exactly one of `name=` / `index=`. `to=` required. **Returns:** `None`.

### `ark.core.focus_tab`

```kdl
focus_tab (name=<str> | index=<int>)
```

Exactly one of `name=` / `index=`. **Returns:** `None`.

### `ark.core.split_pane`

```kdl
split_pane into=<str> side=<"left"|"right"|"up"|"down"> [size=<str>] {
    command "<shell-cmd>"?
    cwd "<path>"?
}
```

| Arg       | Type    | Req | Notes                                                 |
|-----------|---------|-----|-------------------------------------------------------|
| `into`    | string  | yes | Cross-referenced against declared `layout { tab … }`. |
| `side`    | enum    | yes | One of `left`, `right`, `up`, `down`.                 |
| `size`    | string  | no  | Percent (`"50%"`) or cell count (`"40"`).             |
| `command` | child   | no  | Shell command positional arg on a `command` child.    |
| `cwd`     | child   | no  | Working directory on a `cwd` child.                   |

**Returns:** `None`. **Side effects:** creates a new pane.

### `ark.core.close_pane`

```kdl
close_pane (id=<str> | selector=<str>)
```

Exactly one of `id=` / `selector=`. **Returns:** `None`.

### `ark.core.mount_plugin`

```kdl
mount_plugin name=<str> [at=<str>] [into=<str>]
```

| Arg    | Type   | Req | Notes                                                      |
|--------|--------|-----|------------------------------------------------------------|
| `name` | string | yes | Cross-referenced against `plugin "<name>" { }` blocks.     |
| `at`   | string | no  | Override declared mount target (status-bar/floating/…).    |
| `into` | string | no  | Named pane slot.                                           |

**Returns:** `None`. **Side effects:** delegates to zellij
`launch-or-focus-plugin`.

### `ark.core.unmount_plugin`

```kdl
unmount_plugin name=<str>
```

**Returns:** `None`.

### `ark.core.pipe`

```kdl
pipe plugin=<str> [severity=<str>] [name=<str>] {
    text "<content>"   OR
    json "<content>"
}
```

Exactly one of `text` / `json` child required. `severity` must be one
of `info`, `warn`, `error`, `debug` when present. **Returns:** `None`.

### `ark.core.emit`

```kdl
emit "<user-event-name>" {
    json "<payload-json>"?
}
```

| Arg    | Type   | Req | Notes                                          |
|--------|--------|-----|------------------------------------------------|
| `name` | string | yes | Positional arg. Dotted, namespaced form.       |
| `json` | child  | no  | JSON string. Missing ⇒ payload is JSON `null`. |

**Returns:** the parsed payload `Value`. **Side effects:** publishes
`AgentEvent::UserEvent { name, payload, source: "scene" }` on the
bus.

### `ark.core.set_status`

```kdl
set_status text=<str> [severity=<str>] [ttl_ms=<int>]
```

Sugar over `pipe plugin="ark-status"`. `severity` enum matches `pipe`.
**Returns:** `None`.

### `ark.core.exec`

```kdl
exec script=<str> [shell=<str>] [timeout_ms=<int>] [cwd=<str>] {
    env {
        var name="<NAME>" value="<VALUE>"*
    }?
}
```

| Arg          | Type   | Req | Default         | Notes                                   |
|--------------|--------|-----|-----------------|-----------------------------------------|
| `script`     | string | yes |                 | Runtime-template-rendered before exec.  |
| `shell`      | string | no  | `"sh"`          | Shell interpreter.                      |
| `timeout_ms` | u64    | no  | `30_000`        | Exceeding surfaces as `op/failed`.      |
| `cwd`        | string | no  | process cwd     | Working directory.                      |
| `env`        | block  | no  | empty           | `var name="X" value="Y"` entries.       |

**Returns:**

```json
{
    "exit_code": <i32>,
    "success":   <bool>,
    "stdout":    "<utf8-lossy>",
    "stderr":    "<utf8-lossy>"
}
```

**Side effects:** spawns a subprocess via `tokio::process::Command`.

### `ark.core.reload_scene`

```kdl
reload_scene
```

No args. Single-slot re-entry guard + turn-inflight gate (R14). Returns
a JSON object describing the applied/queued/dropped outcome (see
`crates/scene/src/reload.rs::ReloadOutcome`). The op lowers a
scene re-parse through the installed `SceneReloader`.

### `ark.core.prompt`

```kdl
prompt text=<str>
```

ACP op #14. Runtime-template-rendered `text`. Returns
`{ "jsonrpc_id": "<str>", "session_id": "<str>" }`. Response lands
asynchronously on the `ark.acp.*` event stream.

### `ark.core.acp_cancel`

```kdl
acp_cancel
```

ACP op #15. No args. 5-second dispatch timeout. **Returns:** `None`.

### `ark.core.acp_permit`

```kdl
acp_permit request_id=<str> outcome=<"selected"|"cancelled"> [option_id=<str>]
```

ACP op #16. `outcome="selected"` requires `option_id=`;
`outcome="cancelled"` forbids it. Unknown `request_id` (resolved,
timed out, or never issued) is silently dropped with a debug log —
scene reactions fanning out to multiple response paths must not
surface spurious `op/failed` diagnostics per T-ACP.5b.

### `ark.core.set_mode`

```kdl
set_mode mode=<str>
```

ACP op #17. `mode` is scope-checked at scene compile time against the
engine's advertised `AgentCapabilities::modes`. **Returns:** `None`.

## Shared error shapes

All ops share a uniform error surface via `IntentError`:

| Variant               | Code         | When                                            |
|-----------------------|--------------|-------------------------------------------------|
| `ArgsInvalid { .. }`  | n/a          | facet-kdl rejected the args at deserialize.     |
| `Failed { name, .. }` | `op/failed`  | Dispatch-time invariant violation (e.g. both `name=` and `index=` on `close_tab`). |

## Version compatibility contract

v1.0 is the freeze point. For the entire 1.x line:

1. **New ops** may be added to `ark.core.*` in a MINOR bump. Scenes
   written against older 1.x versions continue to compile (they just
   don't use the new op).
2. **New optional args** may be added to an existing op's `<Op>Args`
   struct in a MINOR bump, subject to these rules:
   - The new field MUST be `Option<T>` or carry `#[facet(default)]`.
   - Omitting the field MUST preserve the pre-existing behaviour
     exactly. A new arg that silently changes behaviour when absent
     is a MAJOR change.
   - The field name MUST NOT collide with any reserved name already
     in the op's schema.
3. **Removing** or **renaming** an existing arg is a MAJOR change.
   Save it for 2.0.
4. **Changing the type** of an existing arg (`int` → `string`, `bool`
   → enum, required → optional with different default, etc.) is a
   MAJOR change.
5. **Idempotency class** of an op is part of the contract. Flipping
   `always-side-effect` → `idempotent-noop-on-absent` (or any other
   transition) is a MAJOR change — scene reactions depend on the
   class for cascade behaviour.
6. **Return-value shape** is frozen. Callers (cascading reactions,
   `ark scene explain`, the event-bus payload for
   `UserEvent:ark.scene.reloaded`, etc.) key off field names.
   Adding a new field is MINOR; removing or renaming is MAJOR.
7. **Error-code strings** (`op/failed`, `op/unresolved-ref`, …) are
   frozen. Adding a new code is MINOR; changing a code's string form
   is MAJOR.

## Non-goals / explicit non-freeze

The following are explicitly NOT frozen under this document:

- **Implementation internals.** The TODO(T-5.x) stub placeholders in
  `crates/scene/src/ops/` lower to real mux handles over 1.x; the op
  contract does not depend on how the lowering is implemented.
- **Extension-contributed ops.** Only `ark.core.*` is frozen. Extensions
  own their own compatibility policy — see `cavekit-scene.md` R16.
- **Unstable ACP surface.** Per R17, ACP ops beyond the four frozen
  here (`session/fork`, `nes/*`, `elicitation/*`, …) are NOT in the
  v1 vocabulary. They land as capability-flagged extensions, not core
  ops, and each is a separate follow-up task.
- **Runtime-template strings.** The template engine (minijinja) and
  context shape are frozen separately via R9; this document references
  them ("runtime-template-rendered") but does not re-freeze them.

## Deprecation policy

Deprecating a v1 op follows a two-release protocol:

1. **Release N (deprecation announced):** op continues to work
   identically. Add `#[deprecated(since = "<N>", note = "…")]` on
   the `<Op>Op` struct. `ark scene check` emits
   `warning[scene/deprecated-op]` when the scene uses it. The op's
   entry in `CORE_OP_NAMES` stays; cavekit-scene.md gets a deprecation
   notice.
2. **Release N+2 minimum:** op may be removed in the next MAJOR bump
   (2.0, 3.0, …). MINOR removal is never allowed, even for a deprecated
   op, per §"Version compatibility contract" rule 3.

Scenes using a deprecated op continue to compile and run until the
MAJOR removal. The deprecation warning is diagnostic-level, not a hard
failure — `--deny-warnings` callers opt in to stricter enforcement.

## CI enforcement

`ark scene check --v1-strict` is the mechanical gate. When set, it:

- Rejects op names outside the frozen `ark.core.*` vocabulary with
  `error[scene/v1-strict]`.
- Rejects `engine { command … }` blocks pointing at an engine not
  yet wired through ACP (v1 engines = claude, codex, gemini-cli).
- Upgrades `warning[ext/unknown-capability]` to an error.
- Upgrades `warning[scene/deprecated-op]` to an error.

Shipped scenes pass `--v1-strict` in CI. User scenes do NOT need to
pass `--v1-strict`; the default `ark scene check` allows the
deprecation warnings through.

## Reference surface

| Artifact                               | Purpose                                    |
|----------------------------------------|--------------------------------------------|
| `crates/scene/src/ops/mod.rs`          | `CORE_OP_NAMES`, `register_core_ops`.       |
| `crates/scene/src/ops/{tabs,panes,plugins,messaging,control,acp}.rs` | Per-op typed args + dispatch.        |
| `crates/scene/src/intent.rs`           | `Intent` trait, `IntentRegistry`, error types. |
| `crates/scene/src/v1_strict.rs`        | `--v1-strict` validator. |
| `context/refs/wasm-metadata-v1.md`     | Companion doc: extension metadata wire shape. |
| `context/kits/cavekit-scene.md` R7     | Op vocabulary requirement text. |
| `context/kits/cavekit-scene.md` R16    | Extension protocol compatibility rules. |

## Rationale

Freezing 17 ops is a deliberate choice over a smaller or larger set:

- **Smaller** (e.g. freeze only the 13 non-ACP ops) would leave scene
  authors with no stable way to drive an agent turn. ACP is the
  primary value prop of ark; omitting it from v1 would force authors
  to re-migrate at v1.1.
- **Larger** (e.g. include `session/fork`, `nes/*`) would lock in ACP
  surface that is itself still stabilising upstream. The Agent Client
  Protocol crate publishes the stable subset; we track their stability
  commitments 1:1 on this side.

Adding a new op at 1.1 costs one registry entry + one type. Removing
one costs a major version. The asymmetry is intentional.
