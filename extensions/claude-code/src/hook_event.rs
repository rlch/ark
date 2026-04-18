//! Claude Code hook event names (T-004 salvage).
//!
//! Salvaged from the pre-2026-04-18 `crates/hook/src/event.rs`; adapted
//! for cavekit-claude-code R1 + R3:
//!
//! - **Scope expanded** from 6 to 10 variants. The legacy crate only
//!   forwarded a subset; the claude-code extension needs every hook
//!   Claude Code actually fires (9 hook events in the Claude Code
//!   docs: `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
//!   `PreToolUse`, `PostToolUse`, `SubagentStart`, `SubagentStop`,
//!   `Stop`, `PreCompact`, `Notification`). Note: R3's table in the kit
//!   lists 10 distinct `claude-code.*` kinds once `Notification` is
//!   counted — keep this enum and that table in sync at every R3
//!   revision.
//! - **Wire names** (via `#[value(name = "...")]` / `as_str`) stay in
//!   Claude Code's PascalCase `hook_event_name` form so clap's
//!   `ValueEnum` parser accepts the raw argv Claude passes unchanged.
//! - **Ext-event kinds** (via [`HookEvent::ext_kind`]) are the lower
//!   snake/dotted strings per R3:
//!   `session.start`, `session.end`, `user.prompt-submit`,
//!   `pre-tool-use`, `post-tool-use`, `subagent.start`,
//!   `subagent.stop`, `stop`, `pre-compact`, `notification`.
//!   [`hook_payload::payload_to_ext_event`][super::hook_payload::payload_to_ext_event]
//!   prepends `claude-code.` to produce the final `<ext>.<kind>` the
//!   core bus emits.
//!
//! Kept as a small enum so unknown events fail at clap parse time and
//! downstream translators can `match` exhaustively.

use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;

/// Claude Code hook event variants recognised by `cc-hook` (R1).
///
/// `#[value(name = "...")]` pins the wire form to Claude Code's
/// PascalCase `hook_event_name` strings — clap's `ValueEnum` default of
/// kebab-case would otherwise mis-match the values Claude actually
/// sends on the command line / in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum HookEvent {
    /// `SessionStart` → `claude-code.session.start`
    #[value(name = "SessionStart")]
    SessionStart,
    /// `SessionEnd` → `claude-code.session.end`
    #[value(name = "SessionEnd")]
    SessionEnd,
    /// `UserPromptSubmit` → `claude-code.user.prompt-submit`
    #[value(name = "UserPromptSubmit")]
    UserPromptSubmit,
    /// `PreToolUse` → `claude-code.pre-tool-use`
    #[value(name = "PreToolUse")]
    PreToolUse,
    /// `PostToolUse` → `claude-code.post-tool-use`
    #[value(name = "PostToolUse")]
    PostToolUse,
    /// `SubagentStart` → `claude-code.subagent.start`
    #[value(name = "SubagentStart")]
    SubagentStart,
    /// `SubagentStop` → `claude-code.subagent.stop`
    #[value(name = "SubagentStop")]
    SubagentStop,
    /// `Stop` → `claude-code.stop`
    #[value(name = "Stop")]
    Stop,
    /// `PreCompact` → `claude-code.pre-compact`
    #[value(name = "PreCompact")]
    PreCompact,
    /// `Notification` → `claude-code.notification`
    #[value(name = "Notification")]
    Notification,
}

impl HookEvent {
    /// Canonical Claude Code wire name (matches `hook_event_name`).
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::Stop => "Stop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::Notification => "Notification",
        }
    }

    /// R3 kind string — the `<kind>` half of `claude-code.<kind>`.
    ///
    /// Pair with [`crate::hook_payload::EXT_NAME`] / the `ExtEvent.ext`
    /// field to build the full `<ext>.<kind>` dotted name that flows
    /// through the core bus.
    pub fn ext_kind(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session.start",
            HookEvent::SessionEnd => "session.end",
            HookEvent::UserPromptSubmit => "user.prompt-submit",
            HookEvent::PreToolUse => "pre-tool-use",
            HookEvent::PostToolUse => "post-tool-use",
            HookEvent::SubagentStart => "subagent.start",
            HookEvent::SubagentStop => "subagent.stop",
            HookEvent::Stop => "stop",
            HookEvent::PreCompact => "pre-compact",
            HookEvent::Notification => "notification",
        }
    }

    /// Every known variant in enumeration order. Handy for tests +
    /// `cc-hook install-hooks` settings.json reconciliation.
    pub const ALL: &'static [HookEvent] = &[
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::UserPromptSubmit,
        HookEvent::PreToolUse,
        HookEvent::PostToolUse,
        HookEvent::SubagentStart,
        HookEvent::SubagentStop,
        HookEvent::Stop,
        HookEvent::PreCompact,
        HookEvent::Notification,
    ];
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// String parsing kept manual (rather than auto-derived) so error
/// messages name the offending value verbatim. Clap also routes through
/// [`ValueEnum`] so this is mostly used by tests / payload validation.
impl FromStr for HookEvent {
    type Err = UnknownHookEvent;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SessionStart" => Ok(HookEvent::SessionStart),
            "SessionEnd" => Ok(HookEvent::SessionEnd),
            "UserPromptSubmit" => Ok(HookEvent::UserPromptSubmit),
            "PreToolUse" => Ok(HookEvent::PreToolUse),
            "PostToolUse" => Ok(HookEvent::PostToolUse),
            "SubagentStart" => Ok(HookEvent::SubagentStart),
            "SubagentStop" => Ok(HookEvent::SubagentStop),
            "Stop" => Ok(HookEvent::Stop),
            "PreCompact" => Ok(HookEvent::PreCompact),
            "Notification" => Ok(HookEvent::Notification),
            other => Err(UnknownHookEvent(other.to_string())),
        }
    }
}

/// Error returned by [`HookEvent::from_str`] on an unrecognised wire
/// name. The offending value rides along for diagnostics.
#[derive(Debug)]
pub struct UnknownHookEvent(pub String);

impl fmt::Display for UnknownHookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown hook event: `{}`", self.0)
    }
}

impl std::error::Error for UnknownHookEvent {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_each_variant() {
        for ev in HookEvent::ALL {
            let parsed: HookEvent = ev.as_str().parse().expect("round trip");
            assert_eq!(parsed, *ev);
        }
    }

    #[test]
    fn unknown_rejected() {
        let err = "Bogus".parse::<HookEvent>().unwrap_err();
        assert!(err.to_string().contains("Bogus"));
    }

    #[test]
    fn ext_kind_matches_r3_table() {
        // Pin every mapping — drift here breaks R3 envelope tests and
        // the Rhai `on "claude-code.<kind>"` scene surface.
        let want: &[(HookEvent, &str)] = &[
            (HookEvent::SessionStart, "session.start"),
            (HookEvent::SessionEnd, "session.end"),
            (HookEvent::UserPromptSubmit, "user.prompt-submit"),
            (HookEvent::PreToolUse, "pre-tool-use"),
            (HookEvent::PostToolUse, "post-tool-use"),
            (HookEvent::SubagentStart, "subagent.start"),
            (HookEvent::SubagentStop, "subagent.stop"),
            (HookEvent::Stop, "stop"),
            (HookEvent::PreCompact, "pre-compact"),
            (HookEvent::Notification, "notification"),
        ];
        for (ev, kind) in want {
            assert_eq!(ev.ext_kind(), *kind);
        }
    }

    #[test]
    fn all_covers_every_variant() {
        // If a variant is added without ALL being updated, the count
        // drifts silently — keep HookEvent::ALL.len() wired to a pin.
        assert_eq!(HookEvent::ALL.len(), 10);
    }
}
