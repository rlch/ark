//! `ark-plugin-picker` — interactive zellij plugin that surfaces the agent
//! picker (list / detail / new / kill / help screens).
//!
//! This file lands the T-099 scaffold only; state, bootstrap, list render,
//! detail, forms, and distribution wiring arrive in T-100 through T-106. The
//! scaffold establishes the crate shape, dependency set, and the wasm-side
//! lifecycle glue so later tasks can fill in behaviour without touching build
//! configuration.
//!
//! Satisfies `context/kits/cavekit-plugin-picker.md` R1 acceptance criteria:
//!
//! - Crate `ark-plugin-picker` with `crate-type = ["cdylib"]` (see Cargo.toml).
//! - Build target `wasm32-wasip1` — driven by distribution wiring in T-106;
//!   host-side `cargo check` and workspace `cargo build` stay green because
//!   the `ZellijPlugin` impl (and `register_plugin!` macro expansion, which
//!   calls into `host_*` shims imported from the wasm `zellij` module) are
//!   gated behind `#[cfg(target_arch = "wasm32")]`.
//! - Dependencies: `zellij-tile`, `nucleo-matcher` (NOT `fuzzy-matcher` — R1
//!   mandates nucleo-matcher for its smaller wasm footprint), `serde`. The
//!   banned-by-R1 crates (`serde_json`, `humantime`, `chrono`) are deliberately
//!   absent; hand-rolled formatters land with later tasks.
//! - Permissions requested in `load()`:
//!   `ReadCliPipes`, `ChangeApplicationState`, `ReadApplicationState`,
//!   `MessageAndLaunchOtherPlugins`.
//! - Event subscriptions in `load()`:
//!   `Key`, `Timer`, `SessionUpdate`, `ModeUpdate`, `PermissionRequestResult`.
//!   (`PermissionRequestResult` is added so R2's state model can react to the
//!   grant/deny result — standard practice across ark plugins.)
//! - Plugin registers under the name `ark-picker` (see [`PLUGIN_NAME`]) and is
//!   wired through `zellij_tile::register_plugin!` inside the wasm module.
//!
//! # Target gating
//!
//! Same rationale as `ark-plugin-status`: zellij-tile's host shims link to
//! `host_run_plugin_command` which is only defined inside the wasm sandbox.
//! Gating the `ZellijPlugin` impl behind `cfg(target_arch = "wasm32")` lets
//! workspace-wide host builds compile this crate without resolving wasm-only
//! symbols. Host-side tests exercise the plain-Rust state via
//! [`Picker::new`].

/// Registered plugin name used by supervisors when targeting `zellij pipe
/// --name`, matching R1's "load() registers pipe target: ark-picker" bullet.
///
/// Exposed as a constant so the dispatch side (supervisors / orchestrators)
/// and the ingestion filter (this plugin) share one source of truth.
pub const PLUGIN_NAME: &str = "ark-picker";

/// Root plugin state.
///
/// Minimal on purpose — T-100 replaces this with the `PickerScreen` enum and
/// the agents/resurrectable caches described in R2. Keeping it as a unit-ish
/// struct today means the wasm `ZellijPlugin` impl and host-side tests
/// compile against a stable type while the real state model is designed.
#[derive(Debug, Default)]
pub struct Picker {
    // T-100 introduces real fields (PickerScreen, agents cache, filter
    // string, selected index, resurrectable cache, focused session, etc.).
    // No placeholder fields here: adding them now would bake in layout
    // choices that T-100's state-model work should make.
}

impl Picker {
    /// Host-testable constructor. Kept non-`const` so future fields that need
    /// heap allocation (e.g. the agents `BTreeMap` from R2) can slot in
    /// without an API break.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::Picker;
    use zellij_tile::prelude::*;

    impl ZellijPlugin for Picker {
        fn load(&mut self, _configuration: std::collections::BTreeMap<String, String>) {
            // R1: request the four permissions the picker needs across its
            // lifecycle.
            // - ReadCliPipes: ingest supervisor updates piped to `ark-picker`
            //   (R3 incremental updates).
            // - ChangeApplicationState: switch sessions / open panes via
            //   zellij-tile actions when the user selects an agent (R4/R5).
            // - ReadApplicationState: enumerate sessions + panes for the
            //   list screen and detail screen bootstrap (R3/R4).
            // - MessageAndLaunchOtherPlugins: hand off to helper plugins
            //   (e.g. the new-agent form / confirm dialogs) per R6–R8.
            // Grant/deny for the batch surfaces via
            // `EventType::PermissionRequestResult`.
            request_permission(&[
                PermissionType::ReadCliPipes,
                PermissionType::ChangeApplicationState,
                PermissionType::ReadApplicationState,
                PermissionType::MessageAndLaunchOtherPlugins,
            ]);

            // R1: subscribe to the five event streams the picker consumes.
            // - Key: drive the interactive UI (filter typing, arrow keys,
            //   Enter to switch, Ctrl-N new, Ctrl-R rename, Del kill, ?
            //   help — see R4 footer).
            // - Timer: 2 s cadence for timing-sensitive fields and socket
            //   re-scan (R3).
            // - SessionUpdate: learn the focused session so the picker can
            //   highlight / pin it (R3/R4).
            // - ModeUpdate: react to zellij mode transitions (dismiss the
            //   picker when the user exits the plugin mode, etc.).
            // - PermissionRequestResult: react to grant/deny so later tasks
            //   can surface a warning screen instead of silently failing.
            subscribe(&[
                EventType::Key,
                EventType::Timer,
                EventType::SessionUpdate,
                EventType::ModeUpdate,
                EventType::PermissionRequestResult,
            ]);
        }

        fn update(&mut self, event: Event) -> bool {
            // Scaffold stub: explicit arms for each subscribed event type so
            // T-100+ can fill them in without re-deriving the match shape.
            // Returning `false` everywhere means no redraw — safe default
            // while render is a no-op.
            match event {
                Event::Key(_key) => false,
                Event::Timer(_elapsed) => false,
                Event::SessionUpdate(_sessions, _resurrectable) => false,
                Event::ModeUpdate(_mode_info) => false,
                Event::PermissionRequestResult(_status) => false,
                _ => false,
            }
        }

        fn render(&mut self, _rows: usize, _cols: usize) {
            // Scaffold stub: no-op render. T-102 lands the list screen per
            // R4. Leaving render empty here is intentional — a placeholder
            // string would flash in the first iteration's compiled artefact
            // and then have to be unwound.
        }
    }

    register_plugin!(Picker);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check: the host-side constructor returns a value and does not
    /// panic. More meaningful tests land with the T-100 state model.
    #[test]
    fn picker_new_constructs() {
        let _picker = Picker::new();
    }
}
