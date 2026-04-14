//! Skeleton entry pipeline for `ark-hook`.
//!
//! Per cavekit-hook-ipc.md R1, T-046 is the **skeleton only**: parse
//! args, read a single JSON document from stdin, log the event, and
//! exit. Downstream tasks attach the real work:
//! - T-047 — typed payload parser (replaces the loose `serde_json::Value`)
//! - T-048 — JSONL writers under `$STATE/agents/{id}/hooks/`
//! - T-049 — zellij/picker pipe forwarders
//! - T-050 — PermissionRequest stdout payload + explicit-deny exit code 2
//! - T-051 — broader fail-open coverage
//!
//! ## Budget
//! Claude Code blocks its main loop while a hook runs (kit R1). The
//! `<200ms` cap is a **design** target, not a runtime kill switch, so
//! we measure-and-warn rather than cancel work mid-flight. [`run`]
//! captures an [`Instant`] at entry and emits a tracing event with the
//! elapsed millis when it returns; if elapsed exceeds [`HOOK_BUDGET_MS`]
//! the event is `warn` so it surfaces in normal RUST_LOG configs.
//!
//! ## Exit codes
//! `0` on success **and** on every error path. `2` is reserved for
//! explicit-deny in T-050 and is never produced by this skeleton.
//! [`run`] returns [`anyhow::Result`] so the binary's `main` can log
//! the error to stderr before exiting `0`.

use std::io::Read;
use std::time::Instant;

use anyhow::Context;
use tracing::{info, warn};

use crate::cli::Cli;
use crate::payload::{HookPayload, payload_to_events};

/// Hook running-time budget in milliseconds (cavekit-hook-ipc.md R1).
pub const HOOK_BUDGET_MS: u128 = 200;

/// Outcome of a single hook invocation. The skeleton always produces
/// `Allow`; T-050 introduces `Deny` for the explicit-deny exit-code-2
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    /// Default — claude proceeds. Maps to exit code 0.
    Allow,
}

impl HookOutcome {
    /// Process exit code for this outcome. Currently always `0`; the
    /// `2` (deny) value is owned by T-050.
    pub fn exit_code(self) -> i32 {
        match self {
            HookOutcome::Allow => 0,
        }
    }
}

/// Run the hook end-to-end against the supplied `cli` and stdin reader.
///
/// Skeleton behavior:
/// 1. Read all of `stdin` into a `String` (single-document protocol).
/// 2. If the buffer is empty, log a warning and return `Allow` (fail-open
///    per kit R3 — Claude must not stall on missing payloads).
/// 3. Otherwise parse the document as `serde_json::Value` (typed parsing
///    is T-047). Parse failures are logged and still return `Allow`.
/// 4. Log the elapsed time; warn if it exceeds [`HOOK_BUDGET_MS`].
///
/// Returning `Result` lets [`main`](crate) bubble unexpected errors to
/// the top-level handler, which logs them and still exits `0`.
pub fn run<R: Read>(cli: &Cli, mut stdin: R) -> anyhow::Result<HookOutcome> {
    let started = Instant::now();

    let mut buf = String::new();
    let read_result = stdin
        .read_to_string(&mut buf)
        .with_context(|| format!("read stdin for event {}", cli.event));

    if let Err(e) = read_result {
        // Fail-open: log and return Allow. Downstream T-050 may override
        // this for PermissionRequest by emitting an explicit allow JSON
        // payload to stdout.
        warn!(
            agent = %cli.id,
            event = %cli.event,
            error = %e,
            "stdin read failed; fail-open"
        );
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

    if buf.trim().is_empty() {
        warn!(
            agent = %cli.id,
            event = %cli.event,
            "stdin empty; fail-open (R3 — never block claude)"
        );
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

    // T-047: typed parse + translation. JSONL persistence (T-048) and
    // zellij pipe forwarding (T-049) still live downstream — this task
    // only emits the derived AgentEvents via tracing for observability.
    match serde_json::from_str::<HookPayload>(&buf) {
        Ok(payload) => {
            info!(
                agent = %cli.id,
                event = %cli.event,
                bytes = buf.len(),
                session_id = %payload.session_id,
                cwd = %payload.cwd.display(),
                tool_name = payload.tool_name.as_deref().unwrap_or(""),
                "hook payload parsed"
            );
            let events = payload_to_events(&payload, &cli.id, cli.event);
            for ev in &events {
                info!(
                    agent = %cli.id,
                    event = %cli.event,
                    kind = agent_event_kind(ev),
                    detail = %serde_json::to_string(ev).unwrap_or_default(),
                    "translated agent event"
                );
            }
            info!(
                agent = %cli.id,
                event = %cli.event,
                emitted = events.len(),
                "hook translation complete"
            );
        }
        Err(e) => {
            warn!(
                agent = %cli.id,
                event = %cli.event,
                error = %e,
                bytes = buf.len(),
                "stdin not valid HookPayload; fail-open"
            );
        }
    }

    log_budget(cli, started);
    Ok(HookOutcome::Allow)
}

/// Short static label for an [`AgentEvent`], matching the serde
/// `kind` discriminant. Kept local to `run` since logging is the only
/// consumer today.
fn agent_event_kind(ev: &ark_types::event::AgentEvent) -> &'static str {
    use ark_types::event::AgentEvent::*;
    match ev {
        Started { .. } => "started",
        TabOpened { .. } => "tab_opened",
        TabClosed { .. } => "tab_closed",
        Progress { .. } => "progress",
        TaskDone { .. } => "task_done",
        Iteration { .. } => "iteration",
        PhaseTransition { .. } => "phase_transition",
        ToolUse { .. } => "tool_use",
        Message { .. } => "message",
        FileEdited { .. } => "file_edited",
        ReviewComment { .. } => "review_comment",
        PermissionAsked { .. } => "permission_asked",
        PermissionResolved { .. } => "permission_resolved",
        Stall { .. } => "stall",
        Log { .. } => "log",
        Error { .. } => "error",
        Done { .. } => "done",
        _ => "unknown",
    }
}

fn log_budget(cli: &Cli, started: Instant) {
    let elapsed = started.elapsed();
    let ms = elapsed.as_millis();
    if ms > HOOK_BUDGET_MS {
        warn!(
            agent = %cli.id,
            event = %cli.event,
            elapsed_ms = ms,
            budget_ms = HOOK_BUDGET_MS,
            "hook exceeded budget"
        );
    } else {
        info!(
            agent = %cli.id,
            event = %cli.event,
            elapsed_ms = ms,
            budget_ms = HOOK_BUDGET_MS,
            "hook within budget"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    use ark_types::AgentId;

    use crate::cli::Cli;
    use crate::event::HookEvent;

    fn cli_for(event: HookEvent) -> Cli {
        Cli {
            id: AgentId::new("cavekit", "hooktest"),
            event,
        }
    }

    #[test]
    fn empty_stdin_fail_open_returns_allow() {
        let cli = cli_for(HookEvent::PostToolUse);
        let outcome = run(&cli, Cursor::new(Vec::<u8>::new())).expect("run ok");
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
    }

    #[test]
    fn whitespace_only_stdin_fail_open_returns_allow() {
        let cli = cli_for(HookEvent::Stop);
        let outcome = run(&cli, Cursor::new(b"   \n\t  \n".to_vec())).expect("run ok");
        assert_eq!(outcome, HookOutcome::Allow);
    }

    #[test]
    fn malformed_json_fails_open() {
        let cli = cli_for(HookEvent::PostToolUse);
        let outcome = run(&cli, Cursor::new(b"{not json".to_vec())).expect("run ok");
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
    }

    #[test]
    fn valid_json_returns_allow() {
        let cli = cli_for(HookEvent::PostToolUse);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse"}"#;
        let outcome = run(&cli, Cursor::new(payload.as_bytes().to_vec())).expect("run ok");
        assert_eq!(outcome, HookOutcome::Allow);
    }

    #[test]
    fn allow_outcome_exits_zero() {
        // Sanity guard against accidental exit-code drift; T-050 will add
        // a Deny variant returning 2, but the skeleton must never do so.
        assert_eq!(HookOutcome::Allow.exit_code(), 0);
    }
}
