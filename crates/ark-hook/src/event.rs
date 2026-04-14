//! Hook event names accepted by `ark-hook --event`.
//!
//! Names match Claude Code's hook surface (cavekit-hook-ipc.md R2 lists
//! the per-event jsonl file names that mirror these). Kept as a small
//! enum so unknown events fail at clap parse time and downstream tasks
//! (T-047 translator, T-048 writer) can `match` exhaustively.

use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;

/// Claude Code hook event variants relevant to ark.
///
/// New variants land alongside the matching kit update — keep this list
/// in sync with R2's per-event JSONL file enumeration.
///
/// `value(name = "...")` pins the wire form to Claude Code's PascalCase
/// `hook_event_name` strings — clap's ValueEnum default of kebab-case
/// would otherwise mis-match the values Claude actually sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum HookEvent {
    #[value(name = "PostToolUse")]
    PostToolUse,
    #[value(name = "Stop")]
    Stop,
    #[value(name = "PermissionRequest")]
    PermissionRequest,
    #[value(name = "Notification")]
    Notification,
    #[value(name = "SessionEnd")]
    SessionEnd,
    #[value(name = "TaskCompleted")]
    TaskCompleted,
}

impl HookEvent {
    /// Canonical wire name (matches Claude Code's `hook_event_name`).
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::Stop => "Stop",
            HookEvent::PermissionRequest => "PermissionRequest",
            HookEvent::Notification => "Notification",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::TaskCompleted => "TaskCompleted",
        }
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// String parsing kept manual (rather than auto-derived) so error
/// messages name the offending value verbatim — clap also routes
/// through [`ValueEnum`] so this is mostly used by tests / future
/// payload validation.
impl FromStr for HookEvent {
    type Err = UnknownHookEvent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PostToolUse" => Ok(HookEvent::PostToolUse),
            "Stop" => Ok(HookEvent::Stop),
            "PermissionRequest" => Ok(HookEvent::PermissionRequest),
            "Notification" => Ok(HookEvent::Notification),
            "SessionEnd" => Ok(HookEvent::SessionEnd),
            "TaskCompleted" => Ok(HookEvent::TaskCompleted),
            other => Err(UnknownHookEvent(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown hook event: `{0}`")]
pub struct UnknownHookEvent(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_each_variant() {
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::PermissionRequest,
            HookEvent::Notification,
            HookEvent::SessionEnd,
            HookEvent::TaskCompleted,
        ] {
            let parsed: HookEvent = ev.as_str().parse().expect("round trip");
            assert_eq!(parsed, ev);
        }
    }

    #[test]
    fn unknown_rejected() {
        let err = "Bogus".parse::<HookEvent>().unwrap_err();
        assert!(err.to_string().contains("Bogus"));
    }
}
