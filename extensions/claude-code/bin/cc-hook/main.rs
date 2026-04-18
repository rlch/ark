//! `cc-hook` — Claude Code hook subprocess (T-006 salvage; R1 + R2).
//!
//! Invoked by `~/.claude/settings.json` hook entries on each of the 10
//! Claude Code hook event kinds (`SessionStart`, `SessionEnd`,
//! `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`,
//! `SubagentStop`, `Stop`, `PreCompact`, `Notification`). POSTs a
//! single NDJSON line per invocation to the per-session ark socket at
//! `$STATE/sessions/<sid>/cc-hook.sock`, then exits. Write-only — no
//! reverse messages. See `cavekit-claude-code.md` R1 + R2.
//!
//! # Invocation contract (R1)
//!
//! ```text
//! cc-hook --session <sid> --socket <path> --event <HookEventName>
//! ```
//!
//! Claude Code passes the hook payload on stdin as a single JSON
//! document (the `hook_event_name` field inside the payload is
//! redundant with `--event`; we prefer the flag for clap validation
//! and pass the payload through verbatim per R3).
//!
//! # Fail-open contract (R2)
//!
//! Every error path exits 0. Claude Code blocks its main loop while a
//! hook runs; returning non-zero would interfere with claude's
//! execution for reasons that have nothing to do with claude itself.
//! The hook is a pure observer in v0.1 (the `PermissionRequest`
//! allow-payload surface is a v0.2-stretch MCP concern, not v0.1).
//!
//! Errors routed to stderr via `tracing::warn!` — zellij hidden-pane
//! log capture / systemd journald surface these to operators.
//!
//! # What we did NOT salvage from the pre-2026-04-18 `crates/hook/`
//!
//! The legacy crate was a much bigger surface:
//!
//! - **FIFO + per-event JSONL writer** (`writer.rs`) — R2 replaces
//!   this with a single socket write. JSONL persistence lives
//!   ark-side now (on the socket reader, T-011) if at all; cc-hook
//!   itself is pure forwarder.
//! - **Zellij pipe forwarder** (`pipe.rs`) — ark's supervisor owns
//!   the status/picker pipe distribution now. cc-hook does not know
//!   about zellij.
//! - **Allow-payload stdout writer** (`allow.rs`) + **PermissionRequest
//!   policy plumbing** (`run.rs` maybe_emit_permission_decision) —
//!   the v0.1 Claude Code integration delegates permission handling
//!   entirely to claude's TUI. See `lib.rs` non-goal marker (T-008).
//! - **Bridge subcommands** (`bridge.rs`) — scene intent dispatch.
//!   Moved to `ark-bus` / the picker. Not an extension concern.
//! - **Per-event JSONL writer** (`writer.rs`) + **Policy file reader**
//!   — both permission-policy-adjacent; stay in git history only.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use tracing::{error, warn};
use tracing_subscriber::{EnvFilter, fmt};

use ark_ext_claude_code::{EXT_NAME, HookEvent, HookPayload, NdjsonLine};

/// Compile-time bridge version advertised on the first POST per
/// session (R4). Sourced from the `ark-ext-claude-code` crate version
/// so `ark ext claude-code reinstall-hook-binary` surfaces mismatches
/// as soon as the user re-installs the binary from a different crate
/// version than ark is running.
pub const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Connect / write deadline for the unix-socket POST. Keeps cc-hook
/// within Claude Code's practical hook budget even when ark is slow.
/// Pinned at 500ms — a healthy ark on the same machine replies in
/// <10ms; anything beyond half a second means something is wedged and
/// we should bail fail-open rather than stall claude's main loop.
const SOCKET_TIMEOUT_MS: u64 = 500;

/// `cc-hook` CLI arguments.
///
/// Per R1 invocation contract: `--session <sid> --socket <path>
/// --event <HookEventName>`. The hook payload arrives on stdin as a
/// single JSON document.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "cc-hook",
    about = "Claude Code hook subprocess (ark-ext-claude-code R1 + R2)",
    long_about = None,
    version,
)]
struct Cli {
    /// ark session id — the path leaf of `$STATE/sessions/<sid>/`.
    #[arg(long = "session")]
    session: String,

    /// Absolute path to the ark-side unix socket. Usually
    /// `$STATE/sessions/<sid>/cc-hook.sock`. The `$XDG_STATE_HOME`
    /// resolution happens at the caller — cc-hook treats the socket
    /// path as opaque bytes to keep the binary fully stateless.
    #[arg(long = "socket")]
    socket: PathBuf,

    /// Claude Code hook event name (`SessionStart`, `PostToolUse`, …).
    /// See [`HookEvent`] for the full enumeration.
    #[arg(long = "event")]
    event: HookEvent,

    /// First-POST-per-session marker. When set, the emitted NDJSON
    /// line carries `bridge_version` (R4). The ark-side socket reader
    /// decides whether it is actually the first POST it has seen for
    /// `session` — cc-hook is stateless so it cannot know. In
    /// practice, the settings.json installer (T-019) sets this flag
    /// on the `SessionStart` entry only; every other hook template
    /// omits it.
    ///
    /// Defaults off — operator who runs `cc-hook` by hand without the
    /// flag gets a lean payload.
    #[arg(long = "first-post", default_value_t = false)]
    first_post: bool,
}

fn main() -> ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // Even clap failure exits 0 — a misconfigured settings.json
            // entry MUST NOT wedge claude. We emit the clap error to
            // stderr so operators see why the hook is silent.
            //
            // Exception: --help / --version should print to stdout and
            // exit 0 with the normal clap flow. clap distinguishes
            // these via `ErrorKind::DisplayHelp` / `DisplayVersion`.
            use clap::error::ErrorKind;
            match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    e.print().ok();
                    return ExitCode::from(0);
                }
                _ => {
                    eprintln!("cc-hook: CLI parse failed: {e}");
                    return ExitCode::from(0);
                }
            }
        }
    };

    // Read the hook payload from stdin. Every failure here is a
    // fail-open path per R2.
    let mut buf = String::new();
    if let Err(e) = io::stdin().lock().read_to_string(&mut buf) {
        warn!(
            session = %cli.session,
            event = %cli.event,
            error = %e,
            "cc-hook: stdin read failed; fail-open exit 0"
        );
        return ExitCode::from(0);
    }

    // Parse payload — empty / malformed JSON still produces a
    // placeholder HookPayload so the ark side sees that the hook
    // fired. R3 "verbatim payload" is best-effort: if Claude Code
    // hands us junk, we carry whatever we got plus a typed event
    // marker rather than drop the event entirely.
    let payload = match parse_payload(&buf, &cli) {
        Some(p) => p,
        None => placeholder_payload(&cli),
    };

    let line = NdjsonLine {
        kind: cli.event.as_str().to_string(),
        session_id: cli.session.clone(),
        payload,
        emitted_at: chrono::Utc::now().to_rfc3339(),
        bridge_version: cli.first_post.then(|| BRIDGE_VERSION.to_string()),
    };

    let wire = match serde_json::to_string(&line) {
        Ok(s) => s,
        Err(e) => {
            error!(
                session = %cli.session,
                event = %cli.event,
                error = %e,
                "cc-hook: NDJSON serialise failed; fail-open exit 0"
            );
            return ExitCode::from(0);
        }
    };

    if let Err(e) = post_ndjson(&cli.socket, &wire) {
        warn!(
            session = %cli.session,
            event = %cli.event,
            socket = %cli.socket.display(),
            error = %e,
            "cc-hook: socket POST failed; fail-open exit 0 ({EXT_NAME})"
        );
        return ExitCode::from(0);
    }

    ExitCode::from(0)
}

/// Initialize tracing to stderr only. Stdout stays clean — Claude
/// Code's hook parser doesn't need anything from us on stdout in
/// v0.1, but keeping the channel empty leaves room for the v0.2 MCP
/// stretch without a protocol break.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .try_init();
}

/// Parse a stdin buffer into a [`HookPayload`]. Returns `None` on
/// empty input or malformed JSON, letting the caller fall back to a
/// placeholder payload so the NDJSON envelope still ships.
fn parse_payload(buf: &str, cli: &Cli) -> Option<HookPayload> {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        warn!(
            session = %cli.session,
            event = %cli.event,
            "cc-hook: stdin empty; sending placeholder payload"
        );
        return None;
    }
    match serde_json::from_str::<HookPayload>(trimmed) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!(
                session = %cli.session,
                event = %cli.event,
                error = %e,
                "cc-hook: stdin is not a valid HookPayload; sending placeholder payload"
            );
            None
        }
    }
}

/// Synthesize a minimal [`HookPayload`] when stdin is empty or
/// unparseable. Preserves the `hook_event_name` / `session_id` from the
/// CLI so ark-side consumers still see a well-typed envelope.
fn placeholder_payload(cli: &Cli) -> HookPayload {
    HookPayload {
        session_id: cli.session.clone(),
        cwd: PathBuf::new(),
        hook_event_name: cli.event.as_str().to_string(),
        tool_name: None,
        tool_input: None,
        extra: Default::default(),
    }
}

/// Connect to `socket`, write `wire\n`, close. Returns `Ok(())` on a
/// successful write; every other outcome (socket absent, unreachable,
/// write failed mid-stream) propagates as `Err` for the caller to log.
fn post_ndjson(socket: &std::path::Path, wire: &str) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| anyhow::anyhow!("connect {}: {e}", socket.display()))?;
    let to = Duration::from_millis(SOCKET_TIMEOUT_MS);
    stream.set_write_timeout(Some(to))?;
    stream.set_read_timeout(Some(to))?;

    let mut line = String::with_capacity(wire.len() + 1);
    line.push_str(wire);
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::io::BufReader;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;
    use tempfile::TempDir;

    fn short_sock(tag: &str, tmp: &TempDir) -> PathBuf {
        // macOS caps `sun_path` at 104 bytes; TempDir under /tmp keeps
        // the rendered path well below.
        let pid = std::process::id();
        tmp.path().join(format!("cc-hook-{tag}-{pid}.sock"))
    }

    #[test]
    fn placeholder_payload_preserves_event_name() {
        let cli = Cli {
            session: "sess".into(),
            socket: PathBuf::new(),
            event: HookEvent::SubagentStop,
            first_post: false,
        };
        let p = placeholder_payload(&cli);
        assert_eq!(p.hook_event_name, "SubagentStop");
        assert_eq!(p.session_id, "sess");
    }

    #[test]
    fn parse_payload_returns_none_for_empty_and_junk() {
        let cli = Cli {
            session: "s".into(),
            socket: PathBuf::new(),
            event: HookEvent::Stop,
            first_post: false,
        };
        assert!(parse_payload("", &cli).is_none());
        assert!(parse_payload("   \n", &cli).is_none());
        assert!(parse_payload("{not-json", &cli).is_none());
    }

    #[test]
    fn post_ndjson_writes_single_line_with_terminator() {
        let tmp = TempDir::new().unwrap();
        let sock = short_sock("write", &tmp);

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let listener = UnixListener::bind(&sock).expect("bind");
        let sock_path = sock.clone();
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            tx.send(line).ok();
            let _ = std::fs::remove_file(&sock_path);
        });

        post_ndjson(&sock, r#"{"kind":"Stop"}"#).expect("write ok");
        let got = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server got line");
        assert!(got.ends_with('\n'));
        assert!(got.trim_end().ends_with("}"));
        assert!(got.contains(r#""kind":"Stop""#));
    }

    #[test]
    fn post_ndjson_errors_when_socket_missing() {
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("nope.sock");
        let err = post_ndjson(&sock, "{}").expect_err("must error");
        assert!(err.to_string().contains("connect"));
    }

    #[test]
    fn bridge_version_matches_crate_version() {
        // Sanity: the handshake constant (R4) comes from CARGO_PKG_VERSION
        // so a bump to the crate version automatically updates the
        // advertised bridge version. Drift here would break T-010 tests.
        assert_eq!(BRIDGE_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
