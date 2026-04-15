//! `ark-bus` — headless zellij wasm plugin bridging zellij-internal events to
//! the ark supervisor control socket.
//!
//! # Role
//!
//! Zellij keybinds (per cavekit-scene.md R5) dispatch user intents via the
//! zellij plugin protocol; the ark supervisor owns the control socket. This
//! plugin is the headless bridge that sits between the two — it consumes
//! zellij events inside the zellij process and forwards them to the
//! supervisor over IPC. See `context/kits/cavekit-scene.md` R5 (keybinds →
//! ark-bus intent dispatch) and the runtime section in
//! `context/kits/cavekit-architecture.md`.
//!
//! # Status (T-6.1 — skeleton only)
//!
//! This task lays the crate + `ZellijPlugin` impl skeleton so the wasm
//! artifact builds. The two follow-ups layer on top:
//!
//! - T-6.2: hidden-command-pane bridge — spawn a short-lived command pane per
//!   intent to hand zellij actions back to the supervisor.
//! - T-6.3: event forwarder — forward subscribed zellij events (pane focus,
//!   session updates, etc.) to the supervisor control socket.
//!
//! Until those land, `load` / `update` / `render` emit stderr breadcrumbs only
//! so operators can confirm the plugin loaded and is receiving lifecycle
//! callbacks from the zellij host.
//!
//! # Target gating
//!
//! Mirrors `ark-plugin-status` and `ark-plugin-picker`: the `ZellijPlugin`
//! impl and `register_plugin!` expansion link against wasm-only
//! `host_run_plugin_command` symbols, so both are gated behind
//! `#[cfg(target_arch = "wasm32")]`. Host builds still compile the crate
//! (keeps `cargo check --workspace` green) but skip the wasm-only
//! registration.

/// Registered plugin name used by supervisors when targeting this plugin via
/// `zellij pipe --name`. Declared as a constant so dispatchers and (future)
/// ingestion filters share a single source of truth.
pub const PLUGIN_NAME: &str = "ark-bus";

/// Headless bridge between zellij-internal events and the ark supervisor
/// control socket.
///
/// Intent dispatch (T-6.2) and event forwarding (T-6.3) layer on top of this
/// skeleton; this struct currently carries no state and exists only to give
/// the `ZellijPlugin` impl a `Self` to hang off. Follow-up tasks will add
/// cached socket paths, pending-intent queues, and subscription bookkeeping.
#[derive(Debug, Default)]
pub struct ArkBus;

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::ArkBus;
    use zellij_tile::prelude::*;

    impl ZellijPlugin for ArkBus {
        /// Lifecycle breadcrumb — `load` runs once when zellij instantiates
        /// the plugin. T-6.2/T-6.3 will subscribe to the concrete event set
        /// and request the permissions needed to reach the supervisor
        /// control socket; for now this is intentionally a no-op so we can
        /// observe load() firing in the zellij log.
        fn load(&mut self, _configuration: std::collections::BTreeMap<String, String>) {
            eprintln!("{}: load", super::PLUGIN_NAME);
        }

        /// Lifecycle breadcrumb — `update` is called for every subscribed
        /// zellij event. Returning `false` tells zellij we have no pending
        /// render; T-6.3 will flip this to `true` when a forwarded event
        /// should trigger a UI refresh on our (still TBD) hidden pane.
        fn update(&mut self, _event: Event) -> bool {
            eprintln!("{}: update", super::PLUGIN_NAME);
            false
        }

        /// Lifecycle breadcrumb — zellij calls `render` when the plugin pane
        /// needs repainting. ark-bus is headless, so this stays empty; the
        /// plugin is expected to be hosted in a hidden/detached pane per
        /// cavekit-architecture.md (runtime section).
        fn render(&mut self, _rows: usize, _cols: usize) {
            eprintln!("{}: render", super::PLUGIN_NAME);
        }
    }

    register_plugin!(ArkBus);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host-side smoke test — `ArkBus::default()` must instantiate cleanly
    /// without touching the wasm-only `zellij_tile` host imports. This keeps
    /// `cargo test -p ark-bus` (host target) green and guards against a
    /// future regression where plugin state accidentally requires the wasm
    /// environment to construct.
    #[test]
    fn default_constructs_on_host() {
        let _bus = ArkBus::default();
    }

    /// Guard the registered plugin name — supervisors key `zellij pipe
    /// --name` dispatches against this string, so a silent rename would
    /// break the control-socket bridge when T-6.2 lands.
    #[test]
    fn plugin_name_is_stable() {
        assert_eq!(PLUGIN_NAME, "ark-bus");
    }
}
