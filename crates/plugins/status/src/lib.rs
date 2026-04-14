//! `ark-plugin-status` — zellij status-bar plugin for ark (scaffold).
//!
//! Satisfies `context/kits/cavekit-plugin-status.md` R1 acceptance criteria:
//!
//! - Crate name `ark-plugin-status` with `crate-type = ["cdylib"]` (see Cargo.toml).
//! - Build target `wasm32-wasip1` is driven by distribution wiring (T-098 / T-130);
//!   this scaffold only guarantees host-side `cargo check` and workspace
//!   `cargo build` cleanliness.
//! - Dependencies: `zellij-tile`, `serde`, `serde_json` (see Cargo.toml).
//! - `load()` calls `request_permission(&[PermissionType::ReadCliPipes])` and
//!   `subscribe(&[EventType::Timer, EventType::PermissionRequestResult])`.
//! - Plugin registers under the name `ark-status` (see [`PLUGIN_NAME`]) and is
//!   wired through [`zellij_tile::register_plugin!`].
//!
//! Everything else (pipe ingestion R2, render R3, distribution R4, filesystem
//! fallback) is intentionally stubbed — those land in T-095/T-096/T-097/T-098.
//!
//! # Target gating
//!
//! `zellij-tile`'s host shims call `extern "C" fn host_run_plugin_command`
//! imported from the wasm `zellij` module (`#[link(wasm_import_module = ...)]`).
//! On non-wasm targets that symbol is undefined and linking the cdylib fails.
//! The [`ZellijPlugin`] impl (and the `register_plugin!` expansion, which
//! calls into `host_*` shims) are therefore gated behind
//! `#[cfg(target_arch = "wasm32")]`. Host builds still compile this crate so
//! workspace-wide `cargo build` stays green; they just don't link the
//! wasm-only symbols.

use std::collections::BTreeMap;

/// Registered plugin name used by supervisors when targeting `zellij pipe --name`.
///
/// Supervisors publish agent status updates to this pipe target; see
/// cavekit-plugin-status R2. Defined as a constant so the dispatch side and the
/// ingestion filter share a single source of truth.
pub const PLUGIN_NAME: &str = "ark-status";

/// Per-agent status summary held by the plugin.
///
/// Fields are intentionally minimal for the R1 scaffold — T-095 fleshes this
/// out to match the pipe payload schema in cavekit-plugin-status R2 (id, name,
/// orchestrator, phase, progress, findings, stalled_since_secs).
#[derive(Debug, Default, Clone)]
pub struct StatusSummary {
    // Populated by T-095.
}

/// Root plugin state.
///
/// Holds the ordered map of agent id → latest [`StatusSummary`]. `BTreeMap`
/// (not `HashMap`) gives deterministic iteration order so renders are stable
/// across ticks — matches R2's "ordered → deterministic render" guidance.
#[derive(Debug, Default)]
pub struct Status {
    /// Agent id → latest status summary. Keyed by string for now; T-095 will
    /// swap to the real `AgentId` newtype from `ark-types` once the type is
    /// imported here (kept as `String` in the scaffold to avoid dragging
    /// host-only dependencies into the wasm build surface).
    #[allow(dead_code)]
    agents: BTreeMap<String, StatusSummary>,
}

#[cfg(target_arch = "wasm32")]
mod wasm_plugin {
    use super::Status;
    use zellij_tile::prelude::*;

    impl ZellijPlugin for Status {
        fn load(&mut self, _configuration: std::collections::BTreeMap<String, String>) {
            // R1: request only the minimal permission the plugin needs — read
            // incoming `zellij pipe` payloads from supervisors. Granted
            // asynchronously; the result arrives via
            // `EventType::PermissionRequestResult`.
            request_permission(&[PermissionType::ReadCliPipes]);

            // R1: subscribe to the 1 Hz timer (freshness ticks — R2 uses it
            // to redraw when no pipe message arrived) and to permission
            // results so we can react if the user denies the request.
            subscribe(&[EventType::Timer, EventType::PermissionRequestResult]);
        }

        fn update(&mut self, event: Event) -> bool {
            // Stub dispatcher — T-095 expands Timer handling (stale
            // detection, eviction) and T-096 triggers redraws. For the
            // scaffold we acknowledge the subscribed events and return
            // `false` (no redraw).
            match event {
                Event::Timer(_elapsed) => false,
                Event::PermissionRequestResult(_granted) => false,
                _ => false,
            }
        }

        fn pipe(&mut self, _pipe_message: PipeMessage) -> bool {
            // R2 ingestion stub — filled in by T-095.
            false
        }

        fn render(&mut self, _rows: usize, _cols: usize) {
            // R3 render stub — filled in by T-096.
        }
    }

    register_plugin!(Status);
}
