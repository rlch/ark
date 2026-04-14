//! Entry pipeline for `ark-hook`.
//!
//! Per cavekit-hook-ipc.md R1+R2+R3, one invocation does four things:
//! 1. Parse the stdin JSON into a [`HookPayload`] (T-047).
//! 2. Derive one or more [`AgentEvent`]s via [`payload_to_events`].
//! 3. Persist each derived event as a JSON line to
//!    `$STATE/agents/{id}/hooks/{EventName}.jsonl` (T-048).
//! 4. Forward each serialized event to the `ark-status` and `ark-picker`
//!    zellij pipe targets (T-049).
//! 5. For `PermissionRequest` events, unconditionally write the allow
//!    payload to stdout (T-050). Policy gating (`ask`/`auto_approve_*`)
//!    lands in T-054.
//!
//! Every post-parse failure is **fail-open** (R3): log a warning to
//! stderr and keep going. The only way this binary can exit non-zero
//! today is a clap argument-validation failure at launch. The future
//! `2` (explicit deny) is wired in T-054.
//!
//! ## Budget
//! Claude Code blocks its main loop while a hook runs (kit R1). The
//! `<200ms` cap is a design target, not a runtime kill switch. `run`
//! captures an [`Instant`] at entry and emits a tracing event with the
//! elapsed millis on exit; if elapsed exceeds [`HOOK_BUDGET_MS`] the
//! event is `warn` so it surfaces in normal `RUST_LOG` configs.
//!
//! ## I/O injection
//! `run` accepts `&mut impl Read` for stdin and `&mut impl Write` for
//! stdout so unit tests can drive both ends without touching real
//! descriptors. `main.rs` passes `io::stdin().lock()` and
//! `io::stdout().lock()`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use tracing::{info, warn};

use ark_types::EnvPaths;

use crate::allow::write_allow_payload;
use crate::cli::Cli;
use crate::event::HookEvent;
use crate::payload::{HookPayload, payload_to_events};
use crate::pipe::{TARGET_ARK_PICKER, TARGET_ARK_STATUS, pipe_to_zellij};
use crate::writer::append_event_jsonl;

/// Hook running-time budget in milliseconds (cavekit-hook-ipc.md R1).
pub const HOOK_BUDGET_MS: u128 = 200;

/// Outcome of a single hook invocation.
///
/// The skeleton produces `Allow` on every path. `Deny` is reserved for
/// T-054's explicit-deny permission policy (exit code 2); today's
/// PermissionRequest path is unconditional allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    /// Claude proceeds. Exit code 0.
    Allow,
}

impl HookOutcome {
    /// Process exit code for this outcome.
    pub fn exit_code(self) -> i32 {
        match self {
            HookOutcome::Allow => 0,
        }
    }
}

/// Run the hook end-to-end.
///
/// `stdin` is the Claude Code hook payload reader. `stdout` is the
/// writer the PermissionRequest allow payload is emitted to — for
/// unit tests we capture a `Vec<u8>`; the binary passes
/// `io::stdout().lock()`.
pub fn run<R: Read, W: Write>(
    cli: &Cli,
    mut stdin: R,
    mut stdout: W,
) -> anyhow::Result<HookOutcome> {
    run_with_state(cli, &mut stdin, &mut stdout, resolve_state_root())
}

/// Same as [`run`] but takes an explicit `state_root` — used by tests
/// to sandbox JSONL writes under a [`tempfile::TempDir`].
pub fn run_with_state<R: Read, W: Write>(
    cli: &Cli,
    stdin: &mut R,
    stdout: &mut W,
    state_root: Option<PathBuf>,
) -> anyhow::Result<HookOutcome> {
    let started = Instant::now();

    let mut buf = String::new();
    let read_result = stdin
        .read_to_string(&mut buf)
        .with_context(|| format!("read stdin for event {}", cli.event));

    if let Err(e) = read_result {
        // Fail-open for stdin read: log and continue.
        warn!(
            agent = %cli.id,
            event = %cli.event,
            error = %e,
            "stdin read failed; fail-open"
        );
        // T-050 R3: malformed-stdin on PermissionRequest still emits allow.
        maybe_emit_allow(cli, stdout);
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

    if buf.trim().is_empty() {
        warn!(
            agent = %cli.id,
            event = %cli.event,
            "stdin empty; fail-open (R3 — never block claude)"
        );
        maybe_emit_allow(cli, stdout);
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

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
                let serialized = serde_json::to_value(ev).unwrap_or_else(|_| {
                    serde_json::json!({
                        "kind": agent_event_kind(ev),
                        "serialize_failed": true
                    })
                });
                info!(
                    agent = %cli.id,
                    event = %cli.event,
                    kind = agent_event_kind(ev),
                    detail = %serialized,
                    "translated agent event"
                );

                // T-048: persist to per-event JSONL.
                if let Some(root) = state_root.as_deref() {
                    let _ = append_event_jsonl(root, &cli.id, cli.event, &serialized);
                } else {
                    warn!(
                        agent = %cli.id,
                        event = %cli.event,
                        "no state root resolved; skipping JSONL write (fail-open per R3)"
                    );
                }

                // T-049: forward to zellij pipe targets.
                let payload_str =
                    serde_json::to_string(&serialized).unwrap_or_else(|_| String::from("{}"));
                let _ = pipe_to_zellij(TARGET_ARK_STATUS, &payload_str);
                let _ = pipe_to_zellij(TARGET_ARK_PICKER, &payload_str);
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

    // T-050: on PermissionRequest, emit the allow payload AFTER the
    // event/trace emission above so observability records the ask first.
    maybe_emit_allow(cli, stdout);

    log_budget(cli, started);
    Ok(HookOutcome::Allow)
}

/// Resolve the state root via `ark_types::EnvPaths`. On failure (e.g.
/// `HOME` unset in a weird env) we return `None` and run.rs will skip
/// the JSONL write — fail-open per R3.
fn resolve_state_root() -> Option<PathBuf> {
    match EnvPaths::resolve() {
        Ok(layout) => Some(layout.base().to_path_buf()),
        Err(e) => {
            warn!(error = %e, "could not resolve state root; skipping JSONL writes");
            None
        }
    }
}

/// Emit the allow payload to `stdout` iff the current event is
/// `PermissionRequest`. Wrapped errors are logged and swallowed.
fn maybe_emit_allow<W: Write>(cli: &Cli, stdout: &mut W) {
    if cli.event != HookEvent::PermissionRequest {
        return;
    }
    if let Err(e) = write_allow_payload(&mut *stdout) {
        warn!(
            agent = %cli.id,
            event = %cli.event,
            error = %e,
            "allow payload write failed; fail-open"
        );
    }
}

/// Short static label for an [`AgentEvent`], matching the serde
/// `kind` discriminant.
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

// Silence the unused warning in non-unix test configs where helpers use it.
#[allow(dead_code)]
fn _touch_path(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::io::Cursor;

    use ark_types::AgentId;
    use tempfile::TempDir;

    use crate::allow::ALLOW_PAYLOAD_JSON;
    use crate::cli::Cli;
    use crate::event::HookEvent;

    fn cli_for(event: HookEvent) -> Cli {
        Cli {
            id: AgentId::new("cavekit", "hooktest"),
            event,
        }
    }

    fn run_sandboxed(
        cli: &Cli,
        stdin_bytes: &[u8],
        state_root: Option<PathBuf>,
    ) -> (HookOutcome, Vec<u8>) {
        let mut stdin = Cursor::new(stdin_bytes.to_vec());
        let mut stdout: Vec<u8> = Vec::new();
        let outcome = run_with_state(cli, &mut stdin, &mut stdout, state_root).expect("run ok");
        (outcome, stdout)
    }

    #[test]
    fn empty_stdin_fail_open_returns_allow() {
        let cli = cli_for(HookEvent::PostToolUse);
        let (outcome, stdout) = run_sandboxed(&cli, b"", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
        assert!(
            stdout.is_empty(),
            "non-permission event writes nothing to stdout"
        );
    }

    #[test]
    fn whitespace_only_stdin_fail_open_returns_allow() {
        let cli = cli_for(HookEvent::Stop);
        let (outcome, stdout) = run_sandboxed(&cli, b"   \n\t  \n", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());
    }

    #[test]
    fn malformed_json_fails_open() {
        let cli = cli_for(HookEvent::PostToolUse);
        let (outcome, stdout) = run_sandboxed(&cli, b"{not json", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn valid_json_returns_allow_and_writes_jsonl() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PostToolUse);
        // Pre-create agent dir so writer doesn't fail-open.
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());

        let path = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PostToolUse.jsonl");
        assert!(path.is_file());
        let contents = fs::read_to_string(&path).unwrap();
        // Two lines: ToolUse + FileEdited (Edit is a file-edit tool with file_path).
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("tool_use"));
        assert!(lines[1].contains("file_edited"));
    }

    #[test]
    fn permission_request_emits_allow_payload_on_stdout() {
        let cli = cli_for(HookEvent::PermissionRequest);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (outcome, stdout) = run_sandboxed(&cli, payload.as_bytes(), None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_with_malformed_stdin_still_emits_allow() {
        let cli = cli_for(HookEvent::PermissionRequest);
        let (outcome, stdout) = run_sandboxed(&cli, b"{not json", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_with_empty_stdin_still_emits_allow() {
        let cli = cli_for(HookEvent::PermissionRequest);
        let (outcome, stdout) = run_sandboxed(&cli, b"", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn non_permission_events_do_not_write_to_stdout() {
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::Notification,
            HookEvent::SessionEnd,
            HookEvent::TaskCompleted,
        ] {
            let cli = cli_for(ev);
            let payload = format!(
                r#"{{"session_id":"s1","cwd":"/tmp","hook_event_name":"{}"}}"#,
                ev.as_str()
            );
            let (outcome, stdout) = run_sandboxed(&cli, payload.as_bytes(), None);
            assert_eq!(outcome, HookOutcome::Allow);
            assert!(
                stdout.is_empty(),
                "event {} wrote to stdout: {:?}",
                ev,
                stdout
            );
        }
    }

    #[test]
    fn missing_agent_dir_still_allows_and_does_not_crash() {
        // state_root exists, but agent dir does not — writer must fail-open.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PostToolUse);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());
        // Nothing written because the agent dir was missing.
        assert!(!cli.id.state_dir(tmp.path()).join("hooks").exists());
    }

    #[test]
    fn allow_outcome_exits_zero() {
        assert_eq!(HookOutcome::Allow.exit_code(), 0);
    }
}
