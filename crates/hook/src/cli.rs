//! Clap-derive CLI surface for `ark-hook`.
//!
//! The binary serves two distinct call sites (`cavekit-hook-ipc.md` R1):
//!
//! 1. **Legacy hook-event sidecar** — `ark-hook --id <SessionId> --event <EVENT_NAME>`,
//!    invoked by Claude Code's hook config. Reads stdin JSON, translates to
//!    `AgentEvent`s, persists JSONL, pipes to zellij. This is the original
//!    skeleton entry point and stays the *no-subcommand* default to keep
//!    Claude-Code-injected hook configs working unchanged.
//!
//! 2. **Scene bridge subcommands** (T-6.2, T-6.3, T-6.4 + ACP):
//!    - `ark-hook intent --id <SessionId> --json '<{name, args}>'` — connects
//!      to the per-agent control socket (`cavekit-hook-ipc.md` R5) and
//!      dispatches a named intent through the supervisor's
//!      [`ark_scene::intent::IntentRegistry`]. Used by `ark-bus` for
//!      keybind dispatch via the hidden-command-pane bridge.
//!    - `ark-hook emit --id <SessionId> --json '<{event, payload, source}>'` —
//!      broadcasts a synthetic `UserEvent` through the supervisor's event
//!      bus. Used by `ark-bus` for forwarding zellij pane-lifecycle events
//!      (T-6.3).
//!    - `ark-hook permit --id <SessionId> --request-id <str> --outcome <…>` —
//!      responds to an outstanding ACP `session/request_permission`. Used
//!      by picker plugin modals (T-ACP follow-ups).
//!
//! All bridge subcommands resolve `--id` from `$ARK_AGENT_ID` when omitted
//! (per R1 last bullet) — the supervisor sets that env var in every spawned
//! child process so a plugin running inside zellij can call us without
//! threading the agent id by hand.
//!
//! ## Why a single top-level binary?
//!
//! Splitting into `ark-hook`, `ark-bridge`, etc. would mean two binaries on
//! disk + two release artifacts to package. The cavekit kit calls out a
//! single `ark-hook` binary with subcommands — keeping the legacy invocation
//! as the no-subcommand default means existing hook configs do not have to
//! be rewritten.

use clap::{Args, Parser, Subcommand, ValueEnum};

use ark_types::SessionId;

use crate::event::HookEvent;

/// `ark-hook` invocation arguments.
///
/// Either:
/// - **legacy hook form**: omit subcommand, supply `--id` and `--event`.
///   Wired by Claude Code's `hooks` config — see the example in
///   `cavekit-hook-ipc.md`.
/// - **bridge subcommand form**: pick a subcommand under [`Command`].
///   Used by `ark-bus` and the picker for control-socket dispatch.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "ark-hook",
    about = "Claude Code hook sidecar + scene-bridge dispatcher (cavekit-hook-ipc R1)",
    long_about = None,
    version,
    // Legacy form has no subcommand. We make subcommands optional and
    // distinguish at runtime by whether `command` is `Some`.
    subcommand_required = false,
    arg_required_else_help = false,
)]
pub struct Cli {
    /// Optional bridge subcommand. When absent, the legacy hook-event
    /// flow runs and requires `--id` + `--event`.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Target agent id. Required when no subcommand is given (legacy
    /// hook flow). Validated via [`SessionId::from_str`].
    #[arg(long = "id", value_parser = parse_agent_id, global = false)]
    pub id: Option<SessionId>,

    /// Hook event name (e.g. `PostToolUse`, `Stop`, `PermissionRequest`).
    /// Required when no subcommand is given.
    #[arg(long = "event")]
    pub event: Option<HookEvent>,
}

impl Cli {
    /// Returns `true` if this invocation runs the legacy hook-event flow
    /// (no subcommand). Used by `main.rs` to choose between the
    /// hook-event pipeline and the bridge dispatcher.
    pub fn is_legacy_hook(&self) -> bool {
        self.command.is_none()
    }

    /// Resolve the legacy `--id` argument. Returns `None` only when the
    /// caller has used a subcommand instead.
    pub fn legacy_id(&self) -> Option<&SessionId> {
        self.id.as_ref()
    }

    /// Resolve the legacy `--event` argument.
    pub fn legacy_event(&self) -> Option<HookEvent> {
        self.event
    }

    /// Build a [`LegacyCli`] for the no-subcommand path. Returns `Err`
    /// (with a clap-style message) when either `--id` or `--event` is
    /// missing — this is the only post-parse validation in the legacy
    /// flow now that those fields are syntactically optional at the
    /// outer parser layer.
    pub fn into_legacy(&self) -> Result<LegacyCli, String> {
        let id = self
            .id
            .clone()
            .ok_or_else(|| "missing required argument: --id <SessionId>".to_string())?;
        let event = self
            .event
            .ok_or_else(|| "missing required argument: --event <EVENT_NAME>".to_string())?;
        Ok(LegacyCli { id, event })
    }
}

/// Strict (post-validation) shape of the legacy hook-event invocation.
///
/// The outer [`Cli`] keeps `--id` and `--event` syntactically optional so
/// the bridge subcommands (which have their own `--id`) can coexist under
/// a single binary. `Cli::into_legacy` performs the post-parse
/// requirement check and produces this struct, which is what
/// [`crate::run::run`] consumes.
#[derive(Debug, Clone)]
pub struct LegacyCli {
    /// Target agent id.
    pub id: SessionId,
    /// Hook event name.
    pub event: HookEvent,
}

/// Bridge subcommands implementing `cavekit-hook-ipc.md` R1 bullets 2–4.
///
/// All three connect to the per-agent unix socket at
/// `${runtime}/agents/{id}.sock` (R4 path scheme), send a single NDJSON
/// command, and exit `0` on `{ok: true}` / `1` otherwise. Stderr carries
/// human-readable error text so zellij hidden-command-pane log capture
/// surfaces failures back to the operator.
#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// `ark-hook intent --id <SessionId> --json '<{name, args}>'` — dispatch
    /// a named intent through the supervisor's intent registry (R5
    /// `Intent` command). Used by `ark-bus` for keybind dispatch.
    Intent(BridgeArgs),

    /// `ark-hook emit --id <SessionId> --json '<{event, payload, source}>'`
    /// — broadcast a synthetic `UserEvent` (R5 `Emit` command). Used by
    /// `ark-bus` to forward zellij pane-lifecycle events onto the ark
    /// event bus.
    Emit(BridgeArgs),

    /// `ark-hook permit --id <SessionId> --request-id <str> --outcome <…>`
    /// — respond to an outstanding ACP `session/request_permission` (R5
    /// `Permit` command). Used by picker plugin modals.
    Permit(PermitArgs),
}

/// Shared shape for `intent` and `emit`: agent-id + a single JSON string
/// passed verbatim down the control socket.
///
/// Keeping the JSON opaque at the CLI layer lets the supervisor own the
/// shape — the hook binary stays decoupled from
/// [`ark_scene::intent::IntentRegistry`]'s evolving args grammar.
#[derive(Debug, Clone, Args)]
pub struct BridgeArgs {
    /// Target agent id. Falls back to `$ARK_AGENT_ID` (set by the
    /// supervisor in every spawned child process) when omitted, per R1
    /// last bullet.
    #[arg(long = "id", value_parser = parse_agent_id)]
    pub id: Option<SessionId>,

    /// JSON document to send. The exact schema depends on the
    /// subcommand:
    ///
    /// * `intent` expects `{ "name": "<op-name>", "args": { … } }`.
    /// * `emit` expects `{ "event": "<UserEvent name>", "payload": { … },
    ///   "source": "<canonical-source>" }`.
    ///
    /// Validation happens on the supervisor side so this binary stays
    /// neutral.
    #[arg(long = "json")]
    pub json: String,
}

/// Args for `ark-hook permit`. Unlike `intent` / `emit` these fields are
/// fully typed at the CLI layer — `option_id` is the only optional bit.
#[derive(Debug, Clone, Args)]
pub struct PermitArgs {
    /// Target agent id. Falls back to `$ARK_AGENT_ID` when omitted.
    #[arg(long = "id", value_parser = parse_agent_id)]
    pub id: Option<SessionId>,

    /// ACP `session/request_permission` request id this response
    /// resolves.
    #[arg(long = "request-id")]
    pub request_id: String,

    /// Outcome to send back through ACP.
    #[arg(long = "outcome")]
    pub outcome: PermitOutcome,

    /// Optional `option_id` for ACP request flows that present a list of
    /// options (rather than the bare allow / reject pair).
    #[arg(long = "option-id")]
    pub option_id: Option<String>,
}

/// ACP permission outcome values accepted by the `permit` subcommand.
///
/// Mirrors the trio called out in `cavekit-hook-ipc.md` R1: a binary
/// allow / reject_once / reject_always vocabulary aligned with the ACP
/// `RequestPermissionOutcome` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum PermitOutcome {
    /// One-shot allow for this specific request.
    Allow,
    /// Reject this request only — a later request can re-prompt.
    RejectOnce,
    /// Reject this request and remember the rejection (the engine should
    /// not re-ask for an equivalent permission).
    RejectAlways,
}

impl PermitOutcome {
    /// Wire-format string sent over the control socket. Matches the
    /// canonical ACP outcome strings.
    pub fn as_wire(self) -> &'static str {
        match self {
            PermitOutcome::Allow => "allow",
            PermitOutcome::RejectOnce => "reject_once",
            PermitOutcome::RejectAlways => "reject_always",
        }
    }
}

/// Wrapper around `SessionId::parse` that returns a `String` error so
/// clap can render it.
fn parse_agent_id(raw: &str) -> Result<SessionId, String> {
    SessionId::parse(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    use ark_types::SessionId;

    fn fresh_id() -> SessionId {
        SessionId::new("hooktest")
    }

    #[test]
    fn parses_legacy_hook_form() {
        let id = fresh_id();
        let id_str = id.as_str();
        let cli = Cli::try_parse_from(["ark-hook", "--id", &id_str, "--event", "PostToolUse"])
            .expect("legacy form parses");
        assert!(cli.is_legacy_hook());
        assert_eq!(cli.legacy_id().unwrap(), &id);
        assert_eq!(cli.legacy_event(), Some(HookEvent::PostToolUse));
    }

    #[test]
    fn legacy_form_parses_each_known_event() {
        let id = fresh_id();
        let id_str = id.as_str();
        for ev in [
            "PostToolUse",
            "Stop",
            "PermissionRequest",
            "Notification",
            "SessionEnd",
            "TaskCompleted",
        ] {
            Cli::try_parse_from(["ark-hook", "--id", &id_str, "--event", ev])
                .unwrap_or_else(|e| panic!("event {ev} should parse: {e}"));
        }
    }

    #[test]
    fn rejects_unknown_event() {
        let id = fresh_id();
        let id_str = id.as_str();
        let err = Cli::try_parse_from([
            "ark-hook",
            "--id",
            &id_str,
            "--event",
            "TotallyMadeUpEvent",
        ])
        .expect_err("unknown event should be rejected at parse time");
        let msg = err.to_string();
        assert!(msg.contains("TotallyMadeUpEvent") || msg.to_lowercase().contains("invalid"));
    }

    #[test]
    fn intent_subcommand_parses_with_explicit_id() {
        let id = fresh_id();
        let id_str = id.as_str();
        let cli = Cli::try_parse_from([
            "ark-hook",
            "intent",
            "--id",
            &id_str,
            "--json",
            r#"{"name":"ark.core.open_tab","args":{}}"#,
        ])
        .expect("intent parses");
        assert!(!cli.is_legacy_hook());
        match cli.command.as_ref().unwrap() {
            Command::Intent(args) => {
                assert_eq!(args.id.as_ref(), Some(&id));
                assert!(args.json.contains("open_tab"));
            }
            other => panic!("expected Intent, got {other:?}"),
        }
    }

    #[test]
    fn intent_subcommand_id_is_optional() {
        // R1: `--id` falls back to `$ARK_AGENT_ID` so the supervisor's
        // env injection works without threading the id by hand.
        let cli = Cli::try_parse_from([
            "ark-hook",
            "intent",
            "--json",
            r#"{"name":"ark.core.ping","args":{}}"#,
        ])
        .expect("intent parses without --id");
        match cli.command.as_ref().unwrap() {
            Command::Intent(args) => assert!(args.id.is_none()),
            other => panic!("expected Intent, got {other:?}"),
        }
    }

    #[test]
    fn emit_subcommand_parses() {
        let cli = Cli::try_parse_from([
            "ark-hook",
            "emit",
            "--json",
            r#"{"event":"ark.zellij.pane_closed","payload":{},"source":"ext:ark-bus"}"#,
        ])
        .expect("emit parses");
        match cli.command.as_ref().unwrap() {
            Command::Emit(args) => assert!(args.json.contains("ark.zellij.pane_closed")),
            other => panic!("expected Emit, got {other:?}"),
        }
    }

    #[test]
    fn permit_subcommand_parses_outcome_variants() {
        for (raw, want) in [
            ("allow", PermitOutcome::Allow),
            ("reject_once", PermitOutcome::RejectOnce),
            ("reject_always", PermitOutcome::RejectAlways),
        ] {
            let cli = Cli::try_parse_from([
                "ark-hook",
                "permit",
                "--request-id",
                "req-1",
                "--outcome",
                raw,
            ])
            .unwrap_or_else(|e| panic!("permit {raw} should parse: {e}"));
            match cli.command.as_ref().unwrap() {
                Command::Permit(args) => {
                    assert_eq!(args.outcome, want);
                    assert_eq!(args.request_id, "req-1");
                }
                other => panic!("expected Permit, got {other:?}"),
            }
        }
    }

    #[test]
    fn permit_outcome_as_wire_matches_acp_strings() {
        assert_eq!(PermitOutcome::Allow.as_wire(), "allow");
        assert_eq!(PermitOutcome::RejectOnce.as_wire(), "reject_once");
        assert_eq!(PermitOutcome::RejectAlways.as_wire(), "reject_always");
    }

    #[test]
    fn missing_required_args_in_legacy_form_does_not_error_at_parse_time() {
        // With subcommands optional + legacy fields optional at the
        // type level, a bare `ark-hook` parses cleanly. The downstream
        // dispatch (in main.rs / run.rs) is responsible for rejecting
        // a legacy invocation with missing fields.
        let cli = Cli::try_parse_from(["ark-hook"]).expect("bare invocation parses");
        assert!(cli.is_legacy_hook());
        assert!(cli.legacy_id().is_none());
        assert!(cli.legacy_event().is_none());
    }
}
