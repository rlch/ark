---
title: "Compiled-In Extensions"
description: "register_extension! macro"
---

Compiled-in extensions are Rust crates in the ark workspace. They run in-process with zero serialization overhead and are the paved path for built-in extensions like `status`, `picker`, and the ACP bridge.

## One crate, one extension

Each compiled-in extension is a single Rust crate. All derives in the crate auto-group via `module_path!()`. The manifest is code-generated from derives and trait impls — there is no manifest file to maintain.

## Minimal example

```rust
use ark_ext::{Extension, Facet, View, CommandView, CommandPane, Context};

/// A pane that shows git branch + status info.
#[derive(Facet, Extension)]
#[extension(name = "git-status")]
pub struct GitStatus {
    /// Working directory to watch.
    #[facet(default = ".")]
    pub cwd: String,
}

/// The view config for the pane content.
#[derive(Facet, View)]
pub struct GitStatusView {
    /// Refresh interval in milliseconds.
    #[facet(default = "2000")]
    pub refresh_ms: u64,
}

impl CommandView for GitStatusView {
    fn command(&self) -> Vec<String> {
        vec![
            "ark".into(),
            "pane".into(),
            "git".into(),
            "--cwd".into(),
            ".".into(),
        ]
    }
}

/// Event emitted when the branch changes.
#[derive(Facet, Event)]
pub struct BranchChanged {
    pub old_branch: String,
    pub new_branch: String,
}

/// A targeted intent — receives a typed pane handle.
#[ark_intent]
impl GitStatusView {
    pub fn refresh(&self, pane: &CommandPane) {
        pane.write_stdin(b"r"); // trigger refresh
    }
}
```

## Key derives

| Derive | Purpose |
|--------|---------|
| `#[derive(Facet, Extension)]` | Extension identity + config schema |
| `#[derive(Facet, View)]` | View config schema (one view per pane) |
| `#[derive(Facet, Event)]` | Event payload schema (name auto-derived as snake_case) |
| `#[ark_intent]` on `impl` | Intent registration (global or targeted) |

## View rendering modes

The trait you implement determines how the view renders:

| Trait | Pane type | Handle | Use case |
|-------|-----------|--------|----------|
| `CommandView` | Terminal subprocess | `&CommandPane` | CLI tools, TUI apps |
| `ZellijView` | Zellij wasm plugin | `&PluginPane` | Rich interactive UIs |

`CommandPane` exposes `.env()`, `.write_stdin()`, `.pid()`. `PluginPane` exposes `.pipe()`. Both provide `.emit()` and `.handle()`.

## Intent scope

Where you place `#[ark_intent]` determines its scope:

- **On `impl ExtensionStruct`** — global intent, no pane target required
- **On `impl ViewStruct`** — targeted intent, pane handle required in scene

## Events

Extensions emit events via `ctx.emit(event)` (extension-scoped) or `pane.emit(event)` (view-scoped, includes source handle). Events are auto-namespaced by extension name.

An extension can only emit its own events. Any extension can subscribe to any event. Cross-extension wiring is scene-mediated.

## Config ownership

The extension owns the schema and defaults (struct fields + `#[facet(default)]`). The scene author owns the values:

```kdl
scene "dev" {
  use "git-status" config {
    cwd "/home/user/project"
  }
}
```

ark validates config values at `ark scene check` time against the extension's facet SHAPE.

## Registration

Compiled-in extensions register at boot via `inventory`/`linkme`. No central registry file — the linker collects them automatically.
