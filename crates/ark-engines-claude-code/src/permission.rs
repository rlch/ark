//! Permission policy enforcement for the Claude Code engine
//! (cavekit-engine-claude-code R3).
//!
//! The Claude Code CLI emits a `PermissionRequest` hook any time the model
//! wants to use a tool that isn't pre-allowed. The engine applies a simple
//! three-valued policy — configured via `config.engine.claude_code
//! .permission_policy` — to decide whether to auto-approve the request,
//! defer it to Claude's in-TUI prompt, or emit an "ask" trace for the UI.
//!
//! Per R3 *every* decision emits both `PermissionAsked` and
//! `PermissionResolved` events, regardless of which branch the policy took.
//! That invariant is important: downstream consumers (pane log, state
//! writer, hook dispatcher) rely on the paired events to render permission
//! traffic and drive the review UI.
//!
//! # Integration contract with `ark-hook`
//!
//! The hook binary is a tiny, fast CLI that must *not* depend on this crate.
//! Instead, during `install_observability` the engine writes a one-line
//! policy file at `$STATE/agents/{id}/permission_policy`, containing the
//! wire form of [`PermissionPolicy`] (one of `"ask"`, `"auto_approve_read"`,
//! or `"auto_approve_all"`). The hook reads that file on each
//! `PermissionRequest` payload, applies the same decision logic, and writes
//! the approval JSON back to stdout.
//!
//! Helpers in this module ([`write_policy_file`], [`read_policy_file`])
//! encapsulate the file-based contract so both sides stay in sync. The
//! reader is **fail-safe**: any I/O or parse error falls back to
//! [`PermissionPolicy::Ask`], the most conservative choice.
//!
//! # Why not reuse `ark-config`'s type?
//!
//! `ark-config::EngineClaudeCodeSection::permission_policy` is typed as a
//! plain `String` today. We keep our enum independent so this crate does
//! not depend on `ark-config`; callers that hold a `Config` can parse the
//! string field via [`PermissionPolicy::from_str`] at the boundary.
//!
//! # Per-agent overrides
//!
//! R3's final bullet — `runner_config.permission_policy` override — is
//! explicitly marked "future, not v1" in the kit and is **not** implemented
//! here. The policy file always reflects the config-level value.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use ark_types::{AgentEvent, AgentId, EventSink, PermissionDecision};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Tools that are safe to auto-approve under `auto_approve_read`.
///
/// These are the read-only tools shipped by Claude Code: they inspect files
/// / the web but cannot mutate the workspace or run arbitrary code.
pub const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebFetch", "WebSearch"];

/// Policy applied to Claude's `PermissionRequest` hook payloads.
///
/// Wire form (file + config + serde): lowercase-underscore — `"ask"`,
/// `"auto_approve_read"`, `"auto_approve_all"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    /// Never auto-approve. Claude's in-TUI prompt handles the request; the
    /// engine only emits trace events so the pane log can show activity.
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
/// Returns the engine's decision for the hook. Note that a `Deferred`
/// result is *not* a denial — it means "let Claude's own prompt handle it";
/// the hook should emit no approval payload.
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
/// Per R3 both events fire on *every* decision, regardless of which branch
/// of the policy was taken. Errors from the broadcast channel (no
/// subscribers) are ignored — permission tracing is best-effort.
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

// ---------------------------------------------------------------------------
// Policy-file helpers for the ark-hook contract.
// ---------------------------------------------------------------------------

/// Filename (relative to the per-agent state dir) that stores the policy.
const POLICY_FILE_NAME: &str = "permission_policy";

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
/// Returns [`PermissionPolicy::Ask`] — the most conservative policy — if
/// the file is missing or contains an unrecognised value. This matches the
/// fail-safe contract the hook relies on: when in doubt, never
/// auto-approve.
pub fn read_policy_file(state_dir: &Path) -> io::Result<PermissionPolicy> {
    let path = state_dir.join(POLICY_FILE_NAME);
    match fs::read_to_string(&path) {
        Ok(s) => Ok(PermissionPolicy::from_str(s.trim()).unwrap_or(PermissionPolicy::Ask)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PermissionPolicy::Ask),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::event_bus;
    use tempfile::tempdir;
    use tokio::sync::broadcast::error::TryRecvError;

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
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Read"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Grep"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "WebFetch"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Glob"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "WebSearch"),
            PermissionDecision::Allowed
        );
    }

    #[test]
    fn decide_auto_approve_read_defers_writes() {
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Edit"),
            PermissionDecision::Deferred
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Bash"),
            PermissionDecision::Deferred
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveRead, "Write"),
            PermissionDecision::Deferred
        );
    }

    #[test]
    fn decide_auto_approve_all_always_allows() {
        assert_eq!(
            decide(PermissionPolicy::AutoApproveAll, "Edit"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveAll, "Read"),
            PermissionDecision::Allowed
        );
        assert_eq!(
            decide(PermissionPolicy::AutoApproveAll, "Bash"),
            PermissionDecision::Allowed
        );
    }

    // ---- emit_permission_events ------------------------------------------

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

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

        // Exactly two events.
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            other => panic!("expected channel empty, got {other:?}"),
        }
    }

    #[test]
    fn emit_fires_both_events_even_when_allowed() {
        // R3 invariant: *every* decision emits both events.
        let (tx, mut rx) = event_bus::default_channel();
        emit_permission_events(&tx, &sample_id(), "Read", PermissionDecision::Allowed);

        assert!(matches!(
            rx.try_recv(),
            Ok(AgentEvent::PermissionAsked { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentEvent::PermissionResolved {
                decision: PermissionDecision::Allowed,
                ..
            })
        ));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn emit_no_subscribers_is_silent() {
        let (tx, rx) = event_bus::default_channel();
        drop(rx);
        // Must not panic even with zero subscribers.
        emit_permission_events(&tx, &sample_id(), "Edit", PermissionDecision::Deferred);
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
        // Wire form sanity check.
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
            let back = read_policy_file(dir.path()).expect("read");
            assert_eq!(back, policy);
        }
    }

    #[test]
    fn read_policy_file_missing_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        // File does not exist yet.
        let policy = read_policy_file(dir.path()).expect("read");
        assert_eq!(policy, PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_garbage_defaults_to_ask() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("permission_policy"), "definitely-not-valid").expect("seed");
        let policy = read_policy_file(dir.path()).expect("read");
        assert_eq!(policy, PermissionPolicy::Ask);
    }

    #[test]
    fn read_policy_file_tolerates_trailing_newline() {
        let dir = tempdir().expect("tmpdir");
        std::fs::write(dir.path().join("permission_policy"), "auto_approve_all\n").expect("seed");
        let policy = read_policy_file(dir.path()).expect("read");
        assert_eq!(policy, PermissionPolicy::AutoApproveAll);
    }
}
