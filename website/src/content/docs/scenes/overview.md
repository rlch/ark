---
title: "Overview"
description: "What a scene is and why reactive KDL"
---

A **scene** is a single KDL 2.0 file that declares everything about an ark session: layout, extensions, reactions, keybinds, and modes. One scene file = one composed configuration.

## What a scene declares

| Block | Purpose |
|---|---|
| `use` | Activate an extension (views, intents, events enter scope) |
| `include` | Splice a KDL fragment verbatim |
| `layout` | Tab + pane structure with sizing and conditionals |
| `on` | Reactions: event selector + optional predicate + ops |
| `bind` | Keybinds: chord + ops (compiled to zellij keybinds) |
| `mode` | Named alternate tab layouts (switched via `use_mode`) |

## Why KDL

KDL is a document language with a clean node/property/child model. It reads like config, not code. Ark uses KDL 2.0 parsed via `facet-kdl` with full span preservation for diagnostics.

A scene is **not** a script. There are no variables, no loops, no functions. The only dynamic behavior comes from:

- **`when=` predicates** -- Rhai expressions (expression-only, non-Turing-complete) that gate tabs, panes, and individual ops.
- **`{expr}` interpolation** -- Rhai holes in string values, evaluated at spawn or per-event.

## Desired-state reconciliation

Scenes follow a Kubernetes-style model. The scene declares what **should** exist. Ark renders the desired layout as zellij KDL, then issues `zellij action override-layout` to converge. Zellij handles the diff: it retains matched panes, creates missing ones, and closes extras.

When a `when=` predicate flips, ark re-renders and re-issues the override (debounced 200ms). User-initiated changes (manually closing a pane, adding a tab) are tolerated between reconciliation triggers.

## Compile pipeline

```
scene.kdl
  |  parse (facet-kdl + miette diagnostics)
  v
SceneIR { uses, includes, layout, modes, reactions, keybinds }
  |  resolve extensions, splice includes, validate
  v
CompiledScene
  |  evaluate when= predicates, render layout KDL
  v
+-- layout.kdl        -> zellij --layout
+-- subscriber registry -> one per `on` block
+-- keybinds           -> injected into layout KDL
+-- mode layouts       -> pre-rendered for use_mode
```

Every error surfaces via `miette` with file name, line/col, caret, and help text. `ark scene check` validates the full pipeline without launching a session.

## Minimal scene

```kdl
scene "default" {
    layout {
        tab "@main" focus=true {
            pane "@shell" { shell }
        }
    }
}
```

This creates a single tab with a shell pane. No extensions, no reactions, no keybinds. The built-in default scene embedded in ark is exactly this shape.

## Backward compatibility

Ark detects the file shape on load:

- `scene "name" { }` wrapper -- used directly.
- Bare `layout { }` without `scene` -- auto-wrapped as `scene "default" { layout { ... } }`.
- Neither -- parse error.
