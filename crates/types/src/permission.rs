//! Shared permission-policy primitives for the legacy hook flow
//! (cavekit-hook-ipc R3).
//!
//! Under T-ACP.7 the engine-side writer (formerly
//! `ark-engines-claude-code`) is gone and ACP's
//! `session/request_permission` replaces the write-to-disk policy
//! hand-off for ACP-speaking engines. `ark-hook` still consumes these
//! primitives for existing (non-ACP) hook payloads, so the types
//! remain in `ark-types` for compat. Duplicating the enum in two
//! crates caused F-044 in the pre-T-ACP.7 codebase; keeping it here
//! prevents that class of drift from returning.
//!
//! ## Wire contract
//!
//! The engine writes a one-line file at
//! `$STATE/agents/{id}/permission_policy` containing one of
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

use crate::event::{AgentEvent, PermissionDecision};
use crate::event_bus::EventSink;
use crate::id::AgentId;

/// Tools that are safe to auto-approve under
/// [`PermissionPolicy::AutoApproveRead`].
///
/// These are the read-only tools shipped by Claude Code: they inspect
/// files / the web but cannot mutate the workspace or run arbitrary code.
pub const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebFetch", "WebSearch"];

/// Filename (relative to the per-agent state dir) that stores the policy.
pub const POLICY_FILE_NAME: &str = "permission_policy";

/// Policy applied to Claude's `PermissionRequest` hook payloads.
///
/// Wire form (file + config + serde): lowercase-underscore — `"ask"`,
/// `"auto_approve_read"`, `"auto_approve_all"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Never auto-approve. Claude's in-TUI prompt handles the request;
    /// observers still emit trace events so the pane log can show
    /// activity.
    Ask,
    /// Auto-approve read-only tools (see [`READ_ONLY_TOOLS`]); defer the
    /// rest to Claude's prompt.
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
///
/// Returns the decision for the hook. Note that a `Deferred` result is
/// *not* a denial — it means "let Claude's own prompt handle it"; the
/// hook should emit no approval payload in that case.
pub fn decide(policy: PermissionPolicy, tool: &str) -> PermissionDecision {
    match policy {
        PermissionPolicy::Ask => PermissionDecision::Deferred,
        PermissionPolicy::AutoApproveRead => {
            if READ_ONLY_TOOLS.iter().any(|t| *t == tool) {
                PermissionDecision::Allowed
            } else {
                PermissionDecision::Deferred
            }
        }
        PermissionPolicy::AutoApproveAll => PermissionDecision::Allowed,
    }
}

/// Emit the always-on `PermissionAsked` + `PermissionResolved` trace pair.
///
/// Per R3 both events fire on *every* decision, regardless of which
/// branch of the policy was taken. Errors from the broadcast channel
/// (no subscribers) are ignored — permission tracing is best-effort.
pub fn emit_permission_events(
    tx: &EventSink,
    id: &AgentId,
    tool: &str,
    decision: PermissionDecision,
) {
    let _ = tx.send(AgentEvent::PermissionAsked {
        id: id.clone(),
        tool: tool.to_string(),
        summary: format!("policy check for {tool}"),
    });
    let _ = tx.send(AgentEvent::PermissionResolved {
        id: id.clone(),
        tool: tool.to_string(),
        decision,
    });
}

/// Write the policy to `{state_dir}/permission_policy` as a single line.
///
/// The parent directory must already exist (the engine creates it during
/// `install_observability`).
pub fn write_policy_file(state_dir: &Path, policy: PermissionPolicy) -> io::Result<()> {
    let path = state_dir.join(POLICY_FILE_NAME);
    fs::write(path, policy.as_str())
}

/// Read the policy from `{state_dir}/permission_policy`.
///
/// **Fail-SAFE contract** (F-044, T-054 R4): every error — missing file,
/// unreadable file, permission-denied, garbage content — returns
/// [`PermissionPolicy::Ask`], the most restrictive policy. Callers can
/// therefore treat a successful read as authoritative and a corrupted
/// file as a signal to prompt the user rather than silently
/// auto-approving. The function never surfaces an `Err` — errors are
/// logged (by the caller) but converted to `Ask`.
pub fn read_policy_file(state_dir: &Path) -> PermissionPolicy {
    let path = state_dir.join(POLICY_FILE_NAME);
    match fs::read_to_string(&path) {
        Ok(s) => PermissionPolicy::from_str(s.trim()).unwrap_or(PermissionPolicy::Ask),
        Err(_) => PermissionPolicy::Ask,
    }
}

/// Resolve the policy for a given agent under a state base.
///
/// Convenience wrapper: `{state_base}/agents/{id}/permission_policy`.
pub fn read_policy_for_agent(state_base: &Path, id: &AgentId) -> PermissionPolicy {
    read_policy_file(&id.state_dir(state_base))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus;
    use tempfile::tempdir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    // ---- decide() ---------------------------------------------------------

    #[test]
    fn decide_ask_always_defers() {
        assert_eq!(
            decide(PermissionPolicy::Ask, "Edit"),
            PermissionDecision::Deferred
        );
        assert_eq!(
            decide(PermissionPolicy::Ask, "Read"),
            PermissionDecision::Deferred
        );
    }

    #[test]
    fn decide_auto_approve_read_allows_read_tools() {
        for t in READ_ONLY_TOOLS {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveRead, t),
                PermissionDecision::Allowed,
                "tool={t}"
            );
        }
    }

    #[test]
    fn decide_auto_approve_read_defers_writes() {
        for t in ["Edit", "Bash", "Write", "NotebookEdit", "MultiEdit"] {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveRead, t),
                PermissionDecision::Deferred,
                "tool={t}"
            );
        }
    }

    #[test]
    fn decide_auto_approve_all_always_allows() {
        for t in ["Edit", "Read", "Bash"] {
            assert_eq!(
                decide(PermissionPolicy::AutoApproveAll, t),
                PermissionDecision::Allowed,
                "tool={t}"
            );
        }
    }

    // ---- FromStr / Display / serde ---------------------------------------

    #[test]
    fn from_str_roundtrip_all_variants() {
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AutoApproveRead,
            PermissionPolicy::AutoApproveAll,
        ] {
            let s = policy.to_string();
            let back: PermissionPolicy = s.parse().expect("parse");
            assert_eq!(back, policy, "roundtrip for {s}");
        }
    }

    #[test]
    fn display_matches_wire_form() {
        assert_eq!(PermissionPolicy::Ask.to_string(), "ask");
        assert_eq!(
            PermissionPolicy::AutoApproveRead.to_string(),
            "auto_approve_read"
        );
        assert_eq!(
            PermissionPolicy::AutoApproveAll.to_string(),
            "auto_approve_all"
        );
    }

    #[test]
    fn from_str_trims_whitespace() {
        assert_eq!(
            "  ask\n".parse::<PermissionPolicy>().unwrap(),
            PermissionPolicy::Ask
        );
    }

    #[test]
    fn from_str_rejects_invalid() {
        let err = "bogus".parse::<PermissionPolicy>().unwrap_err();
        assert_eq!(err, ParsePermissionPolicyError("bogus".to_string()));
    }

    #[test]
    fn serde_roundtrip_all_variants() {
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AutoApproveRead,
            PermissionPolicy::AutoApproveAll,
        ] {
            let json = serde_json::to_string(&policy).expect("ser");
            let back: PermissionPolicy = serde_json::from_str(&json).expect("de");
            assert_eq!(back, policy, "serde roundtrip for {policy}");
        }
        assert_eq!(
            serde_json::to_string(&PermissionPolicy::AutoApproveRead).unwrap(),
            "\"auto_approve_read\""
        );
    }

    // ---- read/write policy file ------------------------------------------

    #[test]
    fn write_then_read_policy_file_roundtrip() {
        let dir = tempdir().expect("tmpdir");
        for policy in [
            PermissionPolicy::Ask,
            PermissionPolicy::AutoApproveRead,
            PermissionPolicy::AutoApproveAll,
        ] {
            write_policy_file(dir.path(), policy).expect("write");
            let back = read_policy_file(dir.path());
            assert_eq!(back, policy);
        }
    }

    #[test]
    fn read_policy_file_missing_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_garbage_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join(POLICY_FILE_NAME), "definitely-not-valid").expect("seed");
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_empty_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join(POLICY_FILE_NAME), "").expect("seed");
        assert_eq!(read_policy_file(dir.path()), PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_tolerates_trailing_newline() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join(POLICY_FILE_NAME), "auto_approve_all\n").expect("seed");
        assert_eq!(
            read_policy_file(dir.path()),
            PermissionPolicy::AutoApproveAll
        );
    }

    #[test]
    fn read_policy_for_agent_uses_agent_subdir() {
        let dir = tempdir().expect("tmpdir");
        let id = sample_id();
        let agent_dir = id.state_dir(dir.path());
        std::fs::create_dir_all(&agent_dir).expect("mkdir");
        write_policy_file(&agent_dir, PermissionPolicy::AutoApproveAll).expect("write");
        assert_eq!(
            read_policy_for_agent(dir.path(), &id),
            PermissionPolicy::AutoApproveAll
        );
    }

    #[test]
    fn read_policy_for_agent_missing_agent_dir_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        let id = sample_id();
        // Agent dir never created.
        assert_eq!(
            read_policy_for_agent(dir.path(), &id),
            PermissionPolicy::Ask
        );
    }

    // ---- emit_permission_events ------------------------------------------

    #[test]
    fn emit_sends_exactly_asked_then_resolved() {
        let (tx, mut rx) = event_bus::default_channel();
        let id = sample_id();

        emit_permission_events(&tx, &id, "Edit", PermissionDecision::Deferred);

        match rx.try_recv().expect("first event") {
            AgentEvent::PermissionAsked {
                id: ev_id,
                tool,
                summary,
            } => {
                assert_eq!(ev_id, id);
                assert_eq!(tool, "Edit");
                assert_eq!(summary, "policy check for Edit");
            }
            other => panic!("expected PermissionAsked, got {other:?}"),
        }

        match rx.try_recv().expect("second event") {
            AgentEvent::PermissionResolved {
                id: ev_id,
                tool,
                decision,
            } => {
                assert_eq!(ev_id, id);
                assert_eq!(tool, "Edit");
                assert_eq!(decision, PermissionDecision::Deferred);
            }
            other => panic!("expected PermissionResolved, got {other:?}"),
        }

        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            other => panic!("expected channel empty, got {other:?}"),
        }
    }

    #[test]
    fn emit_no_subscribers_is_silent() {
        let (tx, rx) = event_bus::default_channel();
        drop(rx);
        emit_permission_events(&tx, &sample_id(), "Edit", PermissionDecision::Deferred);
    }
}
