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
//! Config-schema pointers on the [`ViewMeta`] entries come straight
//! from `<ConfigStruct as facet::Facet>::SHAPE` (T-027). The
//! per-primitive config structs ([`CommandViewConfig`],
//! [`EditViewConfig`]) live alongside the registration helper so the
//! `&'static Shape` pointers survive the full program lifetime and the
//! compile-time schema walk has a single obvious source of truth.

use facet::Facet;

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

// ---------------------------------------------------------------------------
// Per-primitive config structs (T-027)
// ---------------------------------------------------------------------------

/// Config schema for the `command` primitive — mirrors the R6 shape
/// `command "bin" [args…]` as a reflected struct so the scene compiler
/// can validate the KDL pane body against the declared fields.
///
/// The struct itself is never materialised at runtime — the reconciler
/// reads `command` primitive config straight off the layout KDL via
/// [`crate::compile::layout`]. `#[derive(Facet)]` is here purely to
/// expose a `SHAPE` pointer for [`register_primitives`] to stash on
/// the primitive's [`ViewMeta::config_schema`].
#[derive(Facet, Debug, Clone)]
pub struct CommandViewConfig {
    /// Absolute or `$PATH`-resolvable binary name.
    pub cmd: String,
    /// Positional arguments passed to the binary after the
    /// `env ARK_HANDLE=<handle>` prefix wraps the invocation.
    pub args: Vec<String>,
}

/// Config schema for the `edit` primitive — zellij's native edit pane
/// needs a single `path: String` pointing at the file to open.
#[derive(Facet, Debug, Clone)]
pub struct EditViewConfig {
    /// Path to the file to open in `$EDITOR`.
    pub path: String,
}

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
        // T-027: real `facet::Shape` pointer for the `command` config
        // struct — consumed by `ark scene check` to validate
        // `pane @h { command "bin" arg1 arg2 }` bodies.
        config_schema: Some(<CommandViewConfig as Facet>::SHAPE),
    });

    registry.register(ViewMeta {
        name: SHELL.to_string(),
        source: ViewSource::Primitive,
        render_mode: RenderMode::CommandView,
        // `shell` takes no config — the pane body is bare (`shell { }`
        // or just `shell`). Validator rejects any entry inside the
        // body because `None` flags "schema is empty".
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
        config_schema: Some(<EditViewConfig as Facet>::SHAPE),
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
        let meta = reg
            .resolve(COMMAND)
            .expect("command primitive should register");
        assert_eq!(meta.render_mode, RenderMode::CommandView);
        assert_eq!(meta.source, ViewSource::Primitive);
        let shape = meta
            .config_schema
            .expect("command primitive carries a facet Shape");
        // T-027: shape identity pins to `CommandViewConfig` so downstream
        // validators can field-walk without inspecting strings.
        assert_eq!(shape.type_identifier, "CommandViewConfig");
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
        let shape = meta
            .config_schema
            .expect("edit primitive carries a facet Shape");
        assert_eq!(shape.type_identifier, "EditViewConfig");
    }

    /// T-027: the reflected `CommandViewConfig` schema must expose the
    /// declared fields so the scene compiler can walk them to validate
    /// KDL pane bodies and emit `did you mean?` suggestions.
    #[test]
    fn command_config_shape_has_expected_fields() {
        use facet::{Type, UserType};
        let shape = <CommandViewConfig as Facet>::SHAPE;
        let st = match &shape.ty {
            Type::User(UserType::Struct(s)) => s,
            other => panic!("expected struct shape, got {other:?}"),
        };
        let names: Vec<&'static str> = st.fields.iter().map(|f| f.name).collect();
        assert!(names.contains(&"cmd"));
        assert!(names.contains(&"args"));
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
