//! `mock-claude-cc` (Claude-Code-specific variant) — fixture binary
//! used by the `ark-ext-claude-code` crate's integration tests.
//!
//! Disambiguated from the generic fixture crate's `mock-claude` by the
//! `-cc` suffix (cargo rejects workspace-wide bin-name duplicates as a
//! future hard error). The binary lives under
//! `crates/test-fixtures/claude-code/` rather than inside the generic
//! fixture crate because its surface (emit-only / subagent-burst /
//! transcript-write) is Claude-Code-specific enough that mixing it
//! with the generic `--script PATH` harness would make the latter's
//! contract less obvious.
//!
//! # Task provenance
//!
//! - **T-017** — cc-hook NDJSON timeline driver: flags `--emit-only`,
//!   `--subagent-burst N`, `--transcript-write <path>`. Emits scripted
//!   event timelines covering `SessionStart`, `SessionEnd`,
//!   `SubagentStart`, `SubagentStop`, `PreToolUse`, `PostToolUse`.
//! - **T-018** — transcript-synth helper: writes JSONL lines matching
//!   Claude Code's real transcript shape (`type`, `role`, `content`,
//!   `model`, `usage`, `cost_usd` fields) to a configurable path,
//!   flushing after each line so downstream tailers see live progress.
//!
//! # Emission contract
//!
//! Every event is a single NDJSON line printed to stdout in the shape
//! cc-hook POSTs to ark's per-session socket:
//!
//! ```json
//! { "kind": "SubagentStop",
//!   "session_id": "mock-sess",
//!   "payload": { ... Claude Code hook JSON verbatim ... },
//!   "emitted_at": "2026-04-18T00:00:00Z" }
//! ```
//!
//! This is *not* the same wire shape as Claude Code's own settings.json
//! hooks (those receive the payload on stdin, not NDJSON on stdout).
//! The reason is that these fixtures feed `cc-hook`'s stdin in tests —
//! `cat timeline.ndjson | cc-hook ...` is the shortest path from
//! scripted event → socket POST without running a real `claude` binary.
//!
//! # Deterministic vs wall-clock fields
//!
//! `emitted_at` defaults to a fixed RFC 3339 stamp (`2026-04-18T00:00:00Z`)
//! so golden tests stay stable. The `--now` flag swaps to wall clock for
//! long-running fixtures that want observable monotonicity. Session ids
//! and agent ids are deterministic indexes (`mock-sess`, `agent-0`,
//! `agent-1`, …) unless overridden with `--session-id`.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::Utc;
use clap::Parser;
use serde_json::{Value, json};

/// Deterministic default emission timestamp. See module doc.
const DEFAULT_EMITTED_AT: &str = "2026-04-18T00:00:00Z";

/// Deterministic default session id.
const DEFAULT_SESSION_ID: &str = "mock-sess";

/// Default model string baked into synthesised transcript lines. Picked
/// to match what Claude Code actually ships in Apr 2026 — downstream
/// view code (T-036 expanded view, T-044 list columns) pivots on this
/// value for rendering.
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

#[derive(Parser, Debug)]
#[command(
    name = "mock-claude-cc",
    about = "Emit scripted Claude Code hook NDJSON timelines + optional transcript synth for R13 cc-hook tests.",
    version
)]
struct Cli {
    /// Emit a single canonical happy-path timeline:
    /// `SessionStart` → `UserPromptSubmit` → `PreToolUse` →
    /// `PostToolUse` → `SessionEnd`. Useful for smoke-testing cc-hook's
    /// NDJSON reader without any options.
    #[arg(long)]
    emit_only: bool,

    /// Emit N sequential `SubagentStart` + `SubagentStop` pairs, wrapped
    /// in a `SessionStart` / `SessionEnd` envelope. Each subagent gets
    /// a deterministic `agent-<i>` id and a synthetic transcript path
    /// that lands in `--transcript-write`'s sibling dir when that flag
    /// is also present.
    #[arg(long, value_name = "N")]
    subagent_burst: Option<u32>,

    /// Path to append JSONL-formatted transcript lines to (T-018). When
    /// present, each "assistant message" event in the timeline appends
    /// one line in Claude Code's real transcript shape. Parent dirs are
    /// created on demand; the file is opened append+truncate on first
    /// write then appended thereafter.
    #[arg(long, value_name = "PATH")]
    transcript_write: Option<PathBuf>,

    /// Override the deterministic emission timestamp. Without this the
    /// binary emits `2026-04-18T00:00:00Z` on every line; with `--now`
    /// it stamps the actual wall clock.
    #[arg(long)]
    now: bool,

    /// Override the session id baked into every NDJSON line + transcript
    /// path. Defaults to `mock-sess`.
    #[arg(long, default_value = DEFAULT_SESSION_ID)]
    session_id: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mock-claude-cc: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> io::Result<()> {
    // At most one timeline mode at a time — otherwise event ordering
    // becomes non-obvious. `--transcript-write` is orthogonal and
    // composes with any mode.
    let modes = [cli.emit_only, cli.subagent_burst.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if modes > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exactly one of --emit-only / --subagent-burst may be set",
        ));
    }

    // Transcript-write sink is shared across the whole timeline so
    // "SubagentStop" events can log a final assistant message at the
    // right byte-offset. Created up front so parent-dir failures
    // surface before any NDJSON is emitted.
    let mut transcript: Option<BufWriter<File>> = match cli.transcript_write.as_ref() {
        Some(p) => Some(open_transcript(p)?),
        None => None,
    };

    let stdout = io::stdout();
    let mut out = stdout.lock();

    let emitted_at = if cli.now {
        Utc::now().to_rfc3339()
    } else {
        DEFAULT_EMITTED_AT.to_string()
    };

    if cli.emit_only {
        emit_happy_path(&mut out, &cli.session_id, &emitted_at, transcript.as_mut())?;
    } else if let Some(n) = cli.subagent_burst {
        emit_subagent_burst(
            &mut out,
            &cli.session_id,
            &emitted_at,
            n,
            transcript.as_mut(),
        )?;
    } else {
        // No mode selected: the binary exits 0 with no output. Tests
        // that only need the transcript writer exercised can pass
        // `--transcript-write PATH` alone — but then they also need to
        // decide what to synthesise. Keep the no-op path so tests can
        // assert "parent dir created, file empty" without surprise
        // NDJSON on stdout.
        out.flush()?;
        if let Some(w) = transcript.as_mut() {
            w.flush()?;
        }
        return Ok(());
    }

    // Best-effort flush; if the consumer closed stdout early we'll
    // return the EPIPE and let the caller decide what to do.
    out.flush()?;
    if let Some(w) = transcript.as_mut() {
        w.flush()?;
    }

    Ok(())
}

// ---------- T-017 timelines ----------

/// The canonical happy-path timeline: one session, one prompt, one tool
/// use, one session end. Exercises every hook kind a basic cc-hook
/// integration test needs. Per-event fields chosen so the payload
/// fields tested in T-015 show up at least once.
fn emit_happy_path(
    out: &mut impl Write,
    session_id: &str,
    emitted_at: &str,
    mut transcript: Option<&mut BufWriter<File>>,
) -> io::Result<()> {
    write_ndjson(
        out,
        session_id,
        emitted_at,
        "SessionStart",
        session_start_payload(session_id),
    )?;
    write_ndjson(
        out,
        session_id,
        emitted_at,
        "UserPromptSubmit",
        user_prompt_submit_payload(session_id, "refactor foo()"),
    )?;
    write_ndjson(
        out,
        session_id,
        emitted_at,
        "PreToolUse",
        pre_tool_use_payload(session_id, "Edit", "/repo/src/lib.rs"),
    )?;
    write_ndjson(
        out,
        session_id,
        emitted_at,
        "PostToolUse",
        post_tool_use_payload(session_id, "Edit", "/repo/src/lib.rs", "ok"),
    )?;

    // T-018 — the canonical timeline writes one assistant message so
    // tests that tail the transcript in --emit-only mode see a line
    // land alongside the NDJSON stream.
    if let Some(w) = transcript.as_deref_mut() {
        write_transcript_line(
            w,
            "assistant",
            "Rewrote foo() to use iter() instead of a manual loop.",
            DEFAULT_MODEL,
            42,
            17,
        )?;
    }

    write_ndjson(
        out,
        session_id,
        emitted_at,
        "SessionEnd",
        session_end_payload(session_id),
    )?;
    Ok(())
}

/// N sequential SubagentStart + SubagentStop pairs inside a Session
/// envelope. Each subagent gets its own `agent-<i>` id + synthetic
/// transcript path that matches the real Claude Code layout
/// (`<base>/subagents/<agent-id>.jsonl` relative to the parent session
/// transcript). When a `--transcript-write` sink is present, each
/// SubagentStop flushes one synthetic assistant message matching the
/// agent's last-message extra field so tests can round-trip via the
/// real transcript reader.
fn emit_subagent_burst(
    out: &mut impl Write,
    session_id: &str,
    emitted_at: &str,
    n: u32,
    mut transcript: Option<&mut BufWriter<File>>,
) -> io::Result<()> {
    write_ndjson(
        out,
        session_id,
        emitted_at,
        "SessionStart",
        session_start_payload(session_id),
    )?;

    for i in 0..n {
        let agent_id = format!("agent-{i}");
        let agent_type = "code-writer";
        let transcript_path = format!("/tmp/mock-claude/{session_id}/subagents/{agent_id}.jsonl");

        write_ndjson(
            out,
            session_id,
            emitted_at,
            "SubagentStart",
            subagent_start_payload(session_id, &agent_id, agent_type, &transcript_path),
        )?;

        let last_msg = format!("subagent {agent_id} done");

        // T-018 — matching transcript line flushed BEFORE the Stop
        // NDJSON so a downstream tailer can correlate the message with
        // the subagent-stop event rather than racing it.
        if let Some(w) = transcript.as_deref_mut() {
            write_transcript_line(w, "assistant", &last_msg, DEFAULT_MODEL, 10, 5)?;
        }

        write_ndjson(
            out,
            session_id,
            emitted_at,
            "SubagentStop",
            subagent_stop_payload(
                session_id,
                &agent_id,
                agent_type,
                &transcript_path,
                &last_msg,
            ),
        )?;
    }

    write_ndjson(
        out,
        session_id,
        emitted_at,
        "SessionEnd",
        session_end_payload(session_id),
    )?;
    Ok(())
}

// ---------- NDJSON envelope builder ----------

/// Write one NDJSON-encoded R2 envelope to `out`, ending in `\n`.
/// Matches the shape cc-hook POSTs to ark's per-session socket so the
/// ark-side reader (`CcHookSocket::handle_connection`) decodes this
/// binary's stdout bit-for-bit identically.
fn write_ndjson(
    out: &mut impl Write,
    session_id: &str,
    emitted_at: &str,
    kind: &str,
    payload: Value,
) -> io::Result<()> {
    let line = json!({
        "kind": kind,
        "session_id": session_id,
        "payload": payload,
        "emitted_at": emitted_at,
    });
    serde_json::to_writer(&mut *out, &line).map_err(io::Error::other)?;
    out.write_all(b"\n")?;
    Ok(())
}

// ---------- Payload factories ----------
//
// Each factory returns a `serde_json::Value` matching the corresponding
// Claude Code hook JSON. Field choices are aligned with the T-015
// payload-fields test so a test that pipes mock-claude output through
// cc-hook + the ark-side translator sees the same extras land.

fn session_start_payload(session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "SessionStart",
    })
}

fn session_end_payload(session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "SessionEnd",
    })
}

fn user_prompt_submit_payload(session_id: &str, prompt: &str) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "UserPromptSubmit",
        "prompt": prompt,
    })
}

fn pre_tool_use_payload(session_id: &str, tool: &str, file_path: &str) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": { "file_path": file_path, "old_string": "a", "new_string": "b" },
    })
}

fn post_tool_use_payload(session_id: &str, tool: &str, file_path: &str, status: &str) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "tool_input": { "file_path": file_path },
        "tool_response": { "status": status },
    })
}

fn subagent_start_payload(
    session_id: &str,
    agent_id: &str,
    agent_type: &str,
    transcript_path: &str,
) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "SubagentStart",
        "agent_id": agent_id,
        "agent_type": agent_type,
        "agent_transcript_path": transcript_path,
    })
}

fn subagent_stop_payload(
    session_id: &str,
    agent_id: &str,
    agent_type: &str,
    transcript_path: &str,
    last_msg: &str,
) -> Value {
    json!({
        "session_id": session_id,
        "cwd": "/tmp/mock-claude-cwd",
        "hook_event_name": "SubagentStop",
        "agent_id": agent_id,
        "agent_type": agent_type,
        "agent_transcript_path": transcript_path,
        "last_assistant_message": last_msg,
    })
}

// ---------- T-018 transcript synth ----------

/// Open the transcript file for append, creating it + any parent
/// directories on demand. Opened once per run; every synth line is
/// appended + flushed so a tailer sees lines land in real time.
fn open_transcript(path: &Path) -> io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let f = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(BufWriter::new(f))
}

/// Append one JSONL transcript line in Claude Code's real shape. Fields
/// are the subset downstream view code reads (kit R6 + R11):
///
/// ```json
/// {"type":"message",
///  "role":"assistant",
///  "content":[{"type":"text","text":"..."}],
///  "model":"claude-sonnet-4-6",
///  "usage":{"input_tokens":42,"output_tokens":17},
///  "cost_usd":0.003}
/// ```
///
/// The `cost_usd` calc is a fixed-rate mock (`input*3e-6 + output*15e-6`)
/// picked to land in a plausible range for view snapshots without a
/// per-test tuning knob.
fn write_transcript_line(
    w: &mut BufWriter<File>,
    role: &str,
    text: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> io::Result<()> {
    // Mock cost: 3 ¢/M input, 15 ¢/M output. Matches the real
    // Sonnet-tier price *order of magnitude* so view snapshots look
    // reasonable; not precise enough to substitute for real Anthropic
    // billing data.
    let cost_usd = (input_tokens as f64) * 3e-6 + (output_tokens as f64) * 15e-6;
    let line = json!({
        "type": "message",
        "role": role,
        "content": [ { "type": "text", "text": text } ],
        "model": model,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens },
        "cost_usd": cost_usd,
    });
    serde_json::to_writer(&mut *w, &line).map_err(io::Error::other)?;
    w.write_all(b"\n")?;
    // Flush so a downstream tailer sees this line land before the next
    // NDJSON event on stdout. Without this, BufWriter could hold a full
    // timeline's worth of lines until the process exits, and tests
    // checking "tail sees line N after event N" would race.
    w.flush()?;
    Ok(())
}
