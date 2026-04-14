---
created: "2026-04-14T00:00:00Z"
last_edited: "2026-04-14T00:00:00Z"
---

# Spec: KDL Layouts

## Scope
Zellij KDL tab layouts — user-composable, shipped defaults, templating variables, authoring guide. Layouts describe the pane structure of a tab; the mux renders them at `create_tab` time with runtime substitutions.

## Requirements

### R1: Layout file locations
**Description:** Where ark finds layouts and how users override.
**Acceptance Criteria:**
- [ ] Resolution order for a layout by stem name:
  1. User override at `$XDG_CONFIG_HOME/ark/layouts/{stem}.kdl`
  2. Shipped embedded layout (`include_str!`), extracted on first use to `$XDG_CACHE_HOME/ark/layouts/{stem}.kdl` if needed by zellij
- [ ] Direct path: passing `--layout /abs/path.kdl` or `--layout ./rel/path.kdl` bypasses stem resolution
- [ ] Nonexistent stems error loudly with remediation (list available stems)
- [ ] `ark layouts list` (part of `ark config` functionality or separate) prints shipped + user stems with source indicator
**Dependencies:** cavekit-cli, cavekit-config

### R2: Shipped layout inventory
**Description:** The shipped tab KDLs.
**Acceptance Criteria:**
- [ ] `builder.kdl` — triple-stack (default for cavekit): agent (60%) | diff (top, 40%×60%) + git (bottom, 40%×40%)
- [ ] `classic.kdl` — 2-pane (default for claude-code): agent (65%) | diff (35%)
- [ ] `focused.kdl` — agent only (100%), no diff pane
- [ ] `triple-column.kdl` — agent (50%) | diff (30%) | git (20%), for ultrawide monitors
- [ ] `review.kdl` — codex findings (75%) | git-watch (25%) — used when cavekit enters review phase
- [ ] `log.kdl` — single-pane `ark pane log --id {{id}}` — opt-in
- [ ] All layouts written for zellij ≥ 0.44 KDL syntax
**Dependencies:** cavekit-pane-commands, cavekit-mux-zellij

### R3: Templating
**Description:** Variable substitution at render time.
**Acceptance Criteria:**
- [ ] Supported variables:
  - `{{cwd}}` — absolute path to worktree
  - `{{agent_cmd}}` — first element of `spec.cmd`
  - `{{agent_args}}` — KDL-array-of-strings of remaining `spec.cmd` elements
  - `{{id}}` — full AgentId
  - `{{name}}` — human label
  - `{{session}}` — zellij session name
- [ ] Syntax: `{{var}}` double-brace; un-defined variables error out before invoking zellij
- [ ] Templating engine: minijinja or handlebars; pinned to a simple subset (no loops, no partials)
- [ ] Rendered files cached at `$XDG_RUNTIME_DIR/ark/layouts/{id}-{tab-name}.kdl`, cleaned when tab closes
**Dependencies:** cavekit-mux-zellij

### R4: Pane components via `ark pane`
**Description:** Layouts invoke `ark pane {subcmd}` so users don't author shell pipelines.
**Acceptance Criteria:**
- [ ] `ark pane diff --cwd {{cwd}}` — diff pane
- [ ] `ark pane git --cwd {{cwd}}` — git status pane
- [ ] `ark pane log --id {{id}}` — event log pane
- [ ] Composable in KDL via `pane { command "ark"; args "pane" "diff" "--cwd" "{{cwd}}" }`
- [ ] Any future pane type is added via a new `ark pane` subcommand — no changes needed to the layout grammar
**Dependencies:** cavekit-pane-commands

### R5: Custom layout authoring guide
**Description:** Clear path for users to write their own.
**Acceptance Criteria:**
- [ ] Documented in `docs/layouts.md` (published to website): template vars list, shipped layouts as examples, zellij KDL reference links
- [ ] Validation: `ark doctor` validates KDL files in user override dir (syntax check via zellij's parser if exposed, else best-effort)
- [ ] Path pass-through: users reference custom layouts by path (`--layout ~/my.kdl`) or by stem if dropped into `~/.config/ark/layouts/`
- [ ] Custom layouts can reference any `ark pane` subcommand or arbitrary external commands (e.g., `nvim`, `lazygit`)
**Dependencies:** R3, R4

### R6: Default layout per orchestrator
**Description:** Orchestrator picks a sensible default; user overrides globally or per-spawn.
**Acceptance Criteria:**
- [ ] Cavekit orchestrator default: `builder` (triple-stack)
- [ ] Claude-code orchestrator default: `classic`
- [ ] Review-phase layout for cavekit: `review`
- [ ] Per-spawn: `--layout` flag overrides
- [ ] Per-user global: `config.orchestrator.{slug}.default_layout`
**Dependencies:** cavekit-config, cavekit-orchestrator-cavekit, cavekit-orchestrator-claude-code

## Wireframes (ASCII, visual reference)

### `classic.kdl` (2-pane, default for claude-code orchestrator)
```
┌──────────────────────────────┬─────────────────────┐
│ agent (claude TUI)           │ diff (delta)        │
│  65%                         │  35%                │
│                              │                     │
│                              │                     │
└──────────────────────────────┴─────────────────────┘
```

### `builder.kdl` (triple-stack, default for cavekit orchestrator)
```
┌──────────────────────────────┬─────────────────────┐
│ agent                        │ diff (delta)        │
│  60%                         │  40% × 60%          │
│                              │                     │
│                              ├─────────────────────┤
│                              │ git-watch           │
│                              │  40% × 40%          │
└──────────────────────────────┴─────────────────────┘
```

### `triple-column.kdl` (ultrawide)
```
┌─────────────────────┬─────────────────────┬──────────────┐
│ agent               │ diff                │ git-watch    │
│  50%                │  30%                │  20%         │
│                     │                     │              │
└─────────────────────┴─────────────────────┴──────────────┘
```

### `focused.kdl` (agent only)
```
┌──────────────────────────────────────────────────────────┐
│ agent                                                    │
│  100%                                                    │
└──────────────────────────────────────────────────────────┘
```

### `review.kdl` (spawned on review phase for cavekit)
```
┌───────────────────────────────────────────┬──────────────┐
│ codex review output                       │ git-watch    │
│  75%                                      │  25%         │
│                                           │              │
└───────────────────────────────────────────┴──────────────┘
```

### `log.kdl` (opt-in events tail)
```
┌──────────────────────────────────────────────────────────┐
│ ark pane log --id {{id}}                                 │
│  100%                                                    │
└──────────────────────────────────────────────────────────┘
```

### Example rendered builder tab (cavekit in use)
```
╔══ zellij session: ark-cavekit-myfeat ══════════════════════════════════╗
║ [builder] [review]                                                      ║
║ ┌───────────────────────────────────┬─────────────────────────────────┐║
║ │ agent (claude TUI)                │ diff (delta, auto-refresh)      │║
║ │                                   │                                 │║
║ │ > Claude                          │ src/auth.rs                     │║
║ │                                   │ ────────────────────────        │║
║ │ I'll implement token verification │ @@ -10,3 +10,8 @@               │║
║ │                                   │     fn login(creds: Creds) {    │║
║ │ ⎿ Edit(src/auth.rs)               │ +       verify_token(&creds)?;  │║
║ │ ⎿ Bash(cargo test)                │ +       audit_log(&creds);      │║
║ │                                   │         ...                     │║
║ │ T-002 token verify ✓              │                                 │║
║ │ T-003 session mgmt → active       │ src/audit.rs (new)              │║
║ │                                   │ + pub fn audit_log(...) {       │║
║ └───────────────────────────────────┴─────────────────────────────────┘║
║ status-bar:  ⟳ myfeat 5/8   iter 3/10  review pending                   ║
╚═════════════════════════════════════════════════════════════════════════╝
```

## Shipped KDL — `builder.kdl`
```kdl
layout {
    cwd "{{cwd}}"
    tab name="builder" {
        pane split_direction="vertical" {
            pane size="60%" name="agent" {
                command "{{agent_cmd}}"
                args {{agent_args}}
            }
            pane split_direction="horizontal" size="40%" {
                pane size="60%" name="diff" {
                    command "ark"
                    args "pane" "diff" "--cwd" "{{cwd}}"
                }
                pane size="40%" name="git" {
                    command "ark"
                    args "pane" "git" "--cwd" "{{cwd}}"
                }
            }
        }
    }
}
```

## Shipped KDL — `classic.kdl`
```kdl
layout {
    cwd "{{cwd}}"
    tab name="builder" {
        pane split_direction="vertical" {
            pane size="65%" name="agent" {
                command "{{agent_cmd}}"
                args {{agent_args}}
            }
            pane size="35%" name="diff" {
                command "ark"
                args "pane" "diff" "--cwd" "{{cwd}}"
            }
        }
    }
}
```

## Shipped KDL — `review.kdl`
```kdl
layout {
    cwd "{{cwd}}"
    tab name="review" {
        pane split_direction="vertical" {
            pane size="75%" name="review" {
                command "bash"
                args "-c" "tail -f context/impl/impl-review-findings.md 2>/dev/null || echo 'waiting for review findings…'"
            }
            pane size="25%" name="git" {
                command "ark"
                args "pane" "git" "--cwd" "{{cwd}}"
            }
        }
    }
}
```

## Out of Scope
- Runtime re-layout of an open tab — KDL is immutable post-creation (zellij limitation)
- Floating panes in v1 — reserved for v1.x (e.g., floating diff on demand)
- Layout inheritance / composition (DRY) — each KDL self-contained
- Multi-tab layouts in one file — one KDL = one tab; orchestrator composes tabs

## Cross-References
- cavekit-mux-zellij.md R5 — layout rendering
- cavekit-pane-commands.md — pane subcommands referenced in layouts
- cavekit-orchestrator-cavekit.md / cavekit-orchestrator-claude-code.md — layout selection
- cavekit-config.md — `default_layout` keys
