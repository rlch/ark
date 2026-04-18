//! Core op vocabulary — R7 implementations of the canonical intents
//! (T-048..T-051).
//!
//! One module per subject grouping:
//!
//! * [`panes`]     — `focus`, `close`, `rename`, `resize`, `move`,
//!                   `pin`, `unpin`
//! * [`spawn`]     — `spawn` (tiled + overlay), `new_tab`
//! * [`messaging`] — `pipe`, `emit`, `set_status`
//! * [`control`]   — `exec`, `reload_scene`
//!
//! Each op is a zero-sized struct implementing [`crate::intent::Intent`];
//! they're registered into an [`crate::intent::IntentRegistry`] via
//! [`register_core_ops`].
//!
//! # Idempotency matrix (T-055)
//!
//! | Op            | Policy                               |
//! |---------------|--------------------------------------|
//! | `focus`       | noop on absent handle                |
//! | `close`       | noop on absent handle                |
//! | `rename`      | noop on absent handle                |
//! | `resize`      | noop on absent handle                |
//! | `move`        | noop on absent handle                |
//! | `pin`/`unpin` | noop on absent handle                |
//! | `spawn`       | check-then-create-else-focus         |
//! | `new_tab`     | check-then-create-else-focus         |
//! | `pipe`        | always side-effect                   |
//! | `emit`        | always side-effect                   |
//! | `set_status`  | always side-effect                   |
//! | `exec`        | always side-effect                   |
//! | `reload_scene`| noop when no reloader installed      |

pub mod control;
pub mod messaging;
pub mod panes;
pub mod spawn;

use std::sync::Arc;

use crate::intent::IntentRegistry;

/// Canonical ordered list of every `ark.core.*` op registered by
/// [`register_core_ops`].
///
/// Consumed by `ark scene check` cross-reference validation and
/// "did you mean?" suggestions so callers never reach into the
/// per-op modules for the name list.
pub const CORE_OP_NAMES: &[&str] = &[
    // Panes / tabs
    "ark.core.focus",
    "ark.core.close",
    "ark.core.rename",
    "ark.core.resize",
    "ark.core.move",
    "ark.core.pin",
    "ark.core.unpin",
    // Spawn
    "ark.core.spawn",
    "ark.core.new_tab",
    // Messaging
    "ark.core.pipe",
    "ark.core.emit",
    "ark.core.set_status",
    // Control
    "ark.core.exec",
    "ark.core.reload_scene",
];

/// Register every `ark.core.*` op into `registry`.
///
/// Called once at scene compile. Extension-contributed ops register
/// after this call so user scenes see the full vocabulary.
pub fn register_core_ops(registry: &mut IntentRegistry) {
    // Panes / tabs (T-048)
    registry.register("ark.core.focus", Arc::new(panes::FocusOp));
    registry.register("ark.core.close", Arc::new(panes::CloseOp));
    registry.register("ark.core.rename", Arc::new(panes::RenameOp));
    registry.register("ark.core.resize", Arc::new(panes::ResizeOp));
    registry.register("ark.core.move", Arc::new(panes::MoveOp));
    registry.register("ark.core.pin", Arc::new(panes::PinOp));
    registry.register("ark.core.unpin", Arc::new(panes::UnpinOp));

    // Spawn (T-049)
    registry.register("ark.core.spawn", Arc::new(spawn::SpawnOp));
    registry.register("ark.core.new_tab", Arc::new(spawn::NewTabOp));

    // Messaging (T-050)
    registry.register("ark.core.pipe", Arc::new(messaging::PipeOp));
    registry.register("ark.core.emit", Arc::new(messaging::EmitOp));
    registry.register("ark.core.set_status", Arc::new(messaging::SetStatusOp));

    // Control (T-051)
    registry.register("ark.core.exec", Arc::new(control::ExecOp));
    registry.register("ark.core.reload_scene", Arc::new(control::ReloadSceneOp));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_core_ops_populates_matrix() {
        let mut reg = IntentRegistry::new();
        register_core_ops(&mut reg);
        assert_eq!(reg.len(), CORE_OP_NAMES.len());
    }

    #[test]
    fn core_op_names_all_ark_prefixed() {
        for name in CORE_OP_NAMES {
            assert!(
                name.starts_with("ark.core."),
                "op {name:?} is not ark.core.* prefixed"
            );
        }
    }

    #[test]
    fn every_name_is_registered() {
        let mut reg = IntentRegistry::new();
        register_core_ops(&mut reg);
        let names: Vec<&str> = reg.names();
        for expected in CORE_OP_NAMES {
            assert!(
                names.contains(expected),
                "CORE_OP_NAMES entry {expected:?} missing from registry"
            );
        }
    }
}
