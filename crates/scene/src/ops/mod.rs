//! Core op vocabulary — R7 ops 1–13 implemented as [`Intent`] impls.
//!
//! Grouped by subject across sibling modules:
//!
//! * [`tabs`]       — `open_tab`, `close_tab`, `rename_tab`, `focus_tab`
//! * [`panes`]      — `split_pane`, `close_pane`
//! * [`plugins`]    — `mount_plugin`, `unmount_plugin`
//! * [`messaging`]  — `pipe`, `emit`, `set_status`
//! * [`control`]    — `exec`, `reload_scene`
//!
//! ACP-interaction ops (14–17 — `prompt`, `acp/cancel`, `acp/permit`,
//! `set_mode`) land in Tier-ACP, not here. Runtime-template rendering of
//! string args (T-4.4) and dispatch sequencing (T-4.5) live next to these
//! modules; op cross-reference validation (T-4.3) lives in [`validate`].
//!
//! # Idempotency policy (T-4.5)
//!
//! Per-op semantics, documented here so the full matrix is visible at a
//! glance:
//!
//! | Op              | Policy                                    |
//! |-----------------|-------------------------------------------|
//! | `open_tab`      | if-absent-focus-else-create               |
//! | `close_tab`     | idempotent-noop-on-absent                 |
//! | `rename_tab`    | idempotent-noop-on-absent                 |
//! | `focus_tab`     | idempotent-noop-on-absent                 |
//! | `split_pane`    | always-side-effect                        |
//! | `close_pane`    | idempotent-noop-on-absent                 |
//! | `mount_plugin`  | launch-or-focus (zellij primitive)        |
//! | `unmount_plugin`| idempotent-noop-on-absent                 |
//! | `pipe`          | always-side-effect                        |
//! | `emit`          | always-side-effect                        |
//! | `set_status`    | always-side-effect                        |
//! | `exec`          | always-side-effect                        |
//! | `reload_scene`  | idempotent-noop-on-absent (single-slot)   |
//!
//! Per-op `if_exists="focus|create|error"` override is deferred to v0.2
//! per the R7 / T-4.5 acceptance criterion.
//!
//! # Stub status
//!
//! Most ops are STUBS at this tier: they parse their typed args, log a
//! `tracing::info!` line, and return `Ok(None)`. The [`IntentContext`]
//! handles are placeholders ([`MuxPlaceholder`], [`EventBus`],
//! [`SupervisorHandle`] in `intent.rs`); real work lands when
//! Tier-5 wires the concrete handles in. Each stub carries a
//! `TODO(T-5.x)` / `TODO(real-handle)` marker pointing at the outstanding
//! work.
//!
//! Exceptions — these ops do real work today:
//!
//! * [`control::ExecOp`] spawns a subprocess via `tokio::process::Command`
//!   (no placeholder dependency).
//! * [`messaging::EmitOp`] enqueues a synthetic [`AgentEvent::UserEvent`]
//!   into a placeholder `Mutex<Vec<AgentEvent>>` on [`intent::EventBus`].
//!   The shape of the emitted event is real; the delivery mechanism is a
//!   stub. Tests pull the captured events out via
//!   [`intent::EventBus::drain_user_events`].
//!
//! [`IntentContext`]: crate::intent::IntentContext
//! [`MuxPlaceholder`]: crate::intent::MuxPlaceholder
//! [`EventBus`]: crate::intent::EventBus
//! [`SupervisorHandle`]: crate::intent::SupervisorHandle
//! [`AgentEvent::UserEvent`]: ark_types::event::AgentEvent::UserEvent
//! [`Intent`]: crate::intent::Intent

pub mod acp;
pub mod control;
pub mod dispatch;
pub mod messaging;
pub mod panes;
pub mod plugins;
pub mod render;
pub mod tabs;
pub mod validate;

use crate::intent::IntentRegistry;

/// Idempotency classification for an op (T-4.5 matrix).
///
/// Used by [`dispatch::dispatch_sequence`] telemetry and the
/// documentation table above. Kept as a tiny enum rather than a bool so
/// new categories (e.g. "cascade-throttled") can slot in without churning
/// every op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idempotency {
    /// "open the tab if missing, otherwise focus" (zellij primitive).
    IfAbsentFocusElseCreate,
    /// Skipped silently when the target doesn't exist.
    NoopOnAbsent,
    /// Always side-effects regardless of prior state.
    AlwaysSideEffect,
    /// Delegated to the underlying zellij primitive (`launch-or-focus-plugin`).
    LaunchOrFocus,
}

/// Register every core op (R7 #1–13) into `registry` under the
/// `ark.core.*` namespace.
///
/// Call once at session spawn; re-calling replaces registrations by op
/// name per [`IntentRegistry::register`]'s contract. Extension-contributed
/// ops register after this call so user scenes see the full vocabulary.
pub async fn register_core_ops(registry: &IntentRegistry) {
    // Tabs
    registry.register(tabs::OpenTabOp).await;
    registry.register(tabs::CloseTabOp).await;
    registry.register(tabs::RenameTabOp).await;
    registry.register(tabs::FocusTabOp).await;

    // Panes
    registry.register(panes::SplitPaneOp).await;
    registry.register(panes::ClosePaneOp).await;

    // Plugins
    registry.register(plugins::MountPluginOp).await;
    registry.register(plugins::UnmountPluginOp).await;

    // Messaging
    registry.register(messaging::PipeOp).await;
    registry.register(messaging::EmitOp).await;
    registry.register(messaging::SetStatusOp).await;

    // Control
    registry.register(control::ExecOp).await;
    registry.register(control::ReloadSceneOp).await;

    // ACP-interaction (R7 #14–17). See [`acp`] for the per-op
    // docs. These ops return `op/failed` with a clear "ACP client
    // not wired" message until T-ACP.4a installs a live client on
    // `IntentContext::acp`.
    registry.register(acp::PromptOp).await;
    registry.register(acp::CancelOp).await;
    registry.register(acp::PermitOp).await;
    registry.register(acp::SetModeOp).await;
}

/// Canonical ordered list of every core op NAME registered by
/// [`register_core_ops`].
///
/// Exposed so `ark scene check` (T-4.3 cross-ref pass) and docs can
/// enumerate the core surface without reaching into each op module.
pub const CORE_OP_NAMES: &[&str] = &[
    tabs::OpenTabOp::NAME,
    tabs::CloseTabOp::NAME,
    tabs::RenameTabOp::NAME,
    tabs::FocusTabOp::NAME,
    panes::SplitPaneOp::NAME,
    panes::ClosePaneOp::NAME,
    plugins::MountPluginOp::NAME,
    plugins::UnmountPluginOp::NAME,
    messaging::PipeOp::NAME,
    messaging::EmitOp::NAME,
    messaging::SetStatusOp::NAME,
    control::ExecOp::NAME,
    control::ReloadSceneOp::NAME,
    // ACP-interaction (R7 #14–17). Consumed by `ark scene check`
    // cross-reference validation same as the non-ACP ops above.
    acp::PromptOp::NAME,
    acp::CancelOp::NAME,
    acp::PermitOp::NAME,
    acp::SetModeOp::NAME,
];

// Pull op NAME constants into scope for `CORE_OP_NAMES`.
use crate::intent::Intent;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentRegistry;

    /// `register_core_ops` populates every R7 op slot — 17 in total
    /// (ops 1–13 plus ACP-interaction ops 14–17; T-ACP.2b).
    #[tokio::test]
    async fn register_core_ops_registers_seventeen() {
        let reg = IntentRegistry::new();
        register_core_ops(&reg).await;
        assert_eq!(reg.len().await, CORE_OP_NAMES.len());
        assert_eq!(CORE_OP_NAMES.len(), 17, "R7 ops 1-17 accounted for");
    }

    /// Every entry in `CORE_OP_NAMES` is `ark.core.*`-prefixed.
    #[test]
    fn core_op_names_are_namespaced() {
        for name in CORE_OP_NAMES {
            assert!(
                name.starts_with("ark.core."),
                "op {name:?} is not ark.core.* prefixed"
            );
        }
    }
}
