# ark layouts — authoring guide

`ark` materializes each agent tab from a KDL [layout][zellij-layouts] —
either one of the six shipped templates or a file you author. This page
documents the templating surface, the resolver, and the `ark doctor`
validation hook.

> Spec sources: `context/kits/cavekit-layouts.md` (R1, R3, R5, R6) and
> `context/kits/cavekit-mux-zellij.md` (R5).

## Template variables

Templates are rendered with [minijinja][minijinja] in **strict mode** —
referencing an undefined variable is a hard error before zellij is ever
invoked. The exposed surface is intentionally small:

| Variable          | Type           | Example                                            |
| ----------------- | -------------- | -------------------------------------------------- |
| `{{ cwd }}`       | string         | `/Users/me/Code/myproj`                            |
| `{{ agent_cmd }}` | string         | `claude`                                           |
| `{{ agent_args }}`| list&lt;string&gt; | `["--resume", "--verbose"]`                       |
| `{{ id }}`        | string         | `cavekit-auth-01jx7z8k6x9y2zt4abcdef0123`          |
| `{{ name }}`      | string         | `builder`                                          |

`agent_args` is iterable in the usual minijinja way:

```kdl
pane name="agent" {
    command "{{ agent_cmd }}"
    {% for a in agent_args %}args "{{ a }}"
    {% endfor %}
}
```

There is no env access, no `sys` namespace, no template inheritance, no
`include`. Adding a variable is an audited, breaking change.

## External commands are allowed

A pane's `command` can invoke any binary on `PATH` — `nvim`, `lazygit`,
your own scripts. The shipped layouts use `ark pane diff`, `ark pane git`
and `ark pane log` for portability, but nothing prevents you from
embedding a watcher of your own:

```kdl
pane name="tests" {
    command "cargo"
    args "watch" "-x" "test"
}
```

## Path pass-through

`--layout` accepts both stems and absolute / relative paths:

```bash
ark spawn --layout focused
ark spawn --layout ~/.config/ark/layouts/myreview.kdl
ark spawn --layout ./local-experiment.kdl
```

When a path is given it is used verbatim after templating. The path
**must end in `.kdl`** — zellij issue [#4994][zellij-4994] silently
ignores other extensions when invoked with `--layout`.

## Shadowing shipped layouts

Drop a `.kdl` file into `~/.config/ark/layouts/{stem}.kdl` to override
the embedded version of that stem. Resolution order:

1. `${XDG_CONFIG_HOME}/ark/layouts/{stem}.kdl` (user override)
2. Embedded shipped layout

`ark layouts list` prints every available stem with its source
(`user` / `embedded`). User overrides shadowing a shipped stem appear
only as `user`.

## Validation (`ark doctor`)

`ark doctor` invokes `LayoutResolver::validate_user_layouts()` on your
`~/.config/ark/layouts/`. Each `.kdl` file is rendered against a dummy
`LayoutVars` and the rendered output is brace-/string-checked. Failures
include the originating path so you can fix them in place.

The validator does **not** replicate zellij's full parser — it catches
unbalanced braces, unterminated strings, undefined template variables
and template syntax errors. Zellij itself remains the source of truth
for layout grammar.

## Shipped layouts

| Stem            | Shape                                                       | Default for |
| --------------- | ----------------------------------------------------------- | ----------- |
| `builder`       | agent (60%) over diff + git side-by-side                    | cavekit     |
| `classic`       | agent (70%) beside diff                                     | claude-code |
| `focused`       | single agent pane                                           | —           |
| `triple-column` | agent / diff / git as three vertical columns                | ultrawides  |
| `review`        | agent on top, `ark pane log` underneath                     | cavekit review phase |
| `log`           | single `ark pane log --id {{ id }}` pane                    | opt-in      |

[zellij-layouts]: https://zellij.dev/documentation/creating-a-layout
[zellij-4994]: https://github.com/zellij-org/zellij/issues/4994
[minijinja]: https://docs.rs/minijinja
