//! Entry pipeline for `ark-hook`.
//!
//! Per cavekit-hook-ipc.md R1+R2+R3, one invocation does four things:
//! 1. Parse the stdin JSON into a [`HookPayload`] (T-047).
//! 2. Derive one or more [`AgentEvent`]s via [`payload_to_events`].
//! 3. Persist each derived event as a JSON line to
//!    `$STATE/agents/{id}/hooks/{EventName}.jsonl` (T-048).
//! 4. Forward each serialized event to the `ark-status` and `ark-picker`
//!    zellij pipe targets (T-049).
//! 5. For `PermissionRequest` events, consult the on-disk permission
//!    policy (T-054 + F-044 fix) and emit the allow payload only when
//!    the policy decides `Allowed` for the requested tool.
//!
//! Every post-parse failure is **fail-open** for Claude (R3): log a
//! warning to stderr and keep going (exit 0, never block the CLI). The
//! only way this binary can exit non-zero today is a clap
//! argument-validation failure at launch.
//!
//! ## F-044 fix — policy-gated permission writes
//!
//! Before this fix `ark-hook` unconditionally wrote
//! `{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}` for every
//! `PermissionRequest`, silently bypassing the `permission_policy` file
//! the engine crate had just been taught to write. Every tool call was
//! auto-approved regardless of the configured policy.
//!
//! The policy file (`$STATE/agents/{id}/permission_policy`, one line,
//! one of `ask` / `auto_approve_read` / `auto_approve_all`) is now read
//! on every `PermissionRequest`. Decision flow:
//!
//! - `ask` → no stdout write. Claude's hook schema treats
//!   "no `hookSpecificOutput.decision`" as defer-to-user and prompts
//!   the user in its TUI.
//! - `auto_approve_read` → write allow only when `tool_name` is in
//!   [`ark_types::permission::READ_ONLY_TOOLS`]; otherwise defer.
//! - `auto_approve_all` → always write allow.
//!
//! **Fail-SAFE contract**: every error path — missing policy file,
//! unreadable file, garbage content, missing agent state dir, malformed
//! stdin (no tool_name extractable) — defaults to `ask` (no allow
//! written). The exit code stays 0 on every path.
//!
//! **Observability invariant**: `PermissionAsked` AND
//! `PermissionResolved` are emitted to JSONL + zellij pipe + tracing on
//! *every* `PermissionRequest` hook, regardless of whether the allow
//! payload is ultimately written. This satisfies T-054 R3 ("always
//! emit the trace pair"), cavekit-engine-claude-code R3, and
//! cavekit-hook-ipc R2 (PermissionRequest.jsonl per-event audit log),
//! and is what the reviewer UI subscribes to. The Asked event is
//! always logged first, then Resolved, so downstream consumers can
//! rely on the file ordering. F-053 added the Resolved half of the
//! pair (previously only Asked was emitted).
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
use ark_types::permission::{PermissionPolicy, decide, read_policy_for_agent};
use ark_types::{AgentId, PermissionDecision};

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
/// a future explicit-deny policy (exit code 2); today's
/// PermissionRequest path is either "write allow payload" or "defer to
/// Claude's TUI prompt" — both exit 0.
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
        // Fail-open for stdin read: log and continue. PermissionRequest
        // force-defaults to Ask (no allow) — we cannot know what tool
        // was requested, so fail-SAFE.
        warn!(
            agent = %cli.id,
            event = %cli.event,
            error = %e,
            "stdin read failed; fail-open"
        );
        // F-060: every PermissionRequest fail-open branch must emit
        // the Asked+Resolved pair together. `maybe_emit_permission_decision`
        // returns the Resolved half; the helper pairs it with a
        // synthetic Asked (tool="unknown") emitted first.
        if let Some(decision) =
            maybe_emit_permission_decision(cli, stdout, state_root.as_deref(), None, true)
        {
            emit_permission_pair_synthetic(
                &cli.id,
                state_root.as_deref(),
                "unknown",
                "",
                decision,
                "stdin-read-error",
            );
        }
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

    if buf.trim().is_empty() {
        warn!(
            agent = %cli.id,
            event = %cli.event,
            "stdin empty; fail-open (R3 — never block claude)"
        );
        // F-060: empty stdin is the second fail-open branch. Same pair
        // invariant applies — synthesize both halves in order.
        if let Some(decision) =
            maybe_emit_permission_decision(cli, stdout, state_root.as_deref(), None, true)
        {
            emit_permission_pair_synthetic(
                &cli.id,
                state_root.as_deref(),
                "unknown",
                "",
                decision,
                "empty-stdin",
            );
        }
        log_budget(cli, started);
        return Ok(HookOutcome::Allow);
    }

    // Track whether stdin parsed successfully. Malformed JSON forces
    // `Ask` regardless of configured policy — we cannot validate the
    // tool so fail-SAFE.
    let mut parse_failed = false;
    let parsed_tool_name: Option<String> = match serde_json::from_str::<HookPayload>(&buf) {
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
            payload.tool_name
        }
        Err(e) => {
            warn!(
                agent = %cli.id,
                event = %cli.event,
                error = %e,
                bytes = buf.len(),
                "stdin not valid HookPayload; fail-open"
            );
            parse_failed = true;
            // Malformed stdin on PermissionRequest: no tool_name → Ask.
            // We still want to record that a permission hook fired so
            // observers see the activity. `emit_permission_asked_trace`
            // handles the JSONL + pipe + tracing fan-out with
            // tool="unknown" / summary="" when the payload was unparseable.
            if cli.event == HookEvent::PermissionRequest {
                emit_permission_asked_trace(&cli.id, state_root.as_deref(), "unknown", "");
            }
            None
        }
    };

    // F-044: consult policy + tool to decide whether to write the allow
    // payload. Non-permission events are a no-op. Missing policy /
    // missing tool / unreadable policy all fail SAFE to Ask (no write).
    // Malformed stdin also forces Ask so we never auto-approve a
    // request we couldn't validate.
    let decision = maybe_emit_permission_decision(
        cli,
        stdout,
        state_root.as_deref(),
        parsed_tool_name.as_deref(),
        parse_failed,
    );

    // F-053: always emit a PermissionResolved trace for every
    // PermissionRequest invocation so the audit log (JSONL + zellij
    // pipe) carries the Asked→Resolved pair regardless of policy.
    // Per cavekit-engine-claude-code R3 + cavekit-hook-ipc R2.
    // The `tool` string MUST match the Asked-side string: that side
    // used `payload.tool_name.unwrap_or("unknown")` (valid parse) or
    // the synthesized "unknown" (malformed JSON via
    // `emit_permission_asked_trace`). `parsed_tool_name` is None in
    // both failure paths, so the same fallback applies.
    if let Some(decision) = decision {
        let tool = parsed_tool_name.as_deref().unwrap_or("unknown");
        emit_permission_resolved_trace(&cli.id, state_root.as_deref(), tool, decision);
    }

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

/// Emit a `PermissionAsked` trace for malformed-stdin PermissionRequest
/// payloads so observers still record the event even when we couldn't
/// extract a tool name. Mirrors the fan-out done by `payload_to_events`
/// + JSONL + zellij pipe path above, but with a synthesized event
/// carrying `tool="unknown"`.
fn emit_permission_asked_trace(id: &AgentId, state_root: Option<&Path>, tool: &str, summary: &str) {
    let ev = ark_types::AgentEvent::PermissionAsked {
        id: id.clone(),
        tool: tool.to_string(),
        summary: summary.to_string(),
    };
    let serialized = serde_json::to_value(&ev).unwrap_or_else(
        |_| serde_json::json!({ "kind": "permission_asked", "serialize_failed": true }),
    );
    info!(
        agent = %id,
        event = "PermissionRequest",
        kind = "permission_asked",
        detail = %serialized,
        "synthesized permission_asked (malformed stdin)"
    );
    if let Some(root) = state_root {
        let _ = append_event_jsonl(root, id, HookEvent::PermissionRequest, &serialized);
    }
    let payload_str = serde_json::to_string(&serialized).unwrap_or_else(|_| String::from("{}"));
    let _ = pipe_to_zellij(TARGET_ARK_STATUS, &payload_str);
    let _ = pipe_to_zellij(TARGET_ARK_PICKER, &payload_str);
}

/// F-060 fix: synthesize BOTH `PermissionAsked` and `PermissionResolved`
/// events for a fail-open branch that never went through
/// `payload_to_events` (so no Asked was emitted by the main-loop path).
///
/// Invariant: `PermissionAsked` MUST precede `PermissionResolved` in
/// the JSONL file, and both MUST carry the same `tool` string so
/// downstream consumers can correlate the pair.
///
/// The `reason` field is for logging only — it records which fail-open
/// branch synthesized the pair (stdin-read-error / empty-stdin /
/// malformed-JSON) so operators investigating a hook trace can tell
/// why the tool was `unknown`.
///
/// Per cavekit-engine-claude-code R3 + cavekit-hook-ipc R2.
fn emit_permission_pair_synthetic(
    id: &AgentId,
    state_root: Option<&Path>,
    tool: &str,
    summary: &str,
    decision: PermissionDecision,
    reason: &str,
) {
    info!(
        agent = %id,
        event = "PermissionRequest",
        reason = reason,
        tool = tool,
        "synthesizing Asked+Resolved pair for fail-open branch"
    );
    emit_permission_asked_trace(id, state_root, tool, summary);
    emit_permission_resolved_trace(id, state_root, tool, decision);
}

/// F-053 fix: emit the matching `PermissionResolved` trace after
/// [`maybe_emit_permission_decision`] has decided. Mirrors the
/// Asked-side fan-out (JSONL + zellij pipe + tracing) so the audit log
/// always carries the Asked→Resolved pair for every PermissionRequest
/// hook, regardless of whether the allow payload was actually written.
///
/// Per cavekit-engine-claude-code R3 + cavekit-hook-ipc R2: always emit
/// PermissionAsked + PermissionResolved for every permission hook,
/// regardless of policy. The `tool` string MUST match the one used in
/// the Asked event so downstream consumers can correlate the pair.
fn emit_permission_resolved_trace(
    id: &AgentId,
    state_root: Option<&Path>,
    tool: &str,
    decision: PermissionDecision,
) {
    let ev = ark_types::AgentEvent::PermissionResolved {
        id: id.clone(),
        tool: tool.to_string(),
        decision,
    };
    let serialized = serde_json::to_value(&ev).unwrap_or_else(
        |_| serde_json::json!({ "kind": "permission_resolved", "serialize_failed": true }),
    );
    info!(
        agent = %id,
        event = "PermissionRequest",
        kind = "permission_resolved",
        detail = %serialized,
        "emitted permission_resolved trace"
    );
    if let Some(root) = state_root {
        let _ = append_event_jsonl(root, id, HookEvent::PermissionRequest, &serialized);
    }
    let payload_str = serde_json::to_string(&serialized).unwrap_or_else(|_| String::from("{}"));
    let _ = pipe_to_zellij(TARGET_ARK_STATUS, &payload_str);
    let _ = pipe_to_zellij(TARGET_ARK_PICKER, &payload_str);
}

/// Policy-aware replacement for the old `ensure_permission_allow` /
/// `emit_allow_swallow` pair (F-044).
///
/// For `PermissionRequest` events, reads the policy file, applies
/// [`decide`] against the supplied `tool_name`, and writes the allow
/// payload only when the decision is `Allowed`. Every error path
/// (missing/garbage/unreadable policy file, missing agent dir, missing
/// state root) defaults to [`PermissionPolicy::Ask`] → no stdout write.
///
/// `force_ask` is set by the caller to `true` whenever the stdin was
/// unparseable (read error, empty, malformed JSON) — in those cases
/// we cannot verify which tool was requested so the policy is
/// overridden to Ask regardless of what the on-disk file says. This is
/// the documented "malformed stdin defaults to Ask" fail-SAFE
/// contract (F-044).
///
/// Non-permission events are a no-op and return `None`.
///
/// For PermissionRequest events, returns `Some(decision)` so the caller
/// can emit a matching `PermissionResolved` trace (F-053) — the
/// Asked+Resolved pair is the single source of truth for the permission
/// audit log per cavekit-engine-claude-code R3 + cavekit-hook-ipc R2.
///
/// This is the single enforcement point for the fail-SAFE policy
/// contract (F-044): every branch in [`run_with_state`] routes
/// through this helper so a future fail-open branch cannot accidentally
/// bypass the policy check.
pub(crate) fn maybe_emit_permission_decision<W: Write>(
    cli: &Cli,
    stdout: &mut W,
    state_root: Option<&Path>,
    tool_name: Option<&str>,
    force_ask: bool,
) -> Option<PermissionDecision> {
    if cli.event != HookEvent::PermissionRequest {
        return None;
    }

    // Resolve policy (fail-SAFE: any error or forced override → Ask).
    let policy = if force_ask {
        warn!(
            agent = %cli.id,
            "stdin unparseable; overriding permission policy to ask (fail-safe)"
        );
        PermissionPolicy::Ask
    } else {
        match state_root {
            Some(root) => read_policy_for_agent(root, &cli.id),
            None => {
                warn!(
                    agent = %cli.id,
                    "no state root resolved; defaulting permission policy to ask (fail-safe)"
                );
                PermissionPolicy::Ask
            }
        }
    };

    // No tool_name still goes through `decide`; `AutoApproveAll`
    // legitimately allows-all regardless of tool, so we only need to
    // guard against the `AutoApproveRead` allowlist matching an empty
    // string. Use `""` so no `READ_ONLY_TOOLS` entry matches.
    let tool = tool_name.unwrap_or("");

    let decision = decide(policy, tool);

    info!(
        agent = %cli.id,
        event = %cli.event,
        policy = %policy,
        tool = tool,
        decision = ?decision,
        "permission decision resolved"
    );

    match decision {
        PermissionDecision::Allowed => {
            if let Err(e) = write_allow_payload(&mut *stdout) {
                warn!(
                    agent = %cli.id,
                    event = %cli.event,
                    error = %e,
                    "allow payload write failed; fail-open"
                );
            }
        }
        PermissionDecision::Deferred | PermissionDecision::Denied => {
            // Silence on stdout = "defer to Claude's TUI prompt" per
            // Claude Code's hook schema.
            info!(
                agent = %cli.id,
                event = %cli.event,
                policy = %policy,
                tool = tool,
                "deferring to claude's in-TUI prompt (no hookSpecificOutput)"
            );
        }
    }

    Some(decision)
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
    use std::io;
    use std::io::Cursor;

    use ark_types::AgentId;
    use ark_types::permission::{PermissionPolicy, write_policy_file};
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

    /// Seed a policy file at the agent's state dir under `root`.
    /// Creates the agent state dir if it does not exist.
    fn seed_policy(root: &Path, cli: &Cli, policy: PermissionPolicy) {
        let dir = cli.id.state_dir(root);
        fs::create_dir_all(&dir).expect("mkdir");
        write_policy_file(&dir, policy).expect("write policy");
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

    // -----------------------------------------------------------------
    // Non-permission events never write to stdout (unchanged by F-044).
    // -----------------------------------------------------------------

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
    fn non_permission_events_ignore_policy() {
        // Even with auto_approve_all seeded, non-permission events must
        // never write an allow payload.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PostToolUse);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());
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

    // -----------------------------------------------------------------
    // F-044 fix: policy-gated PermissionRequest stdout emission.
    // These replace the pre-F-044 "always allow" tests, which asserted
    // that an allow payload was written on every PermissionRequest
    // branch — that behavior was the security regression.
    // -----------------------------------------------------------------

    #[test]
    fn permission_request_ask_policy_does_not_write_allow() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
        assert!(
            stdout.is_empty(),
            "ask policy must NOT write allow payload: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_auto_approve_all_writes_allow() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_all_allows_edit_too() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_read_allows_read() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_read_allows_grep() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Grep","tool_input":{"pattern":"foo"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_read_defers_edit() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(
            stdout.is_empty(),
            "auto_approve_read must defer Edit: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_auto_approve_read_defers_bash() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());
    }

    #[test]
    fn permission_request_missing_policy_file_defaults_to_ask() {
        // Agent dir exists but no policy file => read_policy returns Ask.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(
            stdout.is_empty(),
            "missing policy must fail-SAFE to ask even for Read: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_garbage_policy_defaults_to_ask() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        let dir = cli.id.state_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("permission_policy"), "gibberish!!!").unwrap();

        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(
            stdout.is_empty(),
            "garbage policy must fail-SAFE to ask: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_missing_agent_dir_defaults_to_ask() {
        // state_root exists (tempdir) but agent dir not created.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(
            stdout.is_empty(),
            "missing agent dir must fail-SAFE to ask: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_no_state_root_defaults_to_ask() {
        // No state root at all (e.g. HOME unset) → Ask.
        let cli = cli_for(HookEvent::PermissionRequest);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), None);
        assert!(
            stdout.is_empty(),
            "no state root must fail-SAFE to ask: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_malformed_stdin_defaults_to_ask() {
        // Malformed stdin: tool_name is unextractable → Ask, even with
        // an otherwise-permissive policy configured. F-044: this is the
        // "no tool_name available" branch that must not silently
        // auto-approve.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);

        let (outcome, stdout) = run_sandboxed(&cli, b"{not json", Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(
            stdout.is_empty(),
            "malformed stdin must fail-SAFE to ask: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_malformed_stdin_still_emits_asked_jsonl() {
        // T-054 R3: PermissionAsked observability fires regardless of
        // whether we could parse the payload.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let (_, stdout) = run_sandboxed(&cli, b"{not json", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty(), "must NOT write allow on malformed stdin");

        let jsonl = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PermissionRequest.jsonl");
        assert!(
            jsonl.is_file(),
            "PermissionAsked JSONL must still be written on malformed stdin"
        );
        let contents = fs::read_to_string(&jsonl).unwrap();
        assert!(contents.contains("permission_asked"));
        assert!(contents.contains("unknown"));
    }

    #[test]
    fn permission_request_empty_stdin_defaults_to_ask() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let (_, stdout) = run_sandboxed(&cli, b"", Some(tmp.path().to_path_buf()));
        // Empty stdin has no parsed payload → tool_name = None → Ask.
        assert!(
            stdout.is_empty(),
            "empty stdin must fail-SAFE to ask even under auto_approve_all: {stdout:?}"
        );
    }

    #[test]
    fn permission_request_stdin_read_error_defaults_to_ask() {
        /// Reader that always returns an I/O error on read.
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "simulated"))
            }
        }
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut stdin = ErroringReader;
        let mut stdout: Vec<u8> = Vec::new();
        let outcome = run_with_state(
            &cli,
            &mut stdin,
            &mut stdout,
            Some(tmp.path().to_path_buf()),
        )
        .expect("run ok");
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(
            stdout.is_empty(),
            "stdin read error must fail-SAFE to ask (no tool_name): {stdout:?}"
        );
    }

    #[test]
    fn permission_request_writes_jsonl_on_valid_payload() {
        // Observability invariant: the PermissionAsked event lands in
        // the JSONL file regardless of the policy decision. Seed `ask`
        // so no stdout is written but JSONL still fires.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let jsonl = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PermissionRequest.jsonl");
        assert!(jsonl.is_file());
        let contents = fs::read_to_string(&jsonl).unwrap();
        assert!(contents.contains("permission_asked"));
        assert!(contents.contains("Bash"));
    }

    #[test]
    fn run_never_returns_err_on_any_stdin_variant() {
        // Top-level contract: run_with_state never returns Err for any
        // stdin fail-open class, across both permission and
        // non-permission events.
        /// Reader that always returns an I/O error on read.
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "simulated"))
            }
        }
        let cli_perm = cli_for(HookEvent::PermissionRequest);
        let cli_post = cli_for(HookEvent::PostToolUse);

        for cli in [&cli_perm, &cli_post] {
            let mut r = ErroringReader;
            let mut w: Vec<u8> = Vec::new();
            assert!(run_with_state(cli, &mut r, &mut w, None).is_ok());

            let mut r = Cursor::new(Vec::<u8>::new());
            let mut w: Vec<u8> = Vec::new();
            assert!(run_with_state(cli, &mut r, &mut w, None).is_ok());

            let mut r = Cursor::new(b"{garbage".to_vec());
            let mut w: Vec<u8> = Vec::new();
            assert!(run_with_state(cli, &mut r, &mut w, None).is_ok());
        }
    }

    // -----------------------------------------------------------------
    // Unit tests for the policy-aware decision helper itself.
    // -----------------------------------------------------------------

    #[test]
    fn maybe_emit_noop_for_non_permission_events() {
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::Notification,
            HookEvent::SessionEnd,
            HookEvent::TaskCompleted,
        ] {
            let cli = cli_for(ev);
            let mut buf: Vec<u8> = Vec::new();
            maybe_emit_permission_decision(&cli, &mut buf, None, Some("Read"), false);
            assert!(
                buf.is_empty(),
                "non-permission event {ev} wrote bytes: {buf:?}"
            );
        }
    }

    #[test]
    fn maybe_emit_writes_allow_for_allowed_decision() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), Some("Edit"), false);
        assert_eq!(std::str::from_utf8(&buf).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn maybe_emit_silent_for_deferred_decision() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), Some("Read"), false);
        assert!(buf.is_empty());
    }

    #[test]
    fn maybe_emit_silent_when_tool_name_missing_under_auto_approve_read() {
        // Under auto_approve_read, tool_name = None must not match any
        // read-only tool and therefore defers.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), None, false);
        assert!(buf.is_empty());
    }

    #[test]
    fn maybe_emit_allows_under_auto_approve_all_even_without_tool() {
        // When stdin parsed cleanly but tool_name was omitted,
        // auto_approve_all is tool-agnostic and still allows.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), None, false);
        assert_eq!(std::str::from_utf8(&buf).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn maybe_emit_force_ask_overrides_auto_approve_all() {
        // F-044: when stdin was unparseable (read error / malformed /
        // empty) the caller sets force_ask=true, and we must treat the
        // policy as Ask no matter what the file says.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), None, true);
        assert!(
            buf.is_empty(),
            "force_ask must override auto_approve_all: {buf:?}"
        );
    }

    // -----------------------------------------------------------------
    // F-053: every PermissionRequest must emit BOTH PermissionAsked and
    // PermissionResolved events to JSONL + zellij pipe, regardless of
    // policy. Per cavekit-engine-claude-code R3 + cavekit-hook-ipc R2.
    // -----------------------------------------------------------------

    /// Read the PermissionRequest JSONL file for `cli` under `root`.
    /// Returns the parsed lines (as JSON Values) in file order.
    fn read_permission_jsonl(root: &Path, cli: &Cli) -> Vec<serde_json::Value> {
        let path = cli
            .id
            .state_dir(root)
            .join("hooks")
            .join("PermissionRequest.jsonl");
        if !path.is_file() {
            return Vec::new();
        }
        let contents = fs::read_to_string(&path).expect("read jsonl");
        contents
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse jsonl line"))
            .collect()
    }

    /// Extract the `kind` serde discriminant from a serialized AgentEvent.
    fn kind_of(v: &serde_json::Value) -> &str {
        v.get("kind").and_then(|k| k.as_str()).unwrap_or("")
    }

    #[test]
    fn f053_return_type_is_some_for_permission_events() {
        // The helper must return Some(decision) for PermissionRequest so
        // the caller can fan out the matching Resolved trace.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        let out =
            maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), Some("Edit"), false);
        assert_eq!(out, Some(PermissionDecision::Allowed));
    }

    #[test]
    fn f053_return_type_is_none_for_non_permission_events() {
        // Non-permission events must return None so the caller skips
        // the Resolved emission.
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::Notification,
            HookEvent::SessionEnd,
            HookEvent::TaskCompleted,
        ] {
            let cli = cli_for(ev);
            let mut buf: Vec<u8> = Vec::new();
            let out = maybe_emit_permission_decision(&cli, &mut buf, None, Some("Read"), false);
            assert_eq!(out, None, "event {ev} should return None");
        }
    }

    #[test]
    fn f053_ask_policy_emits_asked_then_resolved_deferred() {
        // ask + valid payload → JSONL has Asked then Resolved(Deferred).
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty(), "ask policy must not write allow payload");

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "expected Asked + Resolved, got {} lines: {:#?}",
            lines.len(),
            lines
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(lines[0].get("tool").and_then(|t| t.as_str()), Some("Bash"));
        assert_eq!(lines[1].get("tool").and_then(|t| t.as_str()), Some("Bash"));
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f053_auto_approve_read_read_tool_emits_resolved_allowed() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(lines.len(), 2);
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(lines[0].get("tool").and_then(|t| t.as_str()), Some("Read"));
        assert_eq!(lines[1].get("tool").and_then(|t| t.as_str()), Some("Read"));
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("allowed")
        );
    }

    #[test]
    fn f053_auto_approve_read_edit_tool_emits_resolved_deferred() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty(), "auto_approve_read must defer Edit");

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(lines.len(), 2);
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(lines[0].get("tool").and_then(|t| t.as_str()), Some("Edit"));
        assert_eq!(lines[1].get("tool").and_then(|t| t.as_str()), Some("Edit"));
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f053_auto_approve_all_edit_tool_emits_resolved_allowed() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(lines.len(), 2);
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(lines[0].get("tool").and_then(|t| t.as_str()), Some("Edit"));
        assert_eq!(lines[1].get("tool").and_then(|t| t.as_str()), Some("Edit"));
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("allowed")
        );
    }

    #[test]
    fn f053_missing_policy_file_emits_resolved_deferred() {
        // Missing policy file → defaults to ask → Resolved(Deferred),
        // no stdout.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(lines.len(), 2);
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f053_malformed_stdin_emits_resolved_deferred_with_unknown_tool() {
        // Malformed JSON + PermissionRequest → Asked(unknown) already
        // existed; F-053 adds Resolved(unknown, Deferred) alongside.
        // force_ask path: no stdout, but JSONL must have both events.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);

        let (_, stdout) = run_sandboxed(&cli, b"{not json", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty(), "force_ask must override");

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "expected Asked + Resolved for malformed stdin"
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        // Both events must carry tool="unknown" (Asked + Resolved
        // string must match so downstream can correlate the pair).
        assert_eq!(
            lines[0].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            lines[1].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f053_non_permission_events_do_not_emit_resolved() {
        // Non-PermissionRequest events must not emit Resolved
        // (maybe_emit returns None, run_with_state skips the trace).
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PostToolUse);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, _) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));

        // PermissionRequest JSONL must not be created.
        let jsonl = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PermissionRequest.jsonl");
        assert!(
            !jsonl.exists(),
            "non-permission event must not touch PermissionRequest.jsonl"
        );
    }

    #[test]
    fn f053_ordering_invariant_asked_before_resolved() {
        // Dedicated line-ordering check: whatever the policy, Asked
        // MUST appear in the JSONL strictly before Resolved.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));

        let path = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PermissionRequest.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        let asked_pos = contents
            .find("permission_asked")
            .expect("Asked line present");
        let resolved_pos = contents
            .find("permission_resolved")
            .expect("Resolved line present");
        assert!(
            asked_pos < resolved_pos,
            "Asked ({asked_pos}) must precede Resolved ({resolved_pos}) in JSONL"
        );
    }

    #[test]
    fn f053_empty_stdin_permission_event_emits_resolved_deferred() {
        // Empty stdin + PermissionRequest → force_ask path. Asked is
        // not emitted for this branch (pre-existing behavior) but
        // Resolved MUST still fire so the policy decision is auditable.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();
        let (_, stdout) = run_sandboxed(&cli, b"", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        // At least one permission_resolved line must be present.
        let resolved = lines
            .iter()
            .find(|v| kind_of(v) == "permission_resolved")
            .expect("Resolved must be emitted on empty stdin PermissionRequest");
        assert_eq!(
            resolved.get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            resolved.get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f053_stdin_read_error_permission_event_emits_resolved_deferred() {
        // stdin read error + PermissionRequest → force_ask path.
        // Resolved MUST still fire per F-053.
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "simulated"))
            }
        }
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let mut stdin = ErroringReader;
        let mut stdout: Vec<u8> = Vec::new();
        run_with_state(
            &cli,
            &mut stdin,
            &mut stdout,
            Some(tmp.path().to_path_buf()),
        )
        .expect("run ok");
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        let resolved = lines
            .iter()
            .find(|v| kind_of(v) == "permission_resolved")
            .expect("Resolved must be emitted on stdin read error PermissionRequest");
        assert_eq!(
            resolved.get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    // -----------------------------------------------------------------
    // F-060: Asked+Resolved pair invariant in fail-open branches.
    //
    // The F-053 fix added the Resolved half of the pair to every
    // fail-open branch, but forgot to also synthesize the Asked half
    // in the stdin-read-error and empty-stdin paths — so those two
    // branches ended up emitting Resolved alone, breaking the pair
    // invariant that F-053 itself introduced.
    //
    // After F-060: every PermissionRequest fail-open branch emits the
    // synthetic Asked + Resolved pair in JSONL order, both with
    // tool="unknown". The malformed-JSON branch already had Asked from
    // before (locked with a regression test here), and the valid-JSON
    // path already pairs Asked via payload_to_events + Resolved via
    // the main-loop post-pass.
    // -----------------------------------------------------------------

    #[test]
    fn f060_stdin_read_error_emits_asked_then_resolved_pair() {
        // F-060 regression: both halves of the pair must be present,
        // in order (Asked before Resolved), both with tool="unknown".
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "simulated"))
            }
        }
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let mut stdin = ErroringReader;
        let mut stdout: Vec<u8> = Vec::new();
        run_with_state(
            &cli,
            &mut stdin,
            &mut stdout,
            Some(tmp.path().to_path_buf()),
        )
        .expect("run ok");
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "F-060: stdin-read-error branch must emit Asked + Resolved pair, got {lines:#?}"
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(
            lines[0].get("tool").and_then(|t| t.as_str()),
            Some("unknown"),
            "Asked must carry tool=\"unknown\" when stdin couldn't be read"
        );
        assert_eq!(
            lines[1].get("tool").and_then(|t| t.as_str()),
            Some("unknown"),
            "Resolved must match Asked's tool so consumers can correlate the pair"
        );
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f060_empty_stdin_emits_asked_then_resolved_pair() {
        // F-060: empty stdin is the second fail-open branch. Same pair
        // invariant, same tool="unknown" contract.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let (_, stdout) = run_sandboxed(&cli, b"", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "F-060: empty-stdin branch must emit Asked + Resolved pair, got {lines:#?}"
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(
            lines[0].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            lines[1].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            lines[1].get("decision").and_then(|d| d.as_str()),
            Some("deferred")
        );
    }

    #[test]
    fn f060_whitespace_only_stdin_emits_asked_then_resolved_pair() {
        // Whitespace-only stdin trips the same `buf.trim().is_empty()`
        // branch as truly-empty stdin. The pair invariant must hold.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let (_, stdout) = run_sandboxed(&cli, b"   \n\t  \n", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "F-060: whitespace-only stdin must also emit Asked + Resolved pair"
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
    }

    #[test]
    fn f060_malformed_json_still_emits_asked_then_resolved_pair() {
        // Regression lock: the malformed-JSON branch already had Asked
        // from before F-060 (emitted inline via
        // `emit_permission_asked_trace`). This test pins that behavior
        // so no future refactor can accidentally break it.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let (_, stdout) = run_sandboxed(&cli, b"{not json", Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(
            lines.len(),
            2,
            "F-060 regression: malformed JSON must emit Asked + Resolved pair, got {lines:#?}"
        );
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        assert_eq!(
            lines[0].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
        assert_eq!(
            lines[1].get("tool").and_then(|t| t.as_str()),
            Some("unknown")
        );
    }

    #[test]
    fn f060_valid_payload_still_emits_asked_then_resolved_pair() {
        // The non-synthetic path — valid payload, Asked comes from
        // payload_to_events (carries the real tool name), Resolved
        // comes from the main-loop post-pass. Both must match tools.
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, _) = run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));

        let lines = read_permission_jsonl(tmp.path(), &cli);
        assert_eq!(lines.len(), 2);
        assert_eq!(kind_of(&lines[0]), "permission_asked");
        assert_eq!(kind_of(&lines[1]), "permission_resolved");
        // Both MUST carry the real tool name, not "unknown".
        assert_eq!(lines[0].get("tool").and_then(|t| t.as_str()), Some("Bash"));
        assert_eq!(lines[1].get("tool").and_then(|t| t.as_str()), Some("Bash"));
    }

    #[test]
    fn f060_ordering_in_stdin_read_error_branch() {
        // Asked file-offset MUST be strictly less than Resolved's —
        // byte-level ordering, independent of line parsing.
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::Other, "boom"))
            }
        }
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        fs::create_dir_all(cli.id.state_dir(tmp.path())).unwrap();

        let mut stdin = ErroringReader;
        let mut stdout: Vec<u8> = Vec::new();
        run_with_state(
            &cli,
            &mut stdin,
            &mut stdout,
            Some(tmp.path().to_path_buf()),
        )
        .expect("run ok");

        let path = cli
            .id
            .state_dir(tmp.path())
            .join("hooks")
            .join("PermissionRequest.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        let asked = contents.find("permission_asked").expect("Asked present");
        let resolved = contents
            .find("permission_resolved")
            .expect("Resolved present");
        assert!(
            asked < resolved,
            "F-060: Asked ({asked}) must precede Resolved ({resolved}) in stdin-read-error branch"
        );
    }
}
