//! Typed pane handle wrappers (T-033 / R17).
//!
//! Extensions that declare intents on `impl ViewStruct` receive **typed
//! pane handles** rather than raw strings:
//!
//! - `impl CommandView` views receive [`CommandPane`] handles with
//!   subprocess-shaped affordances (`.env()`, future `.write_stdin()`,
//!   `.pid()`).
//! - `impl ZellijView` views receive [`PluginPane`] handles with
//!   plugin-shaped affordances (future `.pipe()`).
//!
//! Both types implement the common [`PaneHandle`] trait so generic
//! op-dispatch code can carry them uniformly when the affordance
//! distinction doesn't matter (e.g. `.handle()` / `.emit()`).
//!
//! ## Scope of T-033
//!
//! This module defines the *types*. Binding the correct type to each
//! pane's view (compile-time inference from `ViewMeta::render_mode`)
//! happens in T-090's derive macro. The methods stubbed here
//! (`write_stdin`, `pid`, `pipe`) are deliberately absent until their
//! owning tiers land:
//!
//! - `CommandPane::write_stdin`, `CommandPane::pid` — T-048+.
//! - `PluginPane::pipe` — T-097+.
//!
//! Today only [`CommandPane::env`] is wired because it has no runtime
//! dependency — it's a pure formatter over the handle string.

use crate::ast::layout::Handle;

// ---------------------------------------------------------------------------
// Common trait (R17 — emit + handle shared across pane kinds)
// ---------------------------------------------------------------------------

/// Common surface every typed pane handle offers.
///
/// Currently exposes only [`PaneHandle::handle`] — `emit` lands in
/// T-097+ alongside the extension runtime's cross-handler event bus,
/// and `pipe` is specific to [`PluginPane`].
pub trait PaneHandle {
    /// Access the underlying [`Handle`] identity key.
    ///
    /// The reconciler uses this to map op targets back to running
    /// zellij panes via `ARK_HANDLE=<handle>` env matching (R3).
    fn handle(&self) -> &Handle;
}

// ---------------------------------------------------------------------------
// TabHandle — identity wrapper for tab-addressed ops (R7)
// ---------------------------------------------------------------------------

/// Typed wrapper around a tab's [`Handle`].
///
/// Exists as a distinct type from [`CommandPane`] / [`PluginPane`] so
/// tab-only ops (`rename`, `new_tab`) can reject pane handles at the
/// type layer once the derive macro (T-090) binds the right type to
/// each op argument.
#[derive(Debug, Clone)]
pub struct TabHandle(
    /// The tab's identity handle (`@main`, `@review`, …).
    pub Handle,
);

// ---------------------------------------------------------------------------
// CommandPane — for CommandView-rendered panes (R6 / R17)
// ---------------------------------------------------------------------------

/// Typed pane handle passed to intents on an `impl CommandView` view.
///
/// Exposes command-subprocess affordances — currently only the
/// `ARK_HANDLE` env-var formatter needed by the reconciler's
/// override-layout emission (R3). Stdio and process-id accessors wire
/// in at T-048+ alongside the supervisor's subprocess tracking.
#[derive(Debug, Clone)]
pub struct CommandPane {
    /// The pane's identity handle.
    pub handle: Handle,
}

impl CommandPane {
    /// Render the env-wrapper token the reconciler prepends to the
    /// pane's subprocess invocation (R3).
    ///
    /// Format: `ARK_HANDLE=@<name>`. The `@` prefix is preserved so the
    /// reconciler can round-trip the handle through the zellij
    /// `command "env" "ARK_HANDLE=@<h>" "<cmd>"` layout emission with a
    /// single string splice.
    pub fn env(&self) -> String {
        format!("ARK_HANDLE={}", self.handle.raw())
    }

    // `write_stdin` + `pid` land in T-048+; intentionally absent today.
}

impl PaneHandle for CommandPane {
    fn handle(&self) -> &Handle {
        &self.handle
    }
}

// ---------------------------------------------------------------------------
// PluginPane — for ZellijView-rendered panes (R6 / R17)
// ---------------------------------------------------------------------------

/// Typed pane handle passed to intents on an `impl ZellijView` view.
///
/// Exposes plugin affordances — `pipe` lands in T-097+ alongside the
/// zellij plugin runtime's message pipe, so today the type is a bare
/// identity carrier.
#[derive(Debug, Clone)]
pub struct PluginPane {
    /// The pane's identity handle.
    pub handle: Handle,
}

impl PluginPane {
    // `pipe` lands in T-097+; intentionally absent today.
}

impl PaneHandle for PluginPane {
    fn handle(&self) -> &Handle {
        &self.handle
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(name: &str) -> Handle {
        Handle::new(name).expect("fixture handle should parse")
    }

    #[test]
    fn command_pane_emits_env_wrapper() {
        let pane = CommandPane {
            handle: handle("@editor"),
        };
        assert_eq!(pane.env(), "ARK_HANDLE=@editor");
    }

    #[test]
    fn plugin_pane_handle_accessor() {
        let pane = PluginPane {
            handle: handle("@status"),
        };
        // Trait-level access.
        let via_trait: &Handle = pane.handle();
        assert_eq!(via_trait.raw(), "@status");
        // Direct field access.
        assert_eq!(pane.handle.name(), "status");
    }

    #[test]
    fn command_pane_exposes_handle_via_trait() {
        let pane = CommandPane {
            handle: handle("@editor_1"),
        };
        let via_trait: &Handle = pane.handle();
        assert_eq!(via_trait.raw(), "@editor_1");
    }

    #[test]
    fn tab_handle_wraps_handle() {
        let tab = TabHandle(handle("@main"));
        assert_eq!(tab.0.raw(), "@main");
        assert_eq!(tab.0.name(), "main");
    }
}
