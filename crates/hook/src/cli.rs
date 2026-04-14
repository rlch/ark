//! Clap-derive CLI surface for `ark-hook`.
//!
//! Mirrors the signature documented in cavekit-hook-ipc.md R1:
//! `ark-hook --id <AgentId> --event <EVENT_NAME>`.
//!
//! The `--id` value is parsed via [`ark_types::AgentId`]'s `FromStr`
//! impl so invalid ids fail at clap-parse time with a clear message.
//! The `--event` value is parsed into [`crate::HookEvent`] (a typed
//! enum over the documented Claude Code hook event names) so unknown
//! events are rejected up front rather than silently propagated.

use clap::Parser;

use ark_types::AgentId;

use crate::event::HookEvent;

/// `ark-hook` invocation arguments.
///
/// Wired by Claude Code's `hooks` config block — see the example in
/// cavekit-hook-ipc.md.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "ark-hook",
    about = "Claude Code hook sidecar — translates hook payloads into AgentEvents",
    long_about = None,
    version,
)]
pub struct Cli {
    /// Target agent id. String form `{orchestrator}-{name}-{ulid}` —
    /// validated via `AgentId::from_str`.
    #[arg(long = "id", value_parser = parse_agent_id)]
    pub id: AgentId,

    /// Hook event name (e.g. `PostToolUse`, `Stop`, `PermissionRequest`).
    #[arg(long = "event")]
    pub event: HookEvent,
}

/// Wrapper around `AgentId::from_str` that returns a `String` error so
/// clap can render it. Without this clap would require the error type
/// to implement specific traits we'd otherwise need to add to AgentId.
fn parse_agent_id(raw: &str) -> Result<AgentId, String> {
    raw.parse::<AgentId>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use ark_types::AgentId;

    fn fresh_id() -> AgentId {
        AgentId::new("cavekit", "hooktest")
    }

    #[test]
    fn parses_valid_args() {
        let id = fresh_id();
        let cli = Cli::try_parse_from(["ark-hook", "--id", id.as_str(), "--event", "PostToolUse"])
            .expect("valid args parse");
        assert_eq!(cli.id, id);
        assert_eq!(cli.event, HookEvent::PostToolUse);
    }

    #[test]
    fn parses_each_known_event() {
        let id = fresh_id();
        for ev in [
            "PostToolUse",
            "Stop",
            "PermissionRequest",
            "Notification",
            "SessionEnd",
            "TaskCompleted",
        ] {
            Cli::try_parse_from(["ark-hook", "--id", id.as_str(), "--event", ev])
                .unwrap_or_else(|e| panic!("event {ev} should parse: {e}"));
        }
    }

    #[test]
    fn rejects_invalid_agent_id() {
        let err = Cli::try_parse_from([
            "ark-hook",
            "--id",
            "not a real id",
            "--event",
            "PostToolUse",
        ])
        .expect_err("malformed id should be rejected");
        // Render error to a string and confirm it mentions the agent-id problem.
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("agent")
                || msg.to_lowercase().contains("unsafe")
                || msg.to_lowercase().contains("id"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn rejects_unknown_event() {
        let id = fresh_id();
        let err = Cli::try_parse_from([
            "ark-hook",
            "--id",
            id.as_str(),
            "--event",
            "TotallyMadeUpEvent",
        ])
        .expect_err("unknown event should be rejected at parse time");
        let msg = err.to_string();
        assert!(msg.contains("TotallyMadeUpEvent") || msg.to_lowercase().contains("invalid"));
    }

    #[test]
    fn missing_required_args_fail() {
        assert!(Cli::try_parse_from(["ark-hook"]).is_err());
        assert!(Cli::try_parse_from(["ark-hook", "--id", fresh_id().as_str()]).is_err());
        assert!(Cli::try_parse_from(["ark-hook", "--event", "Stop"]).is_err());
    }
}
