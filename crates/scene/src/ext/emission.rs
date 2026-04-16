//! Own-namespace-only emission policy (T-100).
//!
//! Extensions may only emit events in their own namespace. This module
//! validates that constraint at emit time, rejecting:
//!
//! - Cross-namespace events (`git.push` emitted by `lsp`).
//! - Any event in the reserved `ark.core.*` namespace.
//!
//! Unqualified event names (no dot) are allowed — they will be
//! auto-prefixed by the namespace pass before dispatch.

use crate::error::SceneError;

/// Reserved namespace prefix that extensions may never emit into.
const RESERVED_PREFIX: &str = "ark.core";

/// Validate that an extension only emits events in its own namespace.
///
/// # Rules
///
/// 1. `ark.core.*` events cannot be emitted by any extension.
/// 2. Qualified names (`foo.bar`) must start with `<extension_name>.`.
/// 3. Unqualified names (no dot) pass — the namespace pass will
///    auto-prefix them with `<extension_name>.` before dispatch.
///
/// # Errors
///
/// Returns [`SceneError::ExtReservedNamespace`] for `ark.core.*` events,
/// or [`SceneError::OpFailed`] for cross-namespace emission attempts.
pub fn validate_emission_namespace(
    event_name: &str,
    extension_name: &str,
) -> Result<(), SceneError> {
    // Rule 1: reject ark.core.* unconditionally.
    if event_name == RESERVED_PREFIX || event_name.starts_with(&format!("{RESERVED_PREFIX}.")) {
        return Err(SceneError::ExtReservedNamespace {
            ext: extension_name.to_string(),
            attempted: event_name.to_string(),
        });
    }

    // Rule 3: unqualified names (no dot) are fine — auto-prefixed later.
    if !event_name.contains('.') {
        return Ok(());
    }

    // Rule 2: qualified names must belong to the extension's own namespace.
    let own_prefix = format!("{extension_name}.");
    if !event_name.starts_with(&own_prefix) {
        return Err(SceneError::OpFailed {
            op: "emit".to_string(),
            message: format!(
                "extension `{extension_name}` cannot emit event `{event_name}`: \
                 cross-namespace emission is forbidden"
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Own namespace passes ──────────────────────────────────────

    #[test]
    fn own_namespace_passes() {
        assert!(validate_emission_namespace("git.push", "git").is_ok());
    }

    #[test]
    fn own_namespace_nested_passes() {
        assert!(validate_emission_namespace("git.hooks.pre-commit", "git").is_ok());
    }

    // ── Cross-namespace fails ─────────────────────────────────────

    #[test]
    fn cross_namespace_fails() {
        let err = validate_emission_namespace("git.push", "lsp").unwrap_err();
        match err {
            SceneError::OpFailed { op, message } => {
                assert_eq!(op, "emit");
                assert!(message.contains("cross-namespace"));
                assert!(message.contains("lsp"));
                assert!(message.contains("git.push"));
            }
            other => panic!("expected OpFailed, got: {other:?}"),
        }
    }

    // ── Unqualified passes (will be auto-prefixed) ────────────────

    #[test]
    fn unqualified_passes() {
        assert!(validate_emission_namespace("push", "git").is_ok());
    }

    #[test]
    fn unqualified_single_word_passes() {
        assert!(validate_emission_namespace("ready", "status").is_ok());
    }

    // ── ark.core.* always rejected ────────────────────────────────

    #[test]
    fn ark_core_rejected_by_any_extension() {
        let err = validate_emission_namespace("ark.core.ready", "status").unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { ext, attempted } => {
                assert_eq!(ext, "status");
                assert_eq!(attempted, "ark.core.ready");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    #[test]
    fn ark_core_exact_rejected() {
        let err = validate_emission_namespace("ark.core", "evil").unwrap_err();
        assert!(matches!(err, SceneError::ExtReservedNamespace { .. }));
    }

    #[test]
    fn ark_core_nested_rejected() {
        let err = validate_emission_namespace("ark.core.system.shutdown", "evil").unwrap_err();
        assert!(matches!(err, SceneError::ExtReservedNamespace { .. }));
    }

    // ── Edge cases ────────────────────────────────────────────────

    #[test]
    fn extension_name_prefix_match_not_substring() {
        // "gitter" should NOT be allowed to emit "git.push"
        let err = validate_emission_namespace("git.push", "gitter").unwrap_err();
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[test]
    fn empty_event_name_is_unqualified() {
        // Degenerate but harmless — no dot means unqualified.
        assert!(validate_emission_namespace("", "git").is_ok());
    }
}
