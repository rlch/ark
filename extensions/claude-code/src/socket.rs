//! Per-session cc-hook NDJSON socket reader (T-011).
//!
//! Binds a unix socket at `$STATE/sessions/<sid>/cc-hook.sock` during
//! `on_session_start` and accepts one connection per cc-hook invocation.
//! For each accepted connection, the reader consumes NDJSON lines with
//! `tokio::io::BufReader::read_line`, decodes each into a
//! [`NdjsonLine`], maps `kind` → [`HookEvent`], and invokes a caller-
//! supplied sink with the translated ExtEvent. Malformed lines and
//! unknown kinds log a `tracing::warn!` and are skipped — never crash
//! the loop (R2 fail-open semantics on the ark side too).
//!
//! The reader also surfaces the R4 handshake metadata: if a line
//! carries `bridge_version` and it differs from the running crate's
//! `CRATE_VERSION`, the sink sees a [`SocketEvent::BridgeVersionMismatch`]
//! event. The first mismatch per session is written to an on-disk
//! sentinel at `$STATE/sessions/<sid>/claude-code.bridge_version_mismatch.json`
//! so `ark doctor` (T-042) can surface it without a live API into the
//! supervisor's `ext_state` map — see T-012.
//!
//! Deliberate shape: minimal surface (one bind, one accept loop, one
//! sink callback). No connection pooling, no persistent reverse
//! channel; cc-hook is a fire-and-forget binary that reconnects on
//! every hook fire.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, trace, warn};

use ark_types::{ExtEvent, SessionId, StateLayout};

use crate::hook_event::HookEvent;
use crate::hook_payload::{NdjsonLine, payload_to_ext_event};

/// Compile-time crate version used to validate `bridge_version` in R4
/// handshake frames. Tied to `ark-ext-claude-code`'s `Cargo.toml`
/// `version` field via `env!` so a crate bump automatically rolls the
/// expected handshake value without a second edit.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Basename of the on-disk bridge-version-mismatch sentinel (T-012).
/// Lives under `$STATE/sessions/<sid>/` alongside `cc-hook.sock` and
/// `cc-hook.handshake` so doctor checks only have to look in one dir.
pub const BRIDGE_VERSION_MISMATCH_SENTINEL: &str = "claude-code.bridge_version_mismatch.json";

/// Per-frame readout delivered to the sink by [`CcHookSocket::accept_loop`].
///
/// The socket reader is the single place that both translates NDJSON
/// into ExtEvents AND surfaces handshake metadata; keeping the two
/// concerns on one enum lets the caller stay a single closure rather
/// than juggling parallel channels.
#[derive(Debug, Clone)]
pub enum SocketEvent {
    /// A well-formed cc-hook POST. `event` is the decoded [`HookEvent`]
    /// (from the NDJSON `kind` field); `ext_event` is the translated
    /// `claude-code.<kind>` ExtEvent ready for the core bus.
    HookFired {
        /// Typed hook event — parsed from the NDJSON `kind` field.
        event: HookEvent,
        /// R3 translated ExtEvent ready to publish on the core bus.
        ext_event: ExtEvent,
    },
    /// R4 handshake mismatch detected on the first POST carrying a
    /// `bridge_version`. Fired at most once per [`CcHookSocket`]
    /// instance — subsequent mismatching frames are silently skipped
    /// (one-shot doctor warning per session per kit R4).
    BridgeVersionMismatch {
        /// Version string advertised by cc-hook.
        observed: String,
        /// Version string expected by the running ark-side crate
        /// ([`CRATE_VERSION`]).
        expected: String,
    },
}

/// On-disk payload for the bridge-version-mismatch sentinel. Written
/// once per session by [`CcHookSocket::record_mismatch_sentinel`] the
/// first time a mismatch is observed; `ark doctor` (T-042) reads this
/// file directly to surface the warning.
///
/// Kept deliberately flat so doctor can parse with `serde_json::from_slice`
/// without pulling in this crate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeVersionMismatch {
    /// Version cc-hook advertised in the R4 handshake frame.
    pub observed: String,
    /// Version ark's `ark-ext-claude-code` crate was built with.
    pub expected: String,
    /// RFC 3339 timestamp at which the mismatch was first recorded.
    pub first_seen_at: String,
}

/// Handle to a bound per-session cc-hook socket.
///
/// Construct via [`CcHookSocket::bind`]; drive the accept loop via
/// [`CcHookSocket::accept_loop`]. The socket file is NOT unlinked on
/// drop — the ark-side session directory is 0700 and the supervisor
/// cleans it up at session end.
#[derive(Debug)]
pub struct CcHookSocket {
    listener: UnixListener,
    path: PathBuf,
    session_dir: PathBuf,
}

/// Errors returned by [`CcHookSocket::bind`]. All IO failures collapse
/// to `BindFailed` so callers (the `on_session_start` hook) can decide
/// whether to log + continue (current v0.1 strategy) or promote to a
/// startup-blocking error (future policy if the socket reader becomes
/// load-bearing for user-visible state).
#[derive(Debug, thiserror::Error)]
pub enum CcHookSocketError {
    /// Could not bind the unix socket. Wrapped error carries the OS
    /// reason (stale socket file, permission denied, missing parent
    /// dir, etc.).
    #[error("bind {path}: {source}")]
    BindFailed {
        /// Path we tried to bind at.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

impl CcHookSocket {
    /// Bind the per-session socket at
    /// `$STATE/sessions/<sid>/cc-hook.sock`, unlinking any stale file
    /// left by a previous cc-hook process that crashed without cleanup.
    ///
    /// Creates the parent `session_dir` with mode 0700 via
    /// [`StateLayout::ensure_dir_0700`] before binding, so the
    /// extension can call this from `on_session_start` without assuming
    /// ark's supervisor has already provisioned the directory.
    pub async fn bind(
        layout: &StateLayout,
        session_id: &SessionId,
    ) -> Result<Self, CcHookSocketError> {
        let session_dir = layout.session_dir(session_id);
        // Best-effort mkdir — failure here is almost always a
        // permissions problem that will also fail the bind, so we let
        // the bind surface the better error below.
        let _ = StateLayout::ensure_dir_0700(&session_dir);

        let path = session_dir.join("cc-hook.sock");

        // Unlink any stale socket. We ignore NotFound; anything else
        // gets surfaced via BindFailed because a still-active socket
        // from a live cc-hook reader elsewhere would confuse us.
        if let Err(e) = tokio::fs::remove_file(&path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "cc-hook socket: stale socket unlink failed; continuing into bind"
                );
            }
        }

        let listener =
            UnixListener::bind(&path).map_err(|source| CcHookSocketError::BindFailed {
                path: path.clone(),
                source,
            })?;

        debug!(
            path = %path.display(),
            "cc-hook socket: bound per-session listener"
        );

        Ok(Self {
            listener,
            path,
            session_dir,
        })
    }

    /// Absolute path the listener is bound to. Useful for the
    /// `CLAUDE_HOOK_SOCKET` env var wiring (kit R5) and for test
    /// harnesses.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Absolute path of the session state directory the socket lives
    /// in. Mismatch sentinel + handshake sentinel both land here.
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Absolute path of the bridge-version-mismatch sentinel (T-012).
    /// Lives under the same session directory as the socket so
    /// `ark doctor` only has to consult one dir.
    pub fn mismatch_sentinel_path(&self) -> PathBuf {
        self.session_dir.join(BRIDGE_VERSION_MISMATCH_SENTINEL)
    }

    /// Accept-loop driver. Consumes `self` and runs until cancelled
    /// (the task handle is dropped) or the listener errors in a way
    /// that can't recover (currently: never — every branch logs and
    /// continues).
    ///
    /// `sink` is invoked for each decoded event. We deliberately keep
    /// the sink `FnMut` + `Send` rather than async so callers that
    /// want to forward into a tokio channel can do so with a simple
    /// `tx.send(...)` closure; async sinks add complexity the kit
    /// doesn't require.
    ///
    /// The reader is robust against:
    ///  * Malformed NDJSON → `tracing::warn!` + skip.
    ///  * Unknown `kind` values → `tracing::warn!` + skip.
    ///  * Partial writes (client died mid-line) → `read_line` returns
    ///    whatever is available + closed-connection signal; we drop
    ///    the partial line on the floor.
    ///  * Per-connection errors → logged + connection dropped; loop
    ///    continues accepting.
    ///
    /// Returns when the listener closes (in practice, never during
    /// normal operation — the caller aborts the task on session end).
    pub async fn accept_loop<F>(self, mut sink: F)
    where
        F: FnMut(SocketEvent) + Send,
    {
        // One-shot guard for R4 mismatch — kit says "one-shot doctor
        // warning per session", so after the first mismatch we
        // suppress subsequent ones even if cc-hook somehow re-sends
        // the handshake.
        let mut mismatch_recorded = false;

        loop {
            let (stream, _addr) = match self.listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!(
                        path = %self.path.display(),
                        error = %e,
                        "cc-hook socket: accept failed; continuing"
                    );
                    continue;
                }
            };

            trace!(
                path = %self.path.display(),
                "cc-hook socket: accepted connection"
            );
            Self::handle_connection(stream, &mut sink, &mut mismatch_recorded, &self.session_dir)
                .await;
        }
    }

    /// Handle a single cc-hook connection — read NDJSON line-at-a-time
    /// until EOF, decode, forward to sink.
    ///
    /// Extracted from [`Self::accept_loop`] so tests can drive it with
    /// an in-process `tokio::net::UnixStream` pair without binding a
    /// real listener.
    async fn handle_connection<F>(
        stream: UnixStream,
        sink: &mut F,
        mismatch_recorded: &mut bool,
        session_dir: &Path,
    ) where
        F: FnMut(SocketEvent) + Send,
    {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => return, // EOF; client closed cleanly
                Ok(_) => {
                    // read_line keeps the trailing `\n`; strip to make
                    // serde happy (serde tolerates trailing whitespace
                    // but trimming is explicit).
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    Self::dispatch_line(trimmed, sink, mismatch_recorded, session_dir);
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        "cc-hook socket: read_line failed; dropping connection"
                    );
                    return;
                }
            }
        }
    }

    /// Decode + dispatch a single trimmed NDJSON line.
    fn dispatch_line<F>(raw: &str, sink: &mut F, mismatch_recorded: &mut bool, session_dir: &Path)
    where
        F: FnMut(SocketEvent) + Send,
    {
        let line: NdjsonLine = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    kind = "malformed_ndjson",
                    raw_len = raw.len(),
                    error = %e,
                    "cc-hook socket: line ignored"
                );
                return;
            }
        };

        // R4 handshake: if this frame carries a bridge_version, compare
        // to the running crate version. One-shot per session.
        if let Some(observed) = line.bridge_version.as_deref() {
            if observed != CRATE_VERSION && !*mismatch_recorded {
                warn!(
                    observed = %observed,
                    expected = %CRATE_VERSION,
                    "cc-hook socket: bridge version mismatch — run `ark ext claude-code reinstall-hook-binary`"
                );
                if let Err(e) = record_mismatch_sentinel(session_dir, observed, CRATE_VERSION) {
                    warn!(
                        error = %e,
                        session_dir = %session_dir.display(),
                        "cc-hook socket: failed to persist bridge version mismatch sentinel"
                    );
                }
                sink(SocketEvent::BridgeVersionMismatch {
                    observed: observed.to_string(),
                    expected: CRATE_VERSION.to_string(),
                });
                *mismatch_recorded = true;
            }
        }

        // Map `kind` → HookEvent. Unknown kinds are skipped with a warn
        // rather than crashing the loop; a future hook event Claude
        // Code adds before we upgrade the enum would otherwise wedge
        // the session.
        let event: HookEvent = match line.kind.parse() {
            Ok(e) => e,
            Err(e) => {
                warn!(
                    kind = %line.kind,
                    error = %e,
                    "cc-hook socket: unknown HookEvent kind; line ignored"
                );
                return;
            }
        };

        let ext_event = payload_to_ext_event(&line.payload, event);
        sink(SocketEvent::HookFired { event, ext_event });
    }
}

/// Write the bridge-version-mismatch sentinel atomically via
/// tmp-file + rename (matches the pattern used by ark-core's
/// `write_session_status_atomic`). Exposed at module scope (rather
/// than as a method) so tests can exercise it without setting up a
/// full listener.
pub fn record_mismatch_sentinel(
    session_dir: &Path,
    observed: &str,
    expected: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(session_dir)?;
    let payload = BridgeVersionMismatch {
        observed: observed.to_string(),
        expected: expected.to_string(),
        first_seen_at: chrono::Utc::now().to_rfc3339(),
    };
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let final_path = session_dir.join(BRIDGE_VERSION_MISMATCH_SENTINEL);
    let tmp_path = {
        let mut p = final_path.clone();
        let mut name = p
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from(BRIDGE_VERSION_MISMATCH_SENTINEL));
        name.push(".tmp");
        p.set_file_name(name);
        p
    };

    // Best-effort clean-up of a stale tmp from a crashed writer — if
    // it's there we overwrite rather than fail.
    let _ = std::fs::remove_file(&tmp_path);

    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    use crate::hook_payload::HookPayload;

    fn tmp_layout(td: &TempDir) -> StateLayout {
        // Put state, runtime, and config under one TempDir so the test
        // is self-contained. Paths don't need to be valid XDG dirs —
        // StateLayout is a pure path-builder.
        StateLayout::new(
            td.path().join("state"),
            td.path().join("rt"),
            td.path().join("cfg"),
        )
    }

    /// Build a TempDir under `/tmp` (not the default `$TMPDIR` which on
    /// macOS resolves to `/var/folders/...` and can push the socket
    /// path over macOS's `SUN_LEN` (104 bytes) once the 26-char ulid
    /// session-dir hash is appended. `/tmp` is a symlink to
    /// `/private/tmp` on macOS but is short enough to fit. Linux
    /// already gives us a short path via `/tmp`.
    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("arkcc-")
            .tempdir_in("/tmp")
            .expect("tempdir in /tmp")
    }

    /// Override `SessionId` tests expose — ulid path leaves are ~32
    /// chars which, added to the session_dir structure, blow past
    /// `SUN_LEN` even with `/tmp`. Tests that only exercise socket
    /// bind/accept use a shorter stand-in session id.
    fn short_session_id() -> SessionId {
        SessionId::new("s")
    }

    fn base_payload(event_name: &str) -> HookPayload {
        HookPayload {
            session_id: "sess-1".into(),
            cwd: PathBuf::from("/tmp"),
            hook_event_name: event_name.into(),
            tool_name: None,
            tool_input: None,
            extra: Default::default(),
        }
    }

    fn ndjson(kind: &str, bv: Option<&str>) -> String {
        let line = NdjsonLine {
            kind: kind.to_string(),
            session_id: "sess-1".into(),
            payload: base_payload(kind),
            emitted_at: "2026-04-18T00:00:00Z".into(),
            bridge_version: bv.map(|s| s.to_string()),
        };
        serde_json::to_string(&line).unwrap()
    }

    #[tokio::test]
    async fn bind_creates_socket_file_under_session_dir() {
        let td = short_tempdir();
        let layout = tmp_layout(&td);
        let sid = short_session_id();
        let sock = CcHookSocket::bind(&layout, &sid).await.expect("bind");
        assert!(sock.path().exists(), "socket file not created");
        assert_eq!(sock.path().file_name().unwrap(), "cc-hook.sock");
    }

    #[tokio::test]
    async fn bind_replaces_stale_socket_file() {
        let td = short_tempdir();
        let layout = tmp_layout(&td);
        let sid = short_session_id();

        // Prep a stale socket-file that is NOT a bound listener.
        let session_dir = layout.session_dir(&sid);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(session_dir.join("cc-hook.sock"), b"stale").unwrap();

        // Bind must unlink the stale file and succeed.
        let sock = CcHookSocket::bind(&layout, &sid).await.expect("rebind");
        assert!(sock.path().exists());
    }

    #[tokio::test]
    async fn dispatch_line_forwards_valid_frame() {
        let td = TempDir::new().unwrap();
        let session_dir = td.path().join("sess");
        std::fs::create_dir_all(&session_dir).unwrap();

        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = |ev: SocketEvent| {
            events_tx.send(ev).unwrap();
        };
        let mut recorded = false;
        let line = ndjson("PostToolUse", None);

        CcHookSocket::dispatch_line(&line, &mut sink, &mut recorded, &session_dir);
        let ev = events_rx.try_recv().expect("event");
        match ev {
            SocketEvent::HookFired { event, ext_event } => {
                assert_eq!(event, HookEvent::PostToolUse);
                assert_eq!(ext_event.ext, "claude-code");
                assert_eq!(ext_event.kind, "post-tool-use");
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(!recorded, "no mismatch should be recorded yet");
    }

    #[tokio::test]
    async fn dispatch_line_skips_malformed_ndjson() {
        let td = TempDir::new().unwrap();
        let session_dir = td.path().join("sess");
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = |ev: SocketEvent| {
            events_tx.send(ev).unwrap();
        };
        let mut recorded = false;

        CcHookSocket::dispatch_line("{not-json", &mut sink, &mut recorded, &session_dir);
        CcHookSocket::dispatch_line(r#"{"no":"fields"}"#, &mut sink, &mut recorded, &session_dir);
        assert!(events_rx.try_recv().is_err(), "no events for malformed");
    }

    #[tokio::test]
    async fn dispatch_line_skips_unknown_kind() {
        let td = TempDir::new().unwrap();
        let session_dir = td.path().join("sess");
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = |ev: SocketEvent| {
            events_tx.send(ev).unwrap();
        };
        let mut recorded = false;

        // Valid NDJSON but kind isn't in the HookEvent enum.
        let line = ndjson("TotallyNewHookEvent", None);
        CcHookSocket::dispatch_line(&line, &mut sink, &mut recorded, &session_dir);
        assert!(events_rx.try_recv().is_err(), "no events for unknown kind");
    }

    #[tokio::test]
    async fn dispatch_line_records_bridge_version_mismatch_once() {
        let td = TempDir::new().unwrap();
        let session_dir = td.path().join("sess");
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = |ev: SocketEvent| {
            events_tx.send(ev).unwrap();
        };
        let mut recorded = false;

        let bad = ndjson("SessionStart", Some("9.9.9-bogus"));
        CcHookSocket::dispatch_line(&bad, &mut sink, &mut recorded, &session_dir);
        // Expect two events: the mismatch AND the hook frame.
        let first = events_rx.try_recv().expect("mismatch");
        assert!(matches!(first, SocketEvent::BridgeVersionMismatch { .. }));
        let second = events_rx.try_recv().expect("hook");
        assert!(matches!(second, SocketEvent::HookFired { .. }));
        assert!(recorded);

        // Sentinel file exists on disk + has expected shape.
        let sentinel = session_dir.join(BRIDGE_VERSION_MISMATCH_SENTINEL);
        let bytes = std::fs::read(&sentinel).expect("sentinel written");
        let parsed: BridgeVersionMismatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.observed, "9.9.9-bogus");
        assert_eq!(parsed.expected, CRATE_VERSION);
        assert!(!parsed.first_seen_at.is_empty());

        // Second mismatching frame: the mismatch event is suppressed
        // (one-shot per session) but the hook event still flows.
        let bad2 = ndjson("Stop", Some("also-bogus"));
        CcHookSocket::dispatch_line(&bad2, &mut sink, &mut recorded, &session_dir);
        let again = events_rx.try_recv().expect("hook again");
        assert!(matches!(again, SocketEvent::HookFired { .. }));
        assert!(events_rx.try_recv().is_err(), "no second mismatch");
    }

    #[tokio::test]
    async fn dispatch_line_matching_bridge_version_records_no_mismatch() {
        let td = TempDir::new().unwrap();
        let session_dir = td.path().join("sess");
        let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut sink = |ev: SocketEvent| {
            events_tx.send(ev).unwrap();
        };
        let mut recorded = false;

        let good = ndjson("SessionStart", Some(CRATE_VERSION));
        CcHookSocket::dispatch_line(&good, &mut sink, &mut recorded, &session_dir);
        let ev = events_rx.try_recv().expect("hook");
        assert!(matches!(ev, SocketEvent::HookFired { .. }));
        assert!(events_rx.try_recv().is_err(), "no mismatch");
        assert!(!recorded);
        // Sentinel MUST NOT be written on match.
        assert!(!session_dir.join(BRIDGE_VERSION_MISMATCH_SENTINEL).exists());
    }

    #[tokio::test]
    async fn end_to_end_client_connects_and_posts_line() {
        // Integration: bind a listener, spawn the accept loop, connect
        // from the same process, write an NDJSON line, verify the sink
        // sees the decoded event.
        let td = short_tempdir();
        let layout = tmp_layout(&td);
        let sid = short_session_id();
        let sock = CcHookSocket::bind(&layout, &sid).await.expect("bind");
        let path = sock.path().to_path_buf();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            sock.accept_loop(move |ev| {
                let _ = tx.send(ev);
            })
            .await;
        });

        // Give the listener a moment to enter accept; tokio::spawn is
        // generally instant but read-after-bind races are cheap to
        // defuse with a yield.
        tokio::task::yield_now().await;

        // Connect + write one line.
        let mut client = UnixStream::connect(&path).await.expect("connect");
        let payload = ndjson("Stop", None);
        client
            .write_all(format!("{payload}\n").as_bytes())
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        drop(client);

        let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("sink received event in time")
            .expect("channel open");
        match ev {
            SocketEvent::HookFired { event, .. } => assert_eq!(event, HookEvent::Stop),
            other => panic!("unexpected event: {other:?}"),
        }

        handle.abort();
    }

    #[test]
    fn mismatch_sentinel_round_trips() {
        let td = TempDir::new().unwrap();
        record_mismatch_sentinel(td.path(), "0.0.1", "9.9.9").expect("write");
        let bytes = std::fs::read(td.path().join(BRIDGE_VERSION_MISMATCH_SENTINEL)).unwrap();
        let parsed: BridgeVersionMismatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.observed, "0.0.1");
        assert_eq!(parsed.expected, "9.9.9");
    }
}
