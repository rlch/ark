//! Primitive view registration (T-028 / T-029 / T-030).
//!
//! Three kernel primitives — `command`, `shell`, `edit` — that map 1:1 to
//! zellij native content types (R6). They are registered first in the
//! view registry so extension-tier views can never shadow them (per R6's
//! "primitives-first" resolution order).
//!
//! Render-mode mapping (R6):
//!
//! | Primitive | Render mode  | Zellij emit shape                            |
//! |-----------|--------------|----------------------------------------------|
//! | `command` | CommandView  | `pane { command "env" "ARK_HANDLE=x" "X"; args … }` |
//! | `shell`   | CommandView  | `pane { command "env" "ARK_HANDLE=x" "$SHELL" }`   |
//! | `edit`    | ZellijView   | `pane { edit "path" }` (uses zellij's native edit) |
//!
//! Config-schema summaries on the [`ViewMeta`] entries are placeholder
//! strings (T-027 stub) — real facet `SHAPE` pointers land with T-090's
//! derive-macro work.

use super::{RenderMode, ViewMeta, ViewRegistry, ViewSource};

/// `command` primitive — runs an arbitrary binary in a pane (R6).
///
/// Config: `cmd: String, args: Vec<String>` — the command path + argument
/// list. Invocations are wrapped in `env ARK_HANDLE=<handle>` so the
/// reconciler can identify the subprocess (R3 env-wrapper contract).
pub const COMMAND: &str = "command";

/// `shell` primitive — runs `$SHELL` in a pane (R6). No config.
///
/// Emitted as `pane { command "env" "ARK_HANDLE=<handle>" "$SHELL" }`.
/// The empty config_schema on the [`ViewMeta`] reflects "no config
/// accepted"; `shell { any-attr=… }` surfaces as `error[ext/bad-config]`
/// at compile.
pub const SHELL: &str = "shell";

/// `edit` primitive — opens a file in zellij's native edit pane (R6).
///
/// Config: `path: String`. Unlike `command` / `shell`, this is a
/// [`RenderMode::ZellijView`] because zellij's `edit` pane is handled by
/// the zellij layout engine directly (no ark-managed subprocess). The
/// file is opened in `$EDITOR`.
pub const EDIT: &str = "edit";

/// Register the three kernel primitives into a [`ViewRegistry`] in
/// canonical order (`command`, `shell`, `edit`).
///
/// Invoked by [`ViewRegistry::with_primitives`]. Call this directly only
/// when bootstrapping a registry that will later layer on non-default
/// tiers in a bespoke order.
pub fn register_primitives(registry: &mut ViewRegistry) {
    registry.register(ViewMeta {
        name: COMMAND.to_string(),
        source: ViewSource::Primitive,
        render_mode: RenderMode::CommandView,
        // T-027 stub: real schema wiring lands in T-090.
        config_schema: Some("cmd: String, args: Vec<String>".to_string()),
    });

    registry.register(ViewMeta {
        name: SHELL.to_string(),
        source: ViewSource::Primitive,
        render_mode: RenderMode::CommandView,
        config_schema: None,
    });

    registry.register(ViewMeta {
        name: EDIT.to_string(),
        source: ViewSource::Primitive,
        // `edit` is handled by zellij's native edit pane, not an ark
        // subprocess — hence ZellijView despite not loading a wasm
        // plugin. The render mode discriminates "ark manages the
        // subprocess" (CommandView) from "zellij owns the pane content"
        // (ZellijView).
        render_mode: RenderMode::ZellijView,
        config_schema: Some("path: String".to_string()),
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_command_view() {
        let reg = ViewRegistry::with_primitives();
        let meta = reg.resolve(COMMAND).expect("command primitive should register");
        assert_eq!(meta.render_mode, RenderMode::CommandView);
        assert_eq!(meta.source, ViewSource::Primitive);
        assert!(meta.config_schema.is_some(), "command accepts cmd+args");
    }

    #[test]
    fn shell_has_no_config() {
        let reg = ViewRegistry::with_primitives();
        let meta = reg.resolve(SHELL).expect("shell primitive should register");
        assert_eq!(meta.render_mode, RenderMode::CommandView);
        assert_eq!(meta.source, ViewSource::Primitive);
        assert!(meta.config_schema.is_none(), "shell takes no config");
    }

    #[test]
    fn edit_is_zellij_view() {
        let reg = ViewRegistry::with_primitives();
        let meta = reg.resolve(EDIT).expect("edit primitive should register");
        assert_eq!(meta.render_mode, RenderMode::ZellijView);
        assert_eq!(meta.source, ViewSource::Primitive);
        assert!(meta.config_schema.is_some(), "edit accepts path");
    }

    #[test]
    fn register_primitives_into_fresh_registry() {
        // Direct call (not via with_primitives) should produce the same
        // three entries in the same canonical order.
        let mut reg = ViewRegistry::new();
        register_primitives(&mut reg);
        let names = reg.all_names();
        assert_eq!(names, vec![COMMAND, SHELL, EDIT]);
    }
}
