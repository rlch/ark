//! Picker socket-command helpers (cavekit-plugin-picker R7 / W4).
//!
//! Small, host-side Unix-socket wrappers used by the kill / rename / forget
//! flows wired in T-105. They hand-roll JSON payloads because R1 bans
//! `serde_json` from the picker wasm binary — we stay consistent with the
//! bootstrap parser and [`crate::render_detail::query_agent_status`] by
//! writing the envelope bytes directly and reading one newline-terminated
//! response line back.
//!
//! # Acceptance criteria mapping (`cavekit-plugin-picker.md` R7)
//!
//! - `Kill` / `ForceKill` with `remove_worktree` argument: [`kill_cmd`].
//! - `Rename` with a single `name` arg: [`rename_cmd`] — escapes `"` and `\`
//!   in `name` so names containing quotes or backslashes survive the hand-
//!   rolled JSON envelope round-trip.
//! - `Forget` (no args): [`forget_cmd`].
//! - Transport-level reply envelope `{"ok":true,...}` / `{"ok":false,...}`
//!   consumed by [`send_command`]; all three helpers share that plumbing.
//!
//! # Banned crate reminder
//!
//! R1 bans `serde_json`. All payloads are composed with `format!` and the
//! hand-rolled [`escape_json_string`] so no `serde_json::json!` sneaks in.

use std::path::Path;

/// Read/write deadline for the socket round-trip.
///
/// Matches [`crate::render_detail::SOCKET_TIMEOUT_MS`] so kill / rename /
/// forget share the same "healthy supervisor replies inside 500ms"
/// assumption. Unreachable sockets collapse to [`SocketError::Unreachable`].
#[cfg(unix)]
const SOCKET_TIMEOUT_MS: u64 = 500;

/// Failure modes for the socket helpers.
///
/// Split three ways so the picker can render pointed error screens:
/// * `Unreachable` — connect/IO failed (supervisor dead / socket stale).
///   Drives R7 "agent no longer alive — refresh?" prompt in lib.rs.
/// * `ProtocolError` — response was not a readable NDJSON line / envelope.
/// * `Nak` — supervisor replied with `{"ok":false,"error":"..."}`; the
///   string payload is the human-readable message the user sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketError {
    /// Connect/IO failure.
    Unreachable,
    /// Response present but didn't match the `ok:true|false` envelope.
    ProtocolError(String),
    /// Supervisor returned a negative acknowledgement; carries the
    /// `"error"` string from the envelope verbatim.
    Nak(String),
}

/// Escape `"` and `\` in `s` for inclusion inside a hand-rolled JSON
/// string literal.
///
/// The picker's supervisor messages never contain control characters that
/// need `\uXXXX` escaping (names are shell-safe per ark-types'
/// validation), so covering `"` + `\` is sufficient for the W4 flows.
pub fn escape_json_string(s: &str) -> String {
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

/// Connect to `sock_path`, write `payload\n`, read one newline-terminated
/// response line, return it (without the trailing newline).
///
/// Timeouts mirror `query_agent_status` — 500ms read + write deadlines.
/// Any IO error collapses to `SocketError::Unreachable` so callers can
/// route the picker into the "agent no longer alive" recovery screen.
#[cfg(unix)]
pub fn send_command(sock_path: &Path, payload: &str) -> Result<String, SocketError> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(sock_path).map_err(|_| SocketError::Unreachable)?;
    let to = Duration::from_millis(SOCKET_TIMEOUT_MS);
    stream
        .set_read_timeout(Some(to))
        .map_err(|_| SocketError::Unreachable)?;
    stream
        .set_write_timeout(Some(to))
        .map_err(|_| SocketError::Unreachable)?;
    // Append a newline — supervisor control protocol is NDJSON (one
    // command per line, one response per line).
    let mut buf = String::with_capacity(payload.len() + 1);
    buf.push_str(payload);
    buf.push('\n');
    stream
        .write_all(buf.as_bytes())
        .map_err(|_| SocketError::Unreachable)?;
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|_| SocketError::Unreachable)?;
    // Strip trailing newline(s) so callers match on the envelope cleanly.
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err(SocketError::Unreachable);
    }
    Ok(line)
}

/// Non-unix stub — wasm32 doesn't have `std::os::unix::net`. The wasm
/// caller never invokes this (socket I/O is strictly host-side), but the
/// stub keeps `cargo check --target wasm32-wasip1` green without
/// `#[cfg]` around every use-site.
#[cfg(not(unix))]
pub fn send_command(_sock_path: &Path, _payload: &str) -> Result<String, SocketError> {
    Err(SocketError::Unreachable)
}

/// Inspect a response line for the NDJSON `{"ok":true}` envelope. Returns
/// `Ok(())` on success, `Err(Nak(msg))` when `"ok":false`, `Err(Protocol)`
/// if neither shape is detectable.
///
/// Both `"ok":true` and `"ok": true` (with space) are accepted — the
/// bootstrap parser uses the same tolerant style. The `"error"` field on
/// a Nak is extracted with the hand-rolled [`crate::bootstrap::find_string_field`]
/// so no serde_json creeps in.
fn parse_ack_envelope(line: &str) -> Result<(), SocketError> {
    let trimmed = line.trim();
    if trimmed.contains("\"ok\":true") || trimmed.contains("\"ok\": true") {
        return Ok(());
    }
    if trimmed.contains("\"ok\":false") || trimmed.contains("\"ok\": false") {
        let msg = crate::bootstrap::find_string_field(trimmed, "error").unwrap_or_else(String::new);
        return Err(SocketError::Nak(msg));
    }
    Err(SocketError::ProtocolError(trimmed.to_string()))
}

/// Build and send a `Kill` / `ForceKill` command.
///
/// Variants (matching the R7 wireframe legend):
/// * `force=false, keep_worktree=true` → `{"cmd":"Kill","args":{"remove_worktree":false}}`
/// * `force=true, keep_worktree=false` → `{"cmd":"ForceKill","args":{"remove_worktree":true}}`
///
/// T-105 only exercises the first two rows of that matrix; the function
/// is fully general so later polish tasks can reuse it.
pub fn kill_cmd(sock_path: &Path, force: bool, keep_worktree: bool) -> Result<(), SocketError> {
    let cmd = if force { "ForceKill" } else { "Kill" };
    let remove = if keep_worktree { "false" } else { "true" };
    let payload = format!("{{\"cmd\":\"{cmd}\",\"args\":{{\"remove_worktree\":{remove}}}}}");
    let line = send_command(sock_path, &payload)?;
    parse_ack_envelope(&line)
}

/// Build and send a `Rename` command with a single `name` argument.
///
/// `new_name` is run through [`escape_json_string`] so names containing
/// `"` / `\` produce valid JSON. The supervisor is expected to ack with
/// `{"ok":true}`; anything else maps to `ProtocolError` / `Nak`.
pub fn rename_cmd(sock_path: &Path, new_name: &str) -> Result<(), SocketError> {
    let escaped = escape_json_string(new_name);
    let payload = format!("{{\"cmd\":\"Rename\",\"args\":{{\"name\":\"{escaped}\"}}}}");
    let line = send_command(sock_path, &payload)?;
    parse_ack_envelope(&line)
}

/// Build and send a `Forget` command (no args).
///
/// Tells the supervisor to detach from the agent — the spec.json entry is
/// left on disk so a later resurrect flow can re-attach.
pub fn forget_cmd(sock_path: &Path) -> Result<(), SocketError> {
    let payload = "{\"cmd\":\"Forget\"}".to_string();
    let line = send_command(sock_path, &payload)?;
    parse_ack_envelope(&line)
}

// ---------------------------------------------------------------------------
// Tests — host-side only. Each test spins up an ephemeral UnixListener in
// a scratch tmpdir so there's no global state and no socket collisions
// between parallel test runs.
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;

    /// Ephemeral socket path under `std::env::temp_dir()` — random suffix
    /// so parallel tests don't stomp on each other.
    fn scratch_sock(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("ark-picker-{tag}-{pid}-{nanos}.sock"))
    }

    /// Spawn a one-shot listener that reads the first line, records it on
    /// a channel, then writes `reply\n` back.
    fn one_shot(sock: &Path, reply: &'static str) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel::<String>();
        let listener = UnixListener::bind(sock).expect("bind scratch sock");
        let sock_clone = sock.to_path_buf();
        thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            // Keep one reader for the request line + the underlying
            // stream available for the write back.
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).expect("read request");
            // Strip trailing newline to keep asserts clean.
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            tx.send(line).expect("send request over channel");
            let mut writer = stream;
            writer.write_all(reply.as_bytes()).expect("write reply");
            writer.flush().ok();
            // Clean up the socket path; the test is done with it.
            let _ = std::fs::remove_file(&sock_clone);
        });
        rx
    }

    // --- send_command -------------------------------------------------------

    #[test]
    fn send_command_reads_reply_line() {
        let sock = scratch_sock("sendok");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        let got = send_command(&sock, "{\"cmd\":\"Noop\"}").expect("send ok");
        assert_eq!(got, "{\"ok\":true}");
        // Sanity: the listener saw exactly our payload (newline stripped).
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(req, "{\"cmd\":\"Noop\"}");
    }

    #[test]
    fn send_command_missing_socket_is_unreachable() {
        let sock = scratch_sock("nosock");
        // Path deliberately does not exist.
        let err = send_command(&sock, "{\"cmd\":\"Noop\"}").unwrap_err();
        assert_eq!(err, SocketError::Unreachable);
    }

    // --- kill_cmd -----------------------------------------------------------

    #[test]
    fn kill_cmd_soft_keeps_worktree() {
        let sock = scratch_sock("killsoft");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        kill_cmd(&sock, false, true).expect("kill ok");
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(
            req,
            "{\"cmd\":\"Kill\",\"args\":{\"remove_worktree\":false}}"
        );
    }

    #[test]
    fn kill_cmd_force_removes_worktree() {
        let sock = scratch_sock("killforce");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        kill_cmd(&sock, true, false).expect("forcekill ok");
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(
            req,
            "{\"cmd\":\"ForceKill\",\"args\":{\"remove_worktree\":true}}"
        );
    }

    #[test]
    fn kill_cmd_nak_surfaces_error_string() {
        let sock = scratch_sock("killnak");
        let _rx = one_shot(&sock, "{\"ok\":false,\"error\":\"busy\"}\n");
        let err = kill_cmd(&sock, false, true).unwrap_err();
        assert_eq!(err, SocketError::Nak("busy".to_string()));
    }

    // --- rename_cmd ---------------------------------------------------------

    #[test]
    fn rename_cmd_sends_name_arg() {
        let sock = scratch_sock("renameok");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        rename_cmd(&sock, "newname").expect("rename ok");
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(req, "{\"cmd\":\"Rename\",\"args\":{\"name\":\"newname\"}}");
    }

    #[test]
    fn rename_cmd_escapes_quotes_and_backslashes() {
        // Sanity-check that new_name = `ab"c\d` produces valid JSON that
        // re-reads as `ab"c\d`.
        let sock = scratch_sock("renameesc");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        rename_cmd(&sock, "ab\"c\\d").expect("rename ok");
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        // Expected payload: the `"` is `\"`, the `\` is `\\`.
        assert_eq!(
            req,
            "{\"cmd\":\"Rename\",\"args\":{\"name\":\"ab\\\"c\\\\d\"}}"
        );
    }

    // --- forget_cmd ---------------------------------------------------------

    #[test]
    fn forget_cmd_no_args() {
        let sock = scratch_sock("forgetok");
        let rx = one_shot(&sock, "{\"ok\":true}\n");
        forget_cmd(&sock).expect("forget ok");
        let req = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(req, "{\"cmd\":\"Forget\"}");
    }

    // --- escape_json_string -------------------------------------------------

    #[test]
    fn escape_json_string_passes_plain() {
        assert_eq!(escape_json_string("hello"), "hello");
    }

    #[test]
    fn escape_json_string_escapes_quote() {
        assert_eq!(escape_json_string("a\"b"), "a\\\"b");
    }

    #[test]
    fn escape_json_string_escapes_backslash() {
        assert_eq!(escape_json_string("a\\b"), "a\\\\b");
    }

    // --- parse_ack_envelope -------------------------------------------------

    #[test]
    fn parse_ack_accepts_ok_true() {
        assert!(parse_ack_envelope("{\"ok\":true}").is_ok());
        assert!(parse_ack_envelope("{\"ok\": true}").is_ok());
    }

    #[test]
    fn parse_ack_rejects_ok_false_with_error_payload() {
        let err = parse_ack_envelope("{\"ok\":false,\"error\":\"boom\"}").unwrap_err();
        assert_eq!(err, SocketError::Nak("boom".to_string()));
    }

    #[test]
    fn parse_ack_rejects_unknown_envelope() {
        match parse_ack_envelope("not-json") {
            Err(SocketError::ProtocolError(msg)) => assert_eq!(msg, "not-json"),
            other => panic!("expected ProtocolError, got {other:?}"),
        }
    }
}
