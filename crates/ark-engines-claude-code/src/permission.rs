//! Permission policy enforcement for the Claude Code engine
//! (cavekit-engine-claude-code R3).
//!
//! The core policy primitives (`PermissionPolicy`, `READ_ONLY_TOOLS`,
//! `decide`, `emit_permission_events`, file helpers) live in
//! [`ark_types::permission`] so that `ark-hook` and this crate share a
//! single source of truth — see F-044 (codex P1) for the regression
//! that prompted the promotion. This module re-exports those primitives
//! so existing call sites keep compiling.
//!
//! # Integration contract with `ark-hook`
//!
//! The hook binary is a tiny, fast CLI that depends on `ark-types`
//! (not on this crate). During `install_observability` the engine
//! writes a one-line policy file at
//! `$STATE/agents/{id}/permission_policy`, containing the wire form of
//! [`PermissionPolicy`] (one of `"ask"`, `"auto_approve_read"`,
//! `"auto_approve_all"`). On each `PermissionRequest` the hook reads
//! that file, applies the same [`decide`] logic, and writes the
//! approval JSON back to stdout only when the decision is `Allowed`.
//!
//! The reader is **fail-SAFE**: any I/O or parse error falls back to
//! [`PermissionPolicy::Ask`] (the most conservative choice), so a
//! corrupted policy file can never silently downgrade to
//! auto-approve-everything.

pub use ark_types::permission::{
    POLICY_FILE_NAME, ParsePermissionPolicyError, PermissionPolicy, READ_ONLY_TOOLS, decide,
    emit_permission_events, read_policy_file, read_policy_for_agent, write_policy_file,
};

#[cfg(test)]
mod tests {
    //! The real tests live in `ark_types::permission`. These are smoke
    //! tests over the re-exports to confirm the engine crate's public
    //! surface still resolves.

    use super::*;
    use ark_types::PermissionDecision;

    #[test]
    fn reexported_decide_matches_ark_types() {
        assert_eq!(
            decide(PermissionPolicy::Ask, "Edit"),
            PermissionDecision::Deferred
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveAll, "Edit"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Read"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Edit"),
            PermissionDecision::Deferred
        );
    }

    #[test]
    fn reexported_read_only_tools_matches() {
        assert!(READ_ONLY_TOOLS.iter().any(|t| *t == "Read"));
        assert!(READ_ONLY_TOOLS.iter().any(|t| *t == "Grep"));
    }

    #[test]
    fn reexported_policy_file_read_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        write_policy_file(dir.path(), PermissionPolicy::AutoApproveRead).expect("write");
        assert_eq!(
            read_policy_file(dir.path()),
            PermissionPolicy::AutoApproveRead
        );
    }

    #[test]
    fn reexported_policy_file_missing_is_ask() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }
}
