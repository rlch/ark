---
title: "Scene CLI"
description: "ark scene check, fmt, dry-run, graph, explain, reload"
---

The `ark scene` subcommand tree provides offline validation, formatting, introspection, and live reload for scene files.

## `ark scene check`

Parse, resolve, and validate a scene file. Exits 0 on success, non-zero with full diagnostics on any error. Reports every error, not just the first.

```bash
ark scene check                    # validate default scene
ark scene check my-scene.kdl       # validate specific file
ark scene check --v1-strict scene.kdl  # enforce v1.0 contract
```

Validation covers: KDL parse, scope rules, handle uniqueness, view resolution, Rhai predicate compilation, op reference validation, and handle type checking.

## `ark scene fmt`

Canonical-format a scene file. Idempotent. Orders top-level nodes by category:

1. `use` / `include` (composition)
2. `layout` (structure)
3. `on` (reactions -- relative order preserved)
4. `bind` (keybinds -- relative order preserved)
5. Everything else (`clear-reactions`, `clear-bind`, `disable-extension`, `mode`)

```bash
ark scene fmt                      # format default scene in-place
ark scene fmt my-scene.kdl         # format specific file
ark scene fmt --check              # check only, exit 1 if changes needed
```

The formatter auto-promotes plain KDL strings to raw strings (`#"..."#`) when a `when=` predicate body contains `"` (required for Rhai string literals).

## `ark scene dry-run`

Simulate one event fire against the scene. Prints which reactions match and which ops would execute, without any side effects.

```bash
ark scene dry-run --event 'Started'
ark scene dry-run --event 'UserEvent:user.build-done' --payload '{"result":"success"}'
ark scene dry-run --event 'Done' --file my-scene.kdl
```

Output shows each matching reaction with its predicate verdict and op list:

```
scene dry-run (scene.kdl) event=Started  matched=2  would-fire=2
  on "Started" [unconditional]
    1. set_status
    2. focus
  on "Started" when="agent.name == \"builder\"" [when -> true]
    1. emit
```

Reactions whose `when=` predicate evaluates to `false` or errors are shown as skipped.

## `ark scene graph`

Render an attribution tree showing every extension, view, reaction, keybind, and intent in the composed scene. Each leaf is tagged with its origin file and line.

```bash
ark scene graph                    # default scene
ark scene graph my-scene.kdl       # specific file
ark scene graph --format json      # JSON output (for tooling)
```

## `ark scene explain`

Trace the resolution of a single ref. Answers "where did this come from?"

```bash
ark scene explain intent:ark.core.focus
ark scene explain bind:"Alt d"
ark scene explain view:diff
ark scene explain reaction:Started
ark scene explain ext:claude-code
```

Output shows every fragment that declares the ref, which one won the merge, and the file:line of each declaration.

## `ark scene explain-merge`

Trace the full R11 composition pipeline -- which fragment contributed each element and how merge rules resolved conflicts.

```bash
ark scene explain-merge scene.kdl
```

Merge rules by category:

| Category | Rule |
|---|---|
| Reactions | Append in load order (parents first, includes in source order, root last) |
| Keybinds | Last-wins per chord |
| Extensions | Duplicate-by-name is an error unless `override=true` |
| Layout | First fragment seeds base; later fragments append tabs |

## `ark scene reload`

Hot-reload the active scene in a running session via the supervisor's control socket.

```bash
ark scene reload --session myfeat
```

The `--session` flag is required. It accepts any unambiguous identifier: full agent ID, name prefix, or unique substring. When omitted, the command errors with a pointer to `ark list`.

Under the hood, this sends the `ark.core.reload_scene` intent over the supervisor's unix socket and waits for the response. The reload honors the turn-inflight gate and re-entry guard (see [Hot Reload](/scenes/hot-reload/)).

## `ark scene schema-dump`

Dump the scene grammar schema from facet SHAPE reflection. Useful for editor tooling and LSP integration.

```bash
ark scene schema-dump              # emit to stdout
ark scene schema-dump --format json
```
