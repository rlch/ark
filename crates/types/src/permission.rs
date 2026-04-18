//! Shared permission-policy primitives for the legacy hook flow
//! (cavekit-hook-ipc R3).
//!
//! Under cavekit-soul Phase 1 the engine-side observer (`AgentEvent`
//! emitter, AgentId-keyed lookup) is gone — those concepts re-home
//! inside extensions in Phase 2+. The policy enum + on-disk wire
//! contract survive here as a small typed surface that
//! `ark-hook` and the future extension API both consume.
//!
//! ## Wire contract
//!
//! The engine writes a one-line file at
//! `$STATE/sessions/{id}/permission_policy` containing one of
//! `"ask"`, `"auto_approve_read"`, `"auto_approve_all"`.
//! Both sides use [`write_policy_file`] / [`read_policy_file`].
//! The reader is **fail-SAFE**: every error path (missing file,
//! unreadable file, garbage content) returns [`PermissionPolicy::Ask`]
//! — the most restrictive policy — so a broken policy file can
//! never silently downgrade to "auto-approve".

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Tools that are safe to auto-approve under
/// [`PermissionPolicy::AutoApproveRead`].
///
/// These are the read-only tools shipped by Claude Code: they inspect
/// files / the web but cannot mutate the workspace or run arbitrary code.
pub const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebFetch", "WebSearch"];

/// Filename (relative to the per-session state dir) that stores the policy.
pub const POLICY_FILE_NAME: &str = "permission_policy";

/// Decision returned by [`decide`].
///
/// Under cavekit-soul Phase 1 the rich `PermissionDecision` enum that
/// fed `AgentEvent::PermissionResolved` is gone; this is the small
/// stand-in the policy primitives need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Auto-approve the tool call without prompting.
    Allowed,
    /// Defer to the engine's own prompt — neither auto-approve nor deny.
    Deferred,
}

/// Policy applied to engine permission requests.
///
/// Wire form (file + config + serde): lowercase-underscore — `"ask"`,
/// `"auto_approve_read"`, `"auto_approve_all"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Never auto-approve. The engine's own prompt handles the request.
    Ask,
    /// Auto-approve read-only tools (see [`READ_ONLY_TOOLS`]); defer the
    /// rest to the engine's prompt.
    AutoApproveRead,
    /// Auto-approve every tool. Convenient for fully-autonomous runs.
    AutoApproveAll,
}

impl PermissionPolicy {
    /// Canonical wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionPolicy::Ask => "ask",
            PermissionPolicy::AutoApproveRead => "auto_approve_read",
            PermissionPolicy::AutoApproveAll => "auto_approve_all",
        }
    }
}

impl fmt::Display for PermissionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned by [`PermissionPolicy::from_str`] on an unrecognised value.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid permission policy `{0}`: expected ask | auto_approve_read | auto_approve_all")]
pub struct ParsePermissionPolicyError(pub String);

impl FromStr for PermissionPolicy {
    type Err = ParsePermissionPolicyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "ask" => Ok(PermissionPolicy::Ask),
            "auto_approve_read" => Ok(PermissionPolicy::AutoApproveRead),
            "auto_approve_all" => Ok(PermissionPolicy::AutoApproveAll),
            other => Err(ParsePermissionPolicyError(other.to_string())),
        }
    }
}

/// Apply the policy to a tool name.
pub fn decide(policy: PermissionPolicy, tool: &str) -> PolicyDecision {
    match policy {
        PermissionPolicy::Ask => PolicyDecision::Deferred,
        PermissionPolicy::AutoApproveRead => {
            if READ_ONLY_TOOLS.contains(&tool) {
                PolicyDecision::Allowed
            } else {
                PolicyDecision::Deferred
            }
        }
        PermissionPolicy::AutoApproveAll => PolicyDecision::Allowed,
    }
}

/// Write the policy to `{state_dir}/permission_policy` as a single line.
pub fn write_policy_file(state_dir: &Path, policy: PermissionPolicy) -> io::Result<()> {
    let path = state_dir.join(POLICY_FILE_NAME);
    fs::write(path, policy.as_str())
}

/// Read the policy from `{state_dir}/permission_policy`. **Fail-SAFE**.
pub fn read_policy_file(state_dir: &Path) -> PermissionPolicy {
    let path = state_dir.join(POLICY_FILE_NAME);
    match fs::read_to_string(&path) {
        Ok(s) => PermissionPolicy::from_str(s.trim()).unwrap_or(PermissionPolicy::Ask),
        Err(_) => PermissionPolicy::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn decide_ask_always_defers() {
        assert_eq!(
            decide(PermissionPolicy::Ask, "Edit"),
            PolicyDecision::Deferred
        );
        assert_eq!(
            decide(PermissionPolicy::Ask, "Read"),
            PolicyDecision::Deferred
        );
    }

    #[test]
    fn decide_auto_approve_read_allows_read_tools() {
        for t in READ_ONLY_TOOLS {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveRead, t),
                PolicyDecision::Allowed
            );
        }
    }

    #[test]
    fn decide_auto_approve_read_defers_writes() {
        for t in ["Edit", "Bash", "Write"] {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveRead, t),
                PolicyDecision::Deferred
            );
        }
    }

    #[test]
    fn decide_auto_approve_all_always_allows() {
        for t in ["Edit", "Read", "Bash"] {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveAll, t),
                PolicyDecision::Allowed
            );
        }
    }

    #[test]
    fn from_str_roundtrip_all_variants() {
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AutoApproveRead,
            PermissionPolicy::AutoApproveAll,
        ] {
            let s = policy.to_string();
            let back: PermissionPolicy = s.parse().expect("parse");
            assert_eq!(back, policy);
        }
    }

    #[test]
    fn from_str_rejects_invalid() {
        let err = "bogus".parse::<PermissionPolicy>().unwrap_err();
        assert_eq!(err, ParsePermissionPolicyError("bogus".to_string()));
    }

    #[test]
    fn write_then_read_policy_file_roundtrip() {
        let dir = tempdir().unwrap();
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AutoApproveRead,
            PermissionPolicy::AutoApproveAll,
        ] {
            write_policy_file(dir.path(), policy).unwrap();
            assert_eq!(read_policy_file(dir.path()), policy);
        }
    }

    #[test]
    fn read_policy_file_missing_defaults_to_ask() {
        let dir = tempdir().unwrap();
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_garbage_defaults_to_ask() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(POLICY_FILE_NAME), "definitely-not-valid").unwrap();
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }
}
