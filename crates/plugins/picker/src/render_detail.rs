//! Picker detail-screen rendering + socket query (cavekit-plugin-picker R5 / W2).
//!
//! The detail screen renders as an indented expand-tree under the currently
//! selected list row (session-manager style). It pulls a fresh snapshot from
//! the supervisor's control socket on expand via `{"cmd":"Status"}` — R5
//! explicitly calls this out ("on-demand connect + `{"cmd":"Status"}` for
//! full snapshot"). Every helper here is pure host Rust so the screen is
//! exhaustively host-testable; only the wasm `render`/`update` code in
//! [`crate`] wires the strings into zellij-tile's drawing API.
//!
//! # Acceptance criteria mapping (`cavekit-plugin-picker.md` R5)
//!
//! - Nested expand-tree layout under the selected row: [`build_detail_rows`].
//! - Fields rendered: session / cwd-home-rel / orch / engine / phase / iter /
//!   started / last / review / last-event — all assembled by
//!   [`build_detail_rows`].
//! - Hand-rolled humantime: [`format_humantime`] (same formula as T-102's
//!   `format_age`).
//! - Home-relative cwd: [`home_rel`].
//! - Left / Tab / Esc collapse back to list: [`handle_detail_key`].
//! - On-demand connect to per-agent socket + `{"cmd":"Status"}`:
//!   [`query_agent_status`].
//!
//! # Banned crate reminder
//!
//! R1 bans `humantime`, `chrono`, and `serde_json`. The snapshot response is
//! parsed with the hand-rolled JSON helpers in [`crate::bootstrap`] so every
//! parsing byte stays under our control.

use std::path::Path;

use crate::bootstrap::{find_object_field, find_string_field, find_u64_field};
use crate::render_list::{KeyInput, PickerAction};
use crate::state::{DetailSnapshot, DetailState};

/// Failure modes for [`query_agent_status`].
///
/// Split so the detail screen can render a pointed error message ("agent
/// unreachable" vs "snapshot parse failed") and tests can assert on each
/// branch without stringly-typed matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailError {
    /// Connect/IO failure — the supervisor socket is gone or wedged.
    Unreachable,
    /// Supervisor answered but the JSON didn't match the expected shape.
    ParseError,
}

impl DetailError {
    /// Human-readable rendering for the detail-screen inline error row.
    pub fn message(&self) -> &'static str {
        match self {
            DetailError::Unreachable => "agent unreachable — press ← to collapse",
            DetailError::ParseError => "snapshot parse error — press ← to collapse",
        }
    }
}

/// Short read/write timeout for the on-demand Status probe.
///
/// Kept at 500ms per the task spec — long enough for a healthy supervisor
/// to reply, short enough that a wedged socket doesn't stall the UI.
#[cfg(unix)]
const SOCKET_TIMEOUT_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Replace a `$HOME` prefix with `~`. Returns `path` unchanged if the prefix
/// doesn't match or `home` is empty.
///
/// The check is an exact string-prefix test — fine for the picker's scope
/// (we compare against the literal `$HOME` the supervisor captured at spawn
/// time). A real canonical-path check would require filesystem access we
/// don't have from the wasm plugin.
pub fn home_rel(path: &str, home: &str) -> String {
    if home.is_empty() || !path.starts_with(home) {
        return path.to_string();
    }
    let tail = &path[home.len()..];
    if tail.is_empty() {
        return "~".to_string();
    }
    if tail.starts_with('/') {
        format!("~{tail}")
    } else {
        format!("~/{tail}")
    }
}

/// Hand-rolled humantime — "Ns ago", "Nm ago", "Nh ago", "Nd ago".
///
/// Mirrors T-102's `format_age` so both screens agree on units. `now_ms <
/// then_ms` (clock skew) saturates to `"0s ago"`.
pub fn format_humantime(now_ms: u64, then_ms: u64) -> String {
    let delta_ms = now_ms.saturating_sub(then_ms);
    let secs = delta_ms / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Render an epoch-seconds timestamp humantime-style, or "—" when absent.
fn fmt_opt_time(ts: Option<u64>, now_ms: u64) -> String {
    match ts {
        Some(s) => format_humantime(now_ms, s.saturating_mul(1000)),
        None => "—".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Detail row builder
// ---------------------------------------------------------------------------

/// Indent applied to every detail row — "4 cols" per the task spec — plus
/// the tree-char `└─ ` prefix for the first row. Follow-on rows align to
/// the same column so the block reads as a sub-tree.
pub const DETAIL_INDENT: &str = "    ";
/// Tree glyph prepended to the first visible row of the expand-tree.
pub const DETAIL_TREE: &str = "└─ ";
/// Continuation indent (aligned to the body of the first row, past `└─ `).
pub const DETAIL_CONT: &str = "       ";

/// Construct the indented rows the detail screen draws under the selected
/// list row.
///
/// Returns a `Vec<String>` ready for `Text::new(&row)` / `print_text_with_
/// coordinates` — the wasm side owns the row-y loop.
///
/// - Loading (`snapshot == None && error == None`): one-liner "fetching…".
/// - Error (`error.is_some()`): warn glyph + message.
/// - Success: the nine-ish field rows described by R5.
pub fn build_detail_rows(state: &DetailState, home: &str, now_ms: u64) -> Vec<String> {
    if let Some(err) = state.error.as_deref() {
        return vec![format!("{DETAIL_INDENT}{DETAIL_TREE}⚠ {err}")];
    }
    let Some(snap) = state.snapshot.as_ref() else {
        return vec![format!("{DETAIL_INDENT}{DETAIL_TREE}fetching…")];
    };

    let mut out = Vec::with_capacity(8);
    // First row uses the tree glyph; the rest align via `DETAIL_CONT` so
    // the tree stays anchored visually on row 0.
    let first = |body: String| format!("{DETAIL_INDENT}{DETAIL_TREE}{body}");
    let cont = |body: String| format!("{DETAIL_INDENT}{DETAIL_CONT}{body}");

    out.push(first(format!("session: {}", snap.session)));
    out.push(cont(format!("cwd: {}", home_rel(&snap.cwd, home))));
    out.push(cont(format!(
        "orch: {}  engine: {}",
        snap.orchestrator, snap.engine
    )));
    let iter_txt = match snap.iter {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    };
    out.push(cont(format!("phase: {}  iter: {}", snap.phase, iter_txt)));
    out.push(cont(format!(
        "started: {}",
        fmt_opt_time(snap.started_at, now_ms)
    )));

    let last_txt = match &snap.last_event {
        Some(msg) if !msg.is_empty() => format!(
            "last event: {} — {}",
            fmt_opt_time(snap.last_event_at, now_ms),
            msg
        ),
        _ => format!("last event: {}", fmt_opt_time(snap.last_event_at, now_ms)),
    };
    out.push(cont(last_txt));

    if snap.phase == "reviewing" || snap.phase == "Reviewing" || snap.last_review_at.is_some() {
        out.push(cont(format!(
            "last review: {}",
            fmt_opt_time(snap.last_review_at, now_ms)
        )));
    }
    out
}

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

/// Pure key handler for the detail screen.
///
/// - `Left` / Tab (mapped as `Other` — we do not have a dedicated Tab variant
///   yet, so the caller should map Tab + Backspace-like navigation to the
///   explicit `CollapseDetail` action) / `Esc` → collapse back to the list.
/// - `Enter` → open the agent's session (same as the list screen).
/// - `Delete` → open the kill modal.
/// - Everything else → no-op (snapshot fetch happens elsewhere).
pub fn handle_detail_key(state: &mut DetailState, key: KeyInput) -> PickerAction {
    match key {
        KeyInput::Esc | KeyInput::Left | KeyInput::Tab | KeyInput::Backspace => {
            PickerAction::CollapseDetail
        }
        KeyInput::Enter => PickerAction::OpenSession(state.agent_id.clone()),
        KeyInput::Delete => PickerAction::ConfirmKill(state.agent_id.clone()),
        _ => PickerAction::None,
    }
}

// ---------------------------------------------------------------------------
// Socket query
// ---------------------------------------------------------------------------

/// Query a single supervisor's Status over its UnixStream socket and parse
/// the response into a [`DetailSnapshot`].
///
/// Semantics (R5 + T-066):
/// - Connect to `sock_path` with a [`SOCKET_TIMEOUT_MS`]ms read/write deadline.
/// - Write `{"cmd":"Status"}\n` and read one newline-terminated JSON line.
/// - Parse with the hand-rolled extractors so R1's serde_json ban stays
///   intact.
///
/// Errors collapse into [`DetailError::Unreachable`] (connect/IO) or
/// [`DetailError::ParseError`] (supervisor answered but shape didn't match).
#[cfg(unix)]
pub fn query_agent_status(sock_path: &Path) -> Result<DetailSnapshot, DetailError> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(sock_path).map_err(|_| DetailError::Unreachable)?;
    let to = Duration::from_millis(SOCKET_TIMEOUT_MS);
    stream
        .set_read_timeout(Some(to))
        .map_err(|_| DetailError::Unreachable)?;
    stream
        .set_write_timeout(Some(to))
        .map_err(|_| DetailError::Unreachable)?;
    stream
        .write_all(b"{\"cmd\":\"Status\"}\n")
        .map_err(|_| DetailError::Unreachable)?;
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|_| DetailError::Unreachable)?;
    parse_status_response(&line).ok_or(DetailError::ParseError)
}

/// Non-unix stub so the crate still compiles on wasm32 (no UnixStream).
/// The wasm caller never invokes this — socket I/O is host-side only — but
/// keeping the symbol present means lib.rs can `use` the module under any
/// target without conditional-compilation gymnastics.
#[cfg(not(unix))]
pub fn query_agent_status(_sock_path: &Path) -> Result<DetailSnapshot, DetailError> {
    Err(DetailError::Unreachable)
}

/// Parse a supervisor Status response line (`{"ok":true,"data":{...}}\n`)
/// into a [`DetailSnapshot`]. Exposed separately so tests can exercise the
/// extractor without a real UnixListener.
///
/// Returns `None` on any of:
/// - Response is not an `ok:true` envelope.
/// - `data` object is missing or malformed.
/// - Neither `spec.session` nor `phase` can be found (we require at least a
///   skeleton to render).
pub fn parse_status_response(line: &str) -> Option<DetailSnapshot> {
    let trimmed = line.trim();
    // Envelope check: require `"ok":true` — any false envelope is treated
    // as a parse error upstream.
    if !trimmed.contains("\"ok\":true") && !trimmed.contains("\"ok\": true") {
        return None;
    }
    let data = find_object_field(trimmed, "data")?;
    let spec = find_object_field(data, "spec").unwrap_or("");

    let session = find_string_field(spec, "session").unwrap_or_default();
    let cwd = find_string_field(spec, "cwd").unwrap_or_default();
    let orchestrator = find_string_field(spec, "orchestrator").unwrap_or_default();
    let engine = find_string_field(spec, "engine").unwrap_or_default();

    let phase = find_string_field(data, "phase").unwrap_or_default();
    let iter = find_u64_field(data, "iter").map(|v| v as u32);
    let started_at = find_u64_field(data, "started_at");
    let last_event_at = find_u64_field(data, "last_event_at");
    let last_review_at = find_u64_field(data, "last_review_at");
    let last_event = find_string_field(data, "last_event_summary")
        .or_else(|| find_string_field(data, "last_event"));

    if phase.is_empty() && session.is_empty() {
        return None;
    }

    Some(DetailSnapshot {
        session,
        cwd,
        orchestrator,
        engine,
        phase,
        iter,
        started_at,
        last_event_at,
        last_review_at,
        last_event,
    })
}

// ---------------------------------------------------------------------------
// Tests — host-side only.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- home_rel -----------------------------------------------------------

    #[test]
    fn home_rel_replaces_prefix() {
        let got = home_rel("/Users/rjm/src/ark", "/Users/rjm");
        assert_eq!(got, "~/src/ark");
    }

    #[test]
    fn home_rel_non_matching_prefix_unchanged() {
        let got = home_rel("/etc/hostname", "/Users/rjm");
        assert_eq!(got, "/etc/hostname");
    }

    #[test]
    fn home_rel_empty_home_unchanged() {
        let got = home_rel("/Users/rjm/x", "");
        assert_eq!(got, "/Users/rjm/x");
    }

    #[test]
    fn home_rel_exact_home_is_tilde() {
        let got = home_rel("/Users/rjm", "/Users/rjm");
        assert_eq!(got, "~");
    }

    // --- format_humantime ---------------------------------------------------

    #[test]
    fn humantime_seconds_and_minutes() {
        assert_eq!(format_humantime(5_000, 0), "5s ago");
        assert_eq!(format_humantime(180_000, 0), "3m ago");
    }

    #[test]
    fn humantime_hours_days() {
        assert_eq!(format_humantime(3_600_000, 0), "1h ago");
        assert_eq!(format_humantime(2 * 86_400_000, 0), "2d ago");
    }

    #[test]
    fn humantime_clock_skew_zero() {
        assert_eq!(format_humantime(0, 9_999), "0s ago");
    }

    // --- handle_detail_key --------------------------------------------------

    fn st(id: &str) -> DetailState {
        DetailState {
            agent_id: id.into(),
            snapshot: None,
            error: None,
        }
    }

    #[test]
    fn key_esc_collapses() {
        let mut s = st("a");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Esc),
            PickerAction::CollapseDetail
        );
    }

    #[test]
    fn key_left_collapses() {
        let mut s = st("a");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Left),
            PickerAction::CollapseDetail
        );
    }

    #[test]
    fn key_tab_collapses() {
        let mut s = st("a");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Tab),
            PickerAction::CollapseDetail
        );
    }

    #[test]
    fn key_enter_opens_session() {
        let mut s = st("abc");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Enter),
            PickerAction::OpenSession("abc".to_string())
        );
    }

    #[test]
    fn key_delete_confirms_kill() {
        let mut s = st("abc");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Delete),
            PickerAction::ConfirmKill("abc".to_string())
        );
    }

    #[test]
    fn key_other_is_noop() {
        let mut s = st("a");
        assert_eq!(
            handle_detail_key(&mut s, KeyInput::Other),
            PickerAction::None
        );
    }

    // --- parse_status_response ---------------------------------------------

    #[test]
    fn parse_status_extracts_expected_fields() {
        let line = r#"{"ok":true,"data":{"spec":{"session":"cavekit-auth","cwd":"/Users/rjm/src/ark","orchestrator":"cavekit","engine":"claude-code"},"phase":"running","iter":3,"started_at":1000,"last_event_at":1500,"last_review_at":1200,"last_event_summary":"tick"}}"#;
        let snap = parse_status_response(line).expect("parse");
        assert_eq!(snap.session, "cavekit-auth");
        assert_eq!(snap.cwd, "/Users/rjm/src/ark");
        assert_eq!(snap.orchestrator, "cavekit");
        assert_eq!(snap.engine, "claude-code");
        assert_eq!(snap.phase, "running");
        assert_eq!(snap.iter, Some(3));
        assert_eq!(snap.started_at, Some(1000));
        assert_eq!(snap.last_event_at, Some(1500));
        assert_eq!(snap.last_review_at, Some(1200));
        assert_eq!(snap.last_event.as_deref(), Some("tick"));
    }

    #[test]
    fn parse_status_rejects_ok_false() {
        let line = r#"{"ok":false,"error":"boom"}"#;
        assert!(parse_status_response(line).is_none());
    }

    #[test]
    fn parse_status_rejects_garbage() {
        assert!(parse_status_response("not json").is_none());
    }

    // --- build_detail_rows --------------------------------------------------

    fn sample_snap() -> DetailSnapshot {
        DetailSnapshot {
            session: "cavekit-auth".into(),
            cwd: "/Users/rjm/src/ark".into(),
            orchestrator: "cavekit".into(),
            engine: "claude-code".into(),
            phase: "running".into(),
            iter: Some(3),
            started_at: Some(0),
            last_event_at: Some(0),
            last_review_at: None,
            last_event: Some("tick".into()),
        }
    }

    #[test]
    fn build_rows_loading_when_no_snapshot() {
        let s = DetailState {
            agent_id: "a".into(),
            ..Default::default()
        };
        let rows = build_detail_rows(&s, "/Users/rjm", 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("fetching"));
    }

    #[test]
    fn build_rows_error_when_error_set() {
        let s = DetailState {
            agent_id: "a".into(),
            snapshot: None,
            error: Some("agent unreachable — press ← to collapse".into()),
        };
        let rows = build_detail_rows(&s, "/Users/rjm", 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("⚠"));
        assert!(rows[0].contains("unreachable"));
    }

    #[test]
    fn build_rows_success_has_session_and_cwd() {
        let s = DetailState {
            agent_id: "a".into(),
            snapshot: Some(sample_snap()),
            error: None,
        };
        let rows = build_detail_rows(&s, "/Users/rjm", 60_000);
        // Pick through for known fragments without being brittle about
        // exact row counts — R5 requires the fields, not their ordering.
        let joined = rows.join("\n");
        assert!(joined.contains("session: cavekit-auth"));
        assert!(joined.contains("cwd: ~/src/ark"));
        assert!(joined.contains("orch: cavekit"));
        assert!(joined.contains("engine: claude-code"));
        assert!(joined.contains("phase: running"));
        assert!(joined.contains("iter: 3"));
        assert!(joined.contains("started:"));
        assert!(joined.contains("last event:"));
        assert!(joined.contains("tick"));
    }

    #[test]
    fn build_rows_includes_review_when_present() {
        let mut snap = sample_snap();
        snap.phase = "reviewing".into();
        snap.last_review_at = Some(0);
        let s = DetailState {
            agent_id: "a".into(),
            snapshot: Some(snap),
            error: None,
        };
        let rows = build_detail_rows(&s, "/Users/rjm", 60_000);
        let joined = rows.join("\n");
        assert!(joined.contains("last review:"));
    }

    // --- query_agent_status over a real UnixListener -----------------------

    #[cfg(unix)]
    #[test]
    fn query_returns_unreachable_for_missing_socket() {
        use std::path::PathBuf;
        let missing = PathBuf::from("/tmp/ark-picker-test-does-not-exist.sock");
        let err = query_agent_status(&missing).unwrap_err();
        assert_eq!(err, DetailError::Unreachable);
    }

    #[cfg(unix)]
    #[test]
    fn query_parses_valid_response() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::thread;

        let tmp =
            std::env::temp_dir().join(format!("ark-picker-detail-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let listener = UnixListener::bind(&tmp).expect("bind");

        let server_sock = tmp.clone();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req = String::new();
            let _ = reader.read_line(&mut req);
            let mut w = stream;
            let body = r#"{"ok":true,"data":{"spec":{"session":"cavekit-auth","cwd":"/home/x","orchestrator":"cavekit","engine":"claude-code"},"phase":"running","iter":7,"started_at":10,"last_event_at":20,"last_event_summary":"ok"}}"#;
            w.write_all(body.as_bytes()).unwrap();
            w.write_all(b"\n").unwrap();
            let _ = server_sock;
        });

        let snap = query_agent_status(&tmp).expect("snap");
        handle.join().ok();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(snap.session, "cavekit-auth");
        assert_eq!(snap.phase, "running");
        assert_eq!(snap.iter, Some(7));
        assert_eq!(snap.last_event.as_deref(), Some("ok"));
    }

    #[cfg(unix)]
    #[test]
    fn query_returns_parse_error_on_garbage() {
        use std::io::Write;
        use std::os::unix::net::UnixListener;
        use std::thread;

        let tmp =
            std::env::temp_dir().join(format!("ark-picker-detail-bad-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let listener = UnixListener::bind(&tmp).expect("bind");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream.write_all(b"not a json envelope\n").unwrap();
        });

        let err = query_agent_status(&tmp).unwrap_err();
        handle.join().ok();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(err, DetailError::ParseError);
    }
}
