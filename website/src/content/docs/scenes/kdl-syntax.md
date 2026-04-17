---
title: "KDL Syntax"
description: "KDL 2.0 primer and scene grammar"
---

Ark scenes use [KDL 2.0](https://kdl.dev). This page covers the KDL basics you need and the ark-specific grammar layered on top.

## KDL crash course

```kdl
// Nodes have a name, optional arguments, optional properties, optional children
node "arg1" "arg2" key="value" {
    child-node "nested"
}
```

- **Arguments** are positional values after the node name.
- **Properties** are `key=value` pairs.
- **Children** are enclosed in `{ }`.
- Strings use double quotes. Raw strings use `#"..."#` (useful when the string contains `"`).
- Comments: `//` line, `/* */` block, `/-` node-slashdash (comments out the next node).

## Scene grammar

A scene file has exactly one top-level node. Multiple `scene` nodes are a parse error.

```kdl
scene "my-session" {
    // Legal top-level children (any order except on/bind which run in textual order):
    use "claude-code"
    include "ext:claude-code/reactions"

    layout { /* ... */ }

    mode "review" { /* ... */ }

    on "Started" { /* ops */ }

    bind "Alt d" { /* ops */ }

    clear-reactions event="Started"
    clear-bind "Alt d"
    disable-extension "some-ext"
}
```

Unknown nodes at any level produce a parse error with "did you mean ...?" suggestions.

## Scope rules

Every node has a defined set of legal parents:

| Node | Legal parent |
|---|---|
| `use`, `include`, `on`, `bind`, `mode`, `clear-reactions`, `clear-bind`, `disable-extension` | Scene root |
| `tab` | `layout { }` |
| `row`, `col`, `pane` | `tab`, or nested inside `row`/`col` |
| `when=` attribute | `tab`, `pane`, `row`, `col`, and individual op nodes |

Violations produce `error[scene/misplaced-node]` with parent-node context.

## Handles (`@handle`)

Every `tab` and `pane` requires a handle. Handles are the identity keys the reconciler uses to match panes across reloads.

```kdl
layout {
    tab "@main" focus=true {
        row {
            pane "@editor" { edit path="src/main.rs" }
            pane "@shell"  { shell }
        }
    }
    tab "@scratch" {
        pane "@notes" { shell }
    }
}
```

Rules:
- Handles are prefixed with `@` in the KDL source.
- The namespace is **flat and scene-scoped** -- tabs and panes share one namespace.
- Duplicate handles produce `error[scene/handle-clash]`.
- Missing handles produce a compile error.

Handles are used everywhere: ops target them (`focus "@editor"`), the reconciler matches by them, and every pane command is wrapped with `env ARK_HANDLE=@<handle>` for unique identification.

## Layout DSL

Ark owns the layout vocabulary. Zellij is the rendering backend.

```kdl
layout {
    tab "@main" name="Main" focus=true cwd="{env.HOME}/project" {
        row {
            pane "@code" span=3 { edit path="src/lib.rs" }
            col span=1 {
                pane "@term" { shell }
                pane "@diff" { diff }
            }
        }
    }
}
```

**Sizing:**

| Attribute | Meaning |
|---|---|
| `span=N` | Relative weight within container. Siblings normalize to 100%. |
| `cells=N` | Fixed size in terminal cells. |
| `min=N` | Minimum size in cells. |
| `max=N` | Maximum size in cells. |

**Overlays (floating panes):**

```kdl
pane "@palette" overlay pos="center" size="60%x40%" {
    command cmd="fzf"
}
```

`pos` accepts: `top-right`, `top-left`, `bottom-right`, `bottom-left`, `center`, or `X%xY%`. `sticky=true` pins the overlay across tab switches.

## Views

A pane contains exactly one **view** -- the content that fills it. Zero or more than one view per pane is a compile error.

Three tiers of views share one namespace:

| Tier | Examples | Source |
|---|---|---|
| Primitives | `command`, `shell`, `edit` | Built-in |
| Shipped | `diff`, `status`, `picker` | Compiled-in extensions |
| User-installed | `nvim`, `lazygit` | `ark ext add` |

```kdl
pane "@editor" { edit path="src/main.rs" }
pane "@term"   { shell }
pane "@build"  { command cmd="cargo" args=["watch", "-x", "check"] }
```

View attributes are schema-validated against the view's declared shape.

## Rhai expressions

`when=` predicates and `{expr}` interpolation holes use [Rhai](https://rhai.rs) in expression-only mode. No `fn`, no loops, no assignment -- just expressions.

```kdl
// Conditional pane (evaluated by reconciler)
pane "@review" when=#"agent.phase == "review""# {
    diff
}

// String interpolation (evaluated at spawn)
tab "@main" cwd="{env.HOME}/projects/{name}" {
    pane "@shell" { shell }
}
```

Predicates containing string literals must use KDL raw strings (`#"..."#`) because Rhai strings also use double quotes. `ark scene fmt` auto-promotes plain strings to raw strings when the body contains `"`.

Two evaluation scopes:

| Scope | Available bindings | Evaluated |
|---|---|---|
| Spawn context | `cwd`, `id`, `name`, `env` | Once at layout render |
| Event context | `event`, `payload`, `agent`, `session`, captured locals | Per event fire |
