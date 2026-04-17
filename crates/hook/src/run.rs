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

use ark_types::permission::{PermissionPolicy, PolicyDecision, decide, read_policy_file};
use ark_types::{CoreEvent, EnvPaths, ExtEvent, SessionId};

use crate::allow::write_allow_payload;
// `LegacyCli` is the strict (post-validation) shape of the hook-event
// invocation — see `crate::cli::Cli::into_legacy`. Aliased to `Cli` here
// so the legacy-flow body (and its tests) stays unchanged after the
// outer parser grew bridge subcommands in T-6.2.
use crate::cli::LegacyCli as Cli;
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
            session = %cli.id.as_str(),
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
            session = %cli.id.as_str(),
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
                session = %cli.id.as_str(),
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
                    session = %cli.id.as_str(),
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
                        session = %cli.id.as_str(),
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
                session = %cli.id.as_str(),
                event = %cli.event,
                emitted = events.len(),
                "hook translation complete"
            );
            payload.tool_name
        }
        Err(e) => {
            warn!(
                session = %cli.id.as_str(),
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

/// Emit a `permission.asked` trace for malformed-stdin PermissionRequest
/// payloads so observers still record the event even when we couldn't
/// extract a tool name. Mirrors the fan-out done by `payload_to_events`
/// + JSONL + zellij pipe path above, but with a synthesized event
/// carrying `tool="unknown"`.
fn emit_permission_asked_trace(
    id: &SessionId,
    state_root: Option<&Path>,
    tool: &str,
    summary: &str,
) {
    let ev = CoreEvent::Ext(ExtEvent {
        ext: crate::payload::EXT_NAME.to_string(),
        kind: "permission.asked".to_string(),
        payload: serde_json::json!({
            "id": id.as_str(),
            "tool": tool,
            "summary": summary,
        }),
    });
    let serialized = serde_json::to_value(&ev).unwrap_or_else(
        |_| serde_json::json!({ "kind": "permission.asked", "serialize_failed": true }),
    );
    info!(
        session = %id.as_str(),
        event = "PermissionRequest",
        kind = "permission.asked",
        detail = %serialized,
        "synthesized permission.asked (malformed stdin)"
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
    id: &SessionId,
    state_root: Option<&Path>,
    tool: &str,
    summary: &str,
    decision: PolicyDecision,
    reason: &str,
) {
    info!(
        session = %id.as_str(),
        event = "PermissionRequest",
        reason = reason,
        tool = tool,
        "synthesizing asked+resolved pair for fail-open branch"
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
    id: &SessionId,
    state_root: Option<&Path>,
    tool: &str,
    decision: PolicyDecision,
) {
    let decision_str = match decision {
        PolicyDecision::Allowed => "allowed",
        PolicyDecision::Deferred => "deferred",
    };
    let ev = CoreEvent::Ext(ExtEvent {
        ext: crate::payload::EXT_NAME.to_string(),
        kind: "permission.resolved".to_string(),
        payload: serde_json::json!({
            "id": id.as_str(),
            "tool": tool,
            "decision": decision_str,
        }),
    });
    let serialized = serde_json::to_value(&ev).unwrap_or_else(
        |_| serde_json::json!({ "kind": "permission.resolved", "serialize_failed": true }),
    );
    info!(
        session = %id.as_str(),
        event = "PermissionRequest",
        kind = "permission.resolved",
        detail = %serialized,
        "emitted permission.resolved trace"
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
) -> Option<PolicyDecision> {
    if cli.event != HookEvent::PermissionRequest {
        return None;
    }

    // Resolve policy (fail-SAFE: any error or forced override → Ask).
    let policy = if force_ask {
        warn!(
            session = %cli.id.as_str(),
            "stdin unparseable; overriding permission policy to ask (fail-safe)"
        );
        PermissionPolicy::Ask
    } else {
        match state_root {
            Some(root) => {
                let session_dir = crate::writer::session_state_dir(root, &cli.id);
                read_policy_file(&session_dir)
            }
            None => {
                warn!(
                    session = %cli.id.as_str(),
                    "no state root resolved; defaulting permission policy to ask (fail-safe)"
                );
                PermissionPolicy::Ask
            }
        }
    };

    let tool = tool_name.unwrap_or("");

    let decision = decide(policy, tool);

    info!(
        session = %cli.id.as_str(),
        event = %cli.event,
        policy = %policy,
        tool = tool,
        decision = ?decision,
        "permission decision resolved"
    );

    match decision {
        PolicyDecision::Allowed => {
            if let Err(e) = write_allow_payload(&mut *stdout) {
                warn!(
                    session = %cli.id.as_str(),
                    event = %cli.event,
                    error = %e,
                    "allow payload write failed; fail-open"
                );
            }
        }
        PolicyDecision::Deferred => {
            info!(
                session = %cli.id.as_str(),
                event = %cli.event,
                policy = %policy,
                tool = tool,
                "deferring to claude's in-TUI prompt (no hookSpecificOutput)"
            );
        }
    }

    Some(decision)
}

/// Short static label for a translated core event (used in tracing
/// output only — JSONL serde uses the full `CoreEvent` shape).
fn agent_event_kind(ev: &CoreEvent) -> &str {
    crate::payload::event_kind(ev)
}

fn log_budget(cli: &Cli, started: Instant) {
    let elapsed = started.elapsed();
    let ms = elapsed.as_millis();
    if ms > HOOK_BUDGET_MS {
        warn!(
            session = %cli.id.as_str(),
            event = %cli.event,
            elapsed_ms = ms,
            budget_ms = HOOK_BUDGET_MS,
            "hook exceeded budget"
        );
    } else {
        info!(
            session = %cli.id.as_str(),
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

    use ark_types::permission::{PermissionPolicy, write_policy_file};
    use ark_types::SessionId;
    use tempfile::TempDir;

    use crate::allow::ALLOW_PAYLOAD_JSON;
    use crate::cli::LegacyCli as Cli;
    use crate::event::HookEvent;
    use crate::writer::session_state_dir;

    fn cli_for(event: HookEvent) -> Cli {
        Cli {
            id: SessionId::new("hooktest"),
            event,
        }
    }

    fn session_dir(root: &Path, cli: &Cli) -> PathBuf {
        session_state_dir(root, &cli.id)
    }

    /// Seed a policy file at the session's state dir under `root`.
    fn seed_policy(root: &Path, cli: &Cli, policy: PermissionPolicy) {
        let dir = session_dir(root, cli);
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

    // ---------- non-permission events never write to stdout ----------

    #[test]
    fn empty_stdin_fail_open_returns_allow() {
        let cli = cli_for(HookEvent::PostToolUse);
        let (outcome, stdout) = run_sandboxed(&cli, b"", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert_eq!(outcome.exit_code(), 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn malformed_json_fails_open() {
        let cli = cli_for(HookEvent::PostToolUse);
        let (outcome, stdout) = run_sandboxed(&cli, b"{not json", None);
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());
    }

    #[test]
    fn valid_json_returns_allow_and_writes_jsonl() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PostToolUse);
        fs::create_dir_all(session_dir(tmp.path(), &cli)).unwrap();

        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (outcome, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());

        let path = session_dir(tmp.path(), &cli)
            .join("hooks")
            .join("PostToolUse.jsonl");
        assert!(path.is_file());
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("tool.use"));
        assert!(lines[1].contains("file.edited"));
    }

    #[test]
    fn allow_outcome_exits_zero() {
        assert_eq!(HookOutcome::Allow.exit_code(), 0);
    }

    // ---------- policy-gated PermissionRequest stdout ----------

    #[test]
    fn permission_request_ask_policy_does_not_write_allow() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::Ask);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());
    }

    #[test]
    fn permission_request_auto_approve_all_writes_allow() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let (_, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_read_allows_read() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Read","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert_eq!(std::str::from_utf8(&stdout).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn permission_request_auto_approve_read_defers_edit() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveRead);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/x"}}"#;
        let (_, stdout) =
            run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));
        assert!(stdout.is_empty());
    }

    #[test]
    fn permission_request_malformed_stdin_defaults_to_ask() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let (outcome, stdout) =
            run_sandboxed(&cli, b"{not json", Some(tmp.path().to_path_buf()));
        assert_eq!(outcome, HookOutcome::Allow);
        assert!(stdout.is_empty());
    }

    #[test]
    fn maybe_emit_noop_for_non_permission_events() {
        for ev in [
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::Notification,
        ] {
            let cli = cli_for(ev);
            let mut buf: Vec<u8> = Vec::new();
            let out = maybe_emit_permission_decision(&cli, &mut buf, None, Some("Read"), false);
            assert!(buf.is_empty());
            assert!(out.is_none());
        }
    }

    #[test]
    fn maybe_emit_writes_allow_for_allowed_decision() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        let out =
            maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), Some("Edit"), false);
        assert_eq!(std::str::from_utf8(&buf).unwrap(), ALLOW_PAYLOAD_JSON);
        assert_eq!(out, Some(PolicyDecision::Allowed));
    }

    #[test]
    fn maybe_emit_force_ask_overrides_auto_approve_all() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let mut buf: Vec<u8> = Vec::new();
        maybe_emit_permission_decision(&cli, &mut buf, Some(tmp.path()), None, true);
        assert!(buf.is_empty());
    }

    // ---------- run never returns Err ----------

    #[test]
    fn run_never_returns_err_on_any_stdin_variant() {
        struct ErroringReader;
        impl Read for ErroringReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "simulated"))
            }
        }
        let cli = cli_for(HookEvent::PostToolUse);
        let mut r = ErroringReader;
        let mut w: Vec<u8> = Vec::new();
        assert!(run_with_state(&cli, &mut r, &mut w, None).is_ok());
    }

    // ---------- F-053: permission.asked + permission.resolved pair ----------

    #[test]
    fn permission_request_emits_asked_and_resolved_pair() {
        let tmp = TempDir::new().unwrap();
        let cli = cli_for(HookEvent::PermissionRequest);
        seed_policy(tmp.path(), &cli, PermissionPolicy::AutoApproveAll);
        let payload = r#"{"session_id":"s1","cwd":"/tmp","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        run_sandboxed(&cli, payload.as_bytes(), Some(tmp.path().to_path_buf()));

        let path = session_dir(tmp.path(), &cli)
            .join("hooks")
            .join("PermissionRequest.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("permission.asked"));
        assert!(contents.contains("permission.resolved"));
        assert!(
            contents.find("permission.asked").unwrap()
                < contents.find("permission.resolved").unwrap(),
            "asked must precede resolved"
        );
    }
}
