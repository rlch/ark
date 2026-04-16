---
created: "2026-04-16"
last_edited: "2026-04-17"
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

v1.0 freezes **18 ops across 5 subject groups** under the
`ark.core.*` and `ark.acp.*` namespaces. Four of those are
ACP-interaction ops (`acp.prompt`, `acp.cancel`, `acp.permit`,
`acp.set_mode`); the other 14 are the "classic" scene ops. This
document covers all 18 — they share one compatibility contract and
one freeze event.

Extensions contribute ops under their own namespace (e.g.
`my-ext.do_thing`); extension-authored ops are NOT subject to this
freeze — their stability is the extension author's concern. The scene
compiler rejects extension registrations that land inside the reserved
`ark.core.*` prefix (`error[ext/reserved-namespace]`).

## Op vocabulary at a glance

| Group     | Op                           | KDL verb          | Idempotency                    |
|-----------|------------------------------|-------------------|--------------------------------|
| panes     | `ark.core.focus`             | `focus`           | noop on absent handle          |
| panes     | `ark.core.close`             | `close`           | noop on absent handle          |
| panes     | `ark.core.rename`            | `rename`          | noop on absent handle          |
| panes     | `ark.core.resize`            | `resize`          | noop on absent handle          |
| panes     | `ark.core.move`              | `move`            | noop on absent handle          |
| panes     | `ark.core.pin`               | `pin`             | noop on absent handle          |
| panes     | `ark.core.unpin`             | `unpin`           | noop on absent handle          |
| spawn     | `ark.core.spawn`             | `spawn`           | check-then-create-else-focus   |
| spawn     | `ark.core.new_tab`           | `new_tab`         | check-then-create-else-focus   |
| messaging | `ark.core.pipe`              | `pipe`            | always-side-effect             |
| messaging | `ark.core.emit`              | `emit`            | always-side-effect             |
| messaging | `ark.core.set_status`        | `set_status`      | always-side-effect             |
| control   | `ark.core.exec`              | `exec`            | always-side-effect             |
| control   | `ark.core.reload_scene`      | `reload_scene`    | noop when no reloader          |
| acp       | `ark.acp.prompt`             | `acp.prompt`      | always-side-effect             |
| acp       | `ark.acp.cancel`             | `acp.cancel`      | always-side-effect             |
| acp       | `ark.acp.permit`             | `acp.permit`      | always-side-effect             |
| acp       | `ark.acp.set_mode`           | `acp.set_mode`    | always-side-effect             |

Scene authors write the KDL verb. The registry dispatches through the
fully-qualified `ark.core.*` / `ark.acp.*` name; both forms are frozen.
The canonical list is `CORE_OP_NAMES` in `crates/scene/src/ops/mod.rs`.

## Op schemas

Each entry lists the KDL shape, every arg (type + required/optional),
the return value, and side effects. Schemas are derived from the
dispatch implementations in `crates/scene/src/ops/`.

### Pane / tab ops (T-048)

#### `ark.core.focus`

```kdl
focus "@handle"
```

Polymorphic (tab or pane). Handle-type resolution from
`IntentContext::handle_type_hint`. **Idempotent:** noop on absent handle.
**Returns:** `IntentValue::None`.

#### `ark.core.close`

```kdl
close "@handle"
```

Polymorphic. **Idempotent:** noop on absent handle.
**Returns:** `IntentValue::None`.

#### `ark.core.rename`

```kdl
rename "@handle" to="<name>"
```

Tab-only at compile time. `to=` required.
**Returns:** `IntentValue::None`.

#### `ark.core.resize`

```kdl
resize "@handle" direction="<up|down|left|right>" by="<inc|dec>"
```

Pane-only. Both `direction=` and `by=` required.
**Returns:** `IntentValue::None`.

#### `ark.core.move`

```kdl
move "@handle" to="<anchor>"
```

Pane-only. `to=` required. **Returns:** `IntentValue::None`.

#### `ark.core.pin`

```kdl
pin "@handle"
```

Overlay pane. **Idempotent:** noop on absent handle.
**Returns:** `IntentValue::None`.

#### `ark.core.unpin`

```kdl
unpin "@handle"
```

Overlay pane. **Idempotent:** noop on absent handle.
**Returns:** `IntentValue::None`.

### Spawn ops (T-049)

#### `ark.core.spawn`

```kdl
spawn "@handle" ["overlay" pos="…" size="…"] { <view> }
```

Create a pane. If handle already live, focuses existing pane instead
(check-then-create-else-focus, T-055). `overlay` keyword is a bare
positional. **Returns:** `IntentValue::None`.

#### `ark.core.new_tab`

```kdl
new_tab "@handle" [name="…"] [cwd="…"]
```

Create a tab. Same check-then-create-else-focus policy as `spawn`.
**Returns:** `IntentValue::None`.

### Messaging ops (T-050)

#### `ark.core.pipe`

```kdl
pipe from="@handle" to="@handle" payload="<str>"
```

All three properties required. Always side-effect.
**Returns:** `IntentValue::None`.

#### `ark.core.emit`

```kdl
emit "<event-name>" {
    <key> "<value>"*
}
```

Positional event name required. Children block is converted to a JSON
object payload. **Returns:** `IntentValue::None`. **Side effects:**
publishes `UserEvent { name, payload, source }` on the bus.

#### `ark.core.set_status`

```kdl
set_status text="<str>" [severity="<str>"] [ttl_ms=<int>]
```

`text=` required. Routes through `EventBus::push_status`.
**Returns:** `IntentValue::None`.

### Control ops (T-051)

#### `ark.core.exec`

```kdl
exec script="<str>" [shell="<str>"] [timeout_ms=<int>]
```

| Arg          | Type   | Req | Default   | Notes                              |
|--------------|--------|-----|-----------|------------------------------------|
| `script`     | string | yes |           | Shell script to execute.           |
| `shell`      | string | no  | `"sh"`    | Shell interpreter.                 |
| `timeout_ms` | u64    | no  | `30_000`  | Exceeding surfaces as `op/failed`. |

**Returns:** `IntentValue::Integer(exit_code)`.
**Side effects:** spawns via `tokio::process::Command`.

#### `ark.core.reload_scene`

```kdl
reload_scene
```

No args. Stub in Tier 5 (logs + returns `None`); wired to supervisor
`SceneReloader` in Tier 14 (T-083). **Returns:** `IntentValue::None`.

### ACP ops (T-105)

#### `ark.acp.prompt`

```kdl
acp.prompt text="<str>"
```

`text=` required. Noop when no ACP extension is active (T-106).
**Returns:** `IntentValue::None`.

#### `ark.acp.cancel`

```kdl
acp.cancel
```

No args. Noop when no ACP extension active.
**Returns:** `IntentValue::None`.

#### `ark.acp.permit`

```kdl
acp.permit request_id="<str>" outcome="<allow|reject_once|reject_always>"
```

Both properties required. `outcome` validated against
`["allow", "reject_once", "reject_always"]`.
**Returns:** `IntentValue::None`.

#### `ark.acp.set_mode`

```kdl
acp.set_mode mode="<str>"
```

`mode=` required. Noop when no ACP extension active.
**Returns:** `IntentValue::None`.

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
| `crates/scene/src/ops/{panes,spawn,messaging,control,acp}.rs` | Per-op typed args + dispatch.        |
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
