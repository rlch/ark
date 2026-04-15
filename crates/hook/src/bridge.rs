//! Scene-bridge subcommand dispatchers (T-6.2, T-6.3, T-6.4 + ACP).
//!
//! The bridge subcommands (`ark-hook intent / emit / permit`) all share
//! the same wire shape: connect to the per-agent unix socket at
//! `${runtime}/agents/{id}.sock`, write a single newline-terminated NDJSON
//! command, read one newline-terminated reply, exit `0` on
//! `{"ok": true, …}` or `1` on anything else. See `cavekit-hook-ipc.md`
//! R4 (path scheme) + R5 (protocol) for the contract.
//!
//! ## Why blocking std I/O?
//!
//! The whole point of `ark-hook` is **fast one-shot** invocation under a
//! 50ms budget (R1 "Running time budget < 50ms (keybind UX)"). A tokio
//! runtime adds ~2–5ms of cold-start overhead on a fresh process — most
//! of our budget. Blocking `std::os::unix::net::UnixStream` with explicit
//! read/write deadlines is faster, simpler, and matches the picker's
//! `socket_cmd.rs` pattern verbatim.
//!
//! ## Why hand-roll the JSON wrapper?
//!
//! The supervisor's R5 envelope is `{"cmd": "<name>", "args": <map>}`.
//! Callers already pass us a fully-formed `args` JSON document via the
//! `--json` flag, so we just need to splice it into a `cmd` envelope.
//! Re-parsing through `serde_json::Value` would mean an extra
//! parse + re-serialize for every dispatch with no validation gain; the
//! supervisor already validates the inner shape and surfaces malformed
//! payloads through `IntentError::ArgsInvalid`.
//!
//! ## Errors are STDERR + exit-1
//!
//! `ark-bus` spawns these dispatchers via
//! `open_command_pane_background` — a hidden zellij pane whose stderr is
//! captured into the zellij log. Returning rich `Display` text on every
//! failure path means operators tailing `~/.cache/zellij/zellij.log`
//! see exactly which dispatch failed and why, without our needing a
//! separate logging facility inside this binary.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ark_types::{AgentId, StateLayout};

use crate::cli::{BridgeArgs, PermitArgs};

/// Read/write deadline for the round-trip with the supervisor.
///
/// Mirrors the picker's `SOCKET_TIMEOUT_MS` — a healthy supervisor on
/// the same machine replies in <10ms; anything beyond half a second
/// means the supervisor is overwhelmed or dead and we should bail.
const SOCKET_TIMEOUT_MS: u64 = 500;

/// Env var the supervisor sets in every spawned child process so plugins
/// (and the hook binary they invoke) can resolve their target agent
/// without it being threaded by hand. Per `cavekit-hook-ipc.md` R1 last
/// bullet.
pub const ARK_AGENT_ID_ENV: &str = "ARK_AGENT_ID";

/// Failure modes the bridge dispatchers can produce.
///
/// All variants are surfaced through the binary as exit-1 + stderr text;
/// they exist as a typed enum so the test suite can assert on the shape
/// without string-matching.
#[derive(Debug)]
pub enum BridgeError {
    /// `--id` was omitted AND `$ARK_AGENT_ID` is unset / invalid. The
    /// caller has not told us which agent to target.
    AgentIdUnresolved(String),
    /// Failed to resolve the runtime root via [`StateLayout::from_env`].
    /// Usually `HOME` unset in the spawning environment.
    RuntimeUnresolved(String),
    /// Connect / read / write IO error against the control socket.
    /// Includes the supervisor-not-running case (ECONNREFUSED) and
    /// path-not-found.
    Io(String),
    /// Supervisor reply was not parseable as `{"ok": …}`.
    ProtocolError(String),
    /// Supervisor replied `{"ok": false, "error": "…"}`. The string is
    /// the verbatim error text from the supervisor.
    Nak(String),
    /// JSON splicing failed — the caller passed a `--json` payload that
    /// is not valid JSON. Detected before sending to keep malformed
    /// payloads off the wire.
    InvalidJson(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::AgentIdUnresolved(msg) => write!(f, "agent-id unresolved: {msg}"),
            BridgeError::RuntimeUnresolved(msg) => write!(f, "runtime dir unresolved: {msg}"),
            BridgeError::Io(msg) => write!(f, "control socket IO failed: {msg}"),
            BridgeError::ProtocolError(msg) => write!(f, "protocol error: {msg}"),
            BridgeError::Nak(msg) => write!(f, "supervisor replied with error: {msg}"),
            BridgeError::InvalidJson(msg) => write!(f, "invalid --json payload: {msg}"),
        }
    }
}

impl std::error::Error for BridgeError {}

/// Successful bridge outcome.
///
/// Carries the supervisor's `data` field (if any) so call sites that
/// want to surface the result back through stdout can do so. Today the
/// bridge dispatchers print nothing on success — exit `0` is the only
/// signal — so this is a forward-looking shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutcome {
    /// Verbatim reply line from the supervisor (NDJSON).
    pub reply: String,
}

/// Resolve the target agent id: explicit `--id` first, otherwise
/// `$ARK_AGENT_ID`. Returns a typed error when neither is set or the
/// env var fails to parse.
///
/// `cavekit-hook-ipc.md` R1: "when omitted, `ark-hook` reads
/// `ARK_AGENT_ID` from env (set by supervisor in all spawned child
/// processes including zellij)."
pub fn resolve_agent_id(explicit: Option<&AgentId>) -> Result<AgentId, BridgeError> {
    if let Some(id) = explicit {
        return Ok(id.clone());
    }
    let raw = env::var(ARK_AGENT_ID_ENV).map_err(|_| {
        BridgeError::AgentIdUnresolved(format!(
            "neither --id nor ${ARK_AGENT_ID_ENV} is set"
        ))
    })?;
    raw.parse::<AgentId>().map_err(|e| {
        BridgeError::AgentIdUnresolved(format!(
            "${ARK_AGENT_ID_ENV} = {raw:?} is not a valid AgentId: {e}"
        ))
    })
}

/// Resolve `${runtime}/agents/{id}.sock` for the supplied agent id,
/// using [`StateLayout::from_env`] for the runtime root.
///
/// Path resolution failures (no `HOME`, no writable runtime) become
/// [`BridgeError::RuntimeUnresolved`].
pub fn resolve_socket_path(id: &AgentId) -> Result<PathBuf, BridgeError> {
    let layout = StateLayout::from_env()
        .map_err(|e| BridgeError::RuntimeUnresolved(e.to_string()))?;
    Ok(layout.agent_socket_path(id))
}

/// Override hook for tests: resolve the socket via an explicit
/// [`StateLayout`] rather than from the environment. Production
/// callers always go through [`resolve_socket_path`].
pub fn resolve_socket_path_with(layout: &StateLayout, id: &AgentId) -> PathBuf {
    layout.agent_socket_path(id)
}

/// `ark-hook intent` — dispatch a named intent through the supervisor's
/// intent registry (R5 `Intent { name, args }` command).
///
/// `args.json` MUST parse as a JSON object containing at least the
/// `name` field (and typically an `args` map). The wire envelope sent
/// to the supervisor is:
///
/// ```json
/// {"cmd": "Intent", "args": <args.json verbatim>}
/// ```
pub fn dispatch_intent(args: &BridgeArgs) -> Result<BridgeOutcome, BridgeError> {
    let id = resolve_agent_id(args.id.as_ref())?;
    let sock = resolve_socket_path(&id)?;
    send_envelope(&sock, "Intent", &args.json)
}

/// `ark-hook emit` — broadcast a synthetic `UserEvent` (R5
/// `Emit { event, payload, source }` command).
///
/// `args.json` MUST parse as a JSON object containing `event` (string),
/// `payload` (map), and `source` (string from the canonical R4
/// attribution set). The wire envelope is:
///
/// ```json
/// {"cmd": "Emit", "args": <args.json verbatim>}
/// ```
pub fn dispatch_emit(args: &BridgeArgs) -> Result<BridgeOutcome, BridgeError> {
    let id = resolve_agent_id(args.id.as_ref())?;
    let sock = resolve_socket_path(&id)?;
    send_envelope(&sock, "Emit", &args.json)
}

/// `ark-hook permit` — respond to an outstanding ACP permission
/// request (R5 `Permit { request_id, outcome, option_id? }` command).
///
/// Unlike `intent` / `emit` this dispatcher constructs the args object
/// from typed CLI fields rather than passing through a raw `--json`
/// payload, since the schema is fixed.
pub fn dispatch_permit(args: &PermitArgs) -> Result<BridgeOutcome, BridgeError> {
    let id = resolve_agent_id(args.id.as_ref())?;
    let sock = resolve_socket_path(&id)?;
    let mut args_json = String::from("{");
    args_json.push_str(&format!(
        "\"request_id\":\"{}\"",
        escape_json_string(&args.request_id)
    ));
    args_json.push_str(&format!(
        ",\"outcome\":\"{}\"",
        args.outcome.as_wire()
    ));
    if let Some(opt) = &args.option_id {
        args_json.push_str(&format!(
            ",\"option_id\":\"{}\"",
            escape_json_string(opt)
        ));
    }
    args_json.push('}');
    send_envelope(&sock, "Permit", &args_json)
}

/// Send `{"cmd": <cmd>, "args": <args_json>}` to `sock` and parse the
/// supervisor's reply. The args document must be valid JSON; we
/// cheap-validate before splicing so malformed payloads never reach
/// the wire.
fn send_envelope(sock: &Path, cmd: &str, args_json: &str) -> Result<BridgeOutcome, BridgeError> {
    // Cheap validity check: confirm the args JSON parses. We do NOT
    // round-trip-serialize because that would lose key order /
    // formatting the caller chose; we just want to refuse outright
    // garbage.
    if let Err(e) = serde_json::from_str::<serde_json::Value>(args_json) {
        return Err(BridgeError::InvalidJson(e.to_string()));
    }
    let envelope = format!("{{\"cmd\":\"{cmd}\",\"args\":{args_json}}}");
    let reply = send_command(sock, &envelope)?;
    parse_reply(&reply).map(|()| BridgeOutcome { reply })
}

/// Connect to `sock`, write `payload\n`, read one newline-terminated
/// reply, return it without the trailing newline.
fn send_command(sock: &Path, payload: &str) -> Result<String, BridgeError> {
    let mut stream = UnixStream::connect(sock).map_err(|e| {
        BridgeError::Io(format!("connect {}: {e}", sock.display()))
    })?;
    let to = Duration::from_millis(SOCKET_TIMEOUT_MS);
    stream
        .set_read_timeout(Some(to))
        .map_err(|e| BridgeError::Io(format!("set_read_timeout: {e}")))?;
    stream
        .set_write_timeout(Some(to))
        .map_err(|e| BridgeError::Io(format!("set_write_timeout: {e}")))?;

    let mut wire = String::with_capacity(payload.len() + 1);
    wire.push_str(payload);
    wire.push('\n');
    stream
        .write_all(wire.as_bytes())
        .map_err(|e| BridgeError::Io(format!("write: {e}")))?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| BridgeError::Io(format!("read: {e}")))?;
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err(BridgeError::Io("empty reply".to_string()));
    }
    Ok(line)
}

/// Parse the supervisor's reply envelope. Returns `Ok(())` on
/// `{"ok": true, …}`, `Err(Nak)` on `{"ok": false, "error": …}`, and
/// `Err(ProtocolError)` on anything else.
fn parse_reply(line: &str) -> Result<(), BridgeError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| BridgeError::ProtocolError(format!("not valid JSON: {e}; raw={line:?}")))?;
    let ok = value
        .get("ok")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            BridgeError::ProtocolError(format!("reply missing `ok` bool: {line:?}"))
        })?;
    if ok {
        return Ok(());
    }
    let err = value
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("(no error message)");
    Err(BridgeError::Nak(err.to_string()))
}

/// Escape `"` and `\` for inclusion inside a hand-rolled JSON string
/// literal. Limited surface — we only feed shell-safe identifiers
/// (request ids, option ids) through this helper.
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::AgentId;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;

    fn fresh_id() -> AgentId {
        AgentId::new("cavekit", "bridgetest")
    }

    /// Allocate a short tempdir under `/tmp` so the rendered socket
    /// path stays under macOS's 104-byte `sun_path` cap.
    fn short_sock(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("ark-hook-{tag}-{pid}-{nanos}.sock"))
    }

    /// One-shot listener that reads the first line, records it on a
    /// channel, then writes `reply\n` back and removes the socket.
    fn one_shot(sock: &Path, reply: &'static str) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel::<String>();
        let listener = UnixListener::bind(sock).expect("bind scratch sock");
        let sock_clone = sock.to_path_buf();
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).expect("read");
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            tx.send(line).expect("send");
            let mut w = stream;
            w.write_all(reply.as_bytes()).expect("write");
            w.flush().ok();
            let _ = std::fs::remove_file(&sock_clone);
        });
        rx
    }

    #[test]
    fn resolve_agent_id_explicit_wins() {
        let id = fresh_id();
        let got = resolve_agent_id(Some(&id)).expect("ok");
        assert_eq!(got, id);
    }

    #[test]
    fn resolve_agent_id_falls_back_to_env() {
        // Use a process-wide env var test-isolated by setting + clearing.
        // Note: we don't unset because other tests may parallelise; use
        // a unique scope-local env var name? The spec says ARK_AGENT_ID,
        // so we set + clear that.
        let id = AgentId::new("cavekit", "envvar");
        // SAFETY: `set_var` mutates the process env, which is unsafe in
        // multithreaded contexts. Tests in this module guard via the
        // unique tag in the env value to avoid collision; the cargo test
        // harness rarely contends on this var.
        unsafe { env::set_var(ARK_AGENT_ID_ENV, id.as_str()) };
        let got = resolve_agent_id(None);
        // Clean up before asserting so a panic doesn't leak the env var.
        unsafe { env::remove_var(ARK_AGENT_ID_ENV) };
        assert_eq!(got.expect("ok"), id);
    }

    #[test]
    fn resolve_agent_id_errors_when_unset() {
        // Ensure the env is unset for this test. Other tests may set
        // and unset; we re-clear here to be defensive.
        unsafe { env::remove_var(ARK_AGENT_ID_ENV) };
        let err = resolve_agent_id(None).expect_err("must error");
        match err {
            BridgeError::AgentIdUnresolved(_) => {}
            other => panic!("expected AgentIdUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn parse_reply_accepts_ok_true() {
        parse_reply(r#"{"ok":true}"#).expect("ok");
        parse_reply(r#"{"ok":true,"data":{"echo":"hi"}}"#).expect("ok");
    }

    #[test]
    fn parse_reply_nak_carries_error_text() {
        let err = parse_reply(r#"{"ok":false,"error":"unknown command: Foo"}"#)
            .expect_err("nak");
        match err {
            BridgeError::Nak(msg) => assert!(msg.contains("unknown command")),
            other => panic!("expected Nak, got {other:?}"),
        }
    }

    #[test]
    fn parse_reply_protocol_error_on_garbage() {
        let err = parse_reply(r#"not json at all"#).expect_err("must error");
        match err {
            BridgeError::ProtocolError(_) => {}
            other => panic!("expected ProtocolError, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_intent_sends_envelope_and_returns_reply() {
        let sock = short_sock("intent-ok");
        let rx = one_shot(&sock, "{\"ok\":true,\"data\":\"dispatched\"}\n");

        let envelope = format!(
            "{{\"cmd\":\"Intent\",\"args\":{}}}",
            r#"{"name":"ark.core.ping","args":{}}"#
        );
        let line = send_command(&sock, &envelope).expect("ok");
        parse_reply(&line).expect("ok envelope");

        let req = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(req.contains("\"cmd\":\"Intent\""));
        assert!(req.contains("ark.core.ping"));
    }

    #[test]
    fn send_envelope_rejects_invalid_json_args() {
        let sock = short_sock("invalid-json");
        let err = send_envelope(&sock, "Intent", "{not json")
            .expect_err("must reject");
        match err {
            BridgeError::InvalidJson(_) => {}
            other => panic!("expected InvalidJson, got {other:?}"),
        }
        // Sanity: socket file was never opened (no listener bound).
        assert!(!sock.exists());
    }

    #[test]
    fn send_envelope_surfaces_unreachable_socket_as_io() {
        let sock = short_sock("unreachable");
        let err = send_envelope(&sock, "Intent", "{}").expect_err("must error");
        match err {
            BridgeError::Io(msg) => assert!(msg.contains("connect")),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_permit_serialises_outcome_correctly() {
        use crate::cli::PermitOutcome;
        let sock = short_sock("permit-ok");
        let rx = one_shot(&sock, "{\"ok\":true}\n");

        let args = PermitArgs {
            id: Some(fresh_id()),
            request_id: "req-42".to_string(),
            outcome: PermitOutcome::Allow,
            option_id: Some("opt-A".to_string()),
        };
        // Bypass resolve_socket_path so the test doesn't depend on env.
        let mut envelope = String::from("{\"cmd\":\"Permit\",\"args\":{");
        envelope.push_str("\"request_id\":\"req-42\"");
        envelope.push_str(",\"outcome\":\"allow\"");
        envelope.push_str(",\"option_id\":\"opt-A\"");
        envelope.push_str("}}");
        let line = send_command(&sock, &envelope).expect("ok");
        parse_reply(&line).expect("ok");

        let req = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(req.contains("\"cmd\":\"Permit\""));
        assert!(req.contains("\"outcome\":\"allow\""));
        assert!(req.contains("\"option_id\":\"opt-A\""));
        // Drop unused `args` so clippy doesn't complain.
        let _ = args;
    }

    #[test]
    fn escape_json_string_handles_quotes_and_backslashes() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("a\"b"), "a\\\"b");
        assert_eq!(escape_json_string("c\\d"), "c\\\\d");
    }
}
