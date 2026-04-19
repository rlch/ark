//! `ark bus intent --json <…>` + `ark bus emit --json <…>` — bridge from
//! the zellij-side `ark-bus` wasm plugin to the supervisor control socket
//! (scene-v3 S-B, T-070 + T-071).
//!
//! # Why this exists
//!
//! `zellij-tile`'s wasi sandbox does not expose a unix-socket API, so the
//! `ark-bus` plugin cannot talk to the supervisor directly. Instead it
//! spawns a hidden command pane running a host-side binary; that binary
//! opens `$runtime/sessions/<sid>.sock` and sends an `Intent` / `Emit`
//! NDJSON request.
//!
//! Under the v0.1 "bare ark" philosophy the host-side binary is `ark`
//! itself — the same binary the user installed and has on `PATH`. That
//! replaces the old `ark-hook` crate (deleted in Cleanup T-005) and
//! keeps the ark distribution a single executable.
//!
//! # Wire contract
//!
//! Mirrors the NDJSON envelope documented on
//! [`ark_supervisor::SupervisorCommandHandler`] — request/response are
//! single-line JSON objects, one per direction:
//!
//! ```text
//! Request:  {"cmd":"Intent","args":{"name":"<op>","args":{…}}}
//!           {"cmd":"Emit","args":{"event":"<name>","payload":{…},"source":"<tag>"}}
//! Response: {"ok":true,"data":…}
//!           {"ok":false,"error":"…"}
//! ```
//!
//! # Session-id resolution
//!
//! The plugin invocation does not know which session it belongs to, so
//! the CLI resolves it from (first hit wins):
//!
//!   1. Explicit `--session <id>` arg (full id, prefix, substring, or
//!      `spec.json` name — delegated to [`resolve_session_id`]).
//!   2. `$ARK_SESSION_ID` environment variable (full id only).
//!   3. The unique session directory under `$STATE/sessions/` — if
//!      exactly one agent exists, target it.
//!   4. Otherwise: fail with a clear "ambiguous / none" error.
//!
//! A 2-second connect timeout keeps the whole round-trip inside the
//! keybind UX budget; errors go to stderr and the process exits with
//! non-zero so `zellij`'s stderr log attributes the failure to the
//! dispatch pane.
//!
//! # Emit `source` tag
//!
//! The plugin formats a JSON envelope `{event, payload, source}` — the
//! `source` field is forwarded verbatim as the `Emit` `source` arg so
//! reactions can attribute the broadcast to `ext:ark-bus` per
//! cavekit-scene.md R4.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ark_types::{SessionId, StateLayout};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, list_session_ids, resolve_session_id};

/// 2-second connect + read/write timeout per pipe direction. Keeps the
/// whole dispatch inside the keybind UX budget (cavekit-hook-ipc R4).
const BUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Top-level args for `ark bus <verb>`.
#[derive(Debug, Args)]
#[command(
    about = "ark-bus bridge verbs (intent/emit) for the hidden command pane",
    long_about = "ark-bus bridge verbs.\n\
                  \n\
                  The zellij-side `ark-bus` wasm plugin spawns a hidden\n\
                  command pane running `ark bus intent --json <…>` or\n\
                  `ark bus emit --json <…>` to forward keybind intents\n\
                  and zellij lifecycle events to the supervisor control\n\
                  socket. Session id is resolved from --session,\n\
                  $ARK_SESSION_ID, or the unique session in state.\n\
                  \n\
                  Examples:\n  \
                  ark bus intent --json '{\"name\":\"ark.core.ping\",\"args\":{}}'\n  \
                  ark bus emit --json '{\"event\":\"ark.zellij.pane_closed\",\"payload\":{},\"source\":\"ext:ark-bus\"}'"
)]
pub struct BusArgs {
    /// Session id override (full / prefix / substring / spec.name).
    ///
    /// When omitted, the command tries `$ARK_SESSION_ID` then falls
    /// back to the unique session directory under `$STATE/sessions/`.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,

    /// The verb to run.
    #[command(subcommand)]
    pub command: BusCommand,
}

/// Verbs accepted by `ark bus`.
#[derive(Debug, Subcommand)]
pub enum BusCommand {
    /// Dispatch an intent to the supervisor for the active session.
    ///
    /// JSON payload: `{"name":"<op-name>","args":{…}}` — forwarded
    /// as-is into a control-socket `Intent` request.
    Intent {
        /// JSON intent document.
        #[arg(long, value_name = "JSON")]
        json: String,
    },
    /// Broadcast an event envelope onto the supervisor event bus.
    ///
    /// JSON envelope: `{"event":"<name>","payload":{…},"source":"<tag>"}`
    /// — forwarded as a control-socket `Emit` request.
    Emit {
        /// JSON emit envelope.
        #[arg(long, value_name = "JSON")]
        json: String,
    },
}

/// Dispatch `ark bus <verb>` (T-070/T-071).
pub fn run(args: BusArgs, ctx: &Ctx) -> Result<(), CliError> {
    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );
    let resolved = resolve_active_session(args.session.as_deref(), &layout)?;

    let request = match &args.command {
        BusCommand::Intent { json } => build_intent_request(json)?,
        BusCommand::Emit { json } => build_emit_request(json)?,
    };

    let sock = layout.session_socket_path(&resolved);
    let stream = UnixStream::connect(&sock).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            CliError::OrphanOrDead {
                reason: format!(
                    "supervisor socket for session {} is gone ({})",
                    resolved.as_str(),
                    e
                ),
            }
        }
        _ => CliError::Generic {
            reason: format!("connect supervisor socket {}: {e}", sock.display()),
        },
    })?;
    // Bound the whole exchange so a hung supervisor doesn't stall the
    // dispatch pane. Both directions get the same deadline.
    let _ = stream.set_read_timeout(Some(BUS_TIMEOUT));
    let _ = stream.set_write_timeout(Some(BUS_TIMEOUT));

    let resp = exchange(stream, &request)?;
    render_response(&resolved, &args.command, &resp)
}

/// Pick the session id to target, in priority order:
///   1. `--session <id>` explicit arg (fuzzy-resolved).
///   2. `$ARK_SESSION_ID` env var (full id only).
///   3. Unique session dir under `$STATE/sessions/`.
fn resolve_active_session(
    explicit: Option<&str>,
    layout: &StateLayout,
) -> Result<SessionId, CliError> {
    if let Some(query) = explicit {
        return resolve_session_id(query, layout).map_err(|e| map_resolve_err(e, query));
    }

    if let Ok(sid) = std::env::var("ARK_SESSION_ID") {
        let trimmed = sid.trim();
        if !trimmed.is_empty() {
            // Prefer strict parse; if the caller set a fragment
            // instead of a full id, fall back to the resolver so the
            // failure mode matches the --session path.
            return match SessionId::parse(trimmed) {
                Ok(id) if layout.session_dir(&id).is_dir() => Ok(id),
                _ => resolve_session_id(trimmed, layout).map_err(|e| map_resolve_err(e, trimmed)),
            };
        }
    }

    let ids = list_session_ids(layout).map_err(|e| CliError::Generic {
        reason: format!("enumerate sessions: {e}"),
    })?;
    match ids.len() {
        0 => Err(CliError::NotFound {
            what: "no active session (set --session or $ARK_SESSION_ID)".to_string(),
        }),
        1 => Ok(ids.into_iter().next().unwrap()),
        n => Err(CliError::Ambiguous {
            what: format!("{n} active sessions — pass --session or set $ARK_SESSION_ID"),
            candidates: ids.into_iter().map(|c| c.as_str().to_string()).collect(),
        }),
    }
}

/// Parse the `--json` string for `ark bus intent`, returning the
/// NDJSON request to send. Expects the documented
/// `{"name":"<op>","args":{…}}` shape; `args` is optional and
/// defaults to an empty object for a no-arg intent.
fn build_intent_request(raw: &str) -> Result<Value, CliError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|e| CliError::Generic {
        reason: format!("ark bus intent --json: not valid JSON: {e}"),
    })?;
    let obj = parsed.as_object().ok_or_else(|| CliError::Generic {
        reason: "ark bus intent --json: expected a JSON object with `name`".to_string(),
    })?;
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Generic {
            reason: "ark bus intent --json: missing `name` string field".to_string(),
        })?
        .to_string();
    let args_val = obj.get("args").cloned().unwrap_or_else(|| json!({}));
    Ok(json!({
        "cmd": "Intent",
        "args": {
            "name": name,
            "args": args_val,
        }
    }))
}

/// Parse the `--json` string for `ark bus emit`, returning the
/// NDJSON request. Expects `{"event":"<name>","payload":{…},"source":"<tag>"}`.
/// `payload` defaults to `{}`; `source` defaults to `"ext:ark-bus"`.
fn build_emit_request(raw: &str) -> Result<Value, CliError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|e| CliError::Generic {
        reason: format!("ark bus emit --json: not valid JSON: {e}"),
    })?;
    let obj = parsed.as_object().ok_or_else(|| CliError::Generic {
        reason: "ark bus emit --json: expected a JSON object with `event`".to_string(),
    })?;
    let event = obj
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::Generic {
            reason: "ark bus emit --json: missing `event` string field".to_string(),
        })?
        .to_string();
    let payload = obj.get("payload").cloned().unwrap_or_else(|| json!({}));
    let source = obj
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("ext:ark-bus")
        .to_string();
    Ok(json!({
        "cmd": "Emit",
        "args": {
            "event": event,
            "payload": payload,
            "source": source,
        }
    }))
}

/// Send one NDJSON line, read one NDJSON line back. Same shape as
/// `ark kill` / `ark scene reload` — avoids pulling in the tokio-based
/// `ark_core::control_socket` client (keeps the CLI blocking-only).
fn exchange(mut stream: UnixStream, request: &Value) -> Result<Value, CliError> {
    let mut line = serde_json::to_vec(request).map_err(|e| CliError::Internal {
        reason: format!("encode bus request: {e}"),
    })?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| CliError::Generic {
        reason: format!("write bus request: {e}"),
    })?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| CliError::Generic {
        reason: format!("read bus response: {e}"),
    })?;
    if buf.trim().is_empty() {
        return Err(CliError::Generic {
            reason: "empty response from supervisor".to_string(),
        });
    }
    serde_json::from_str::<Value>(buf.trim()).map_err(|e| CliError::Generic {
        reason: format!("parse bus response: {e}"),
    })
}

/// Map `Response { ok, data, error }` to a stdout/stderr report + exit
/// code. On `ok:true` we print a concise one-liner to stdout so the
/// caller (zellij command-pane log) can confirm dispatch; `ok:false`
/// bubbles as `CliError::Generic` with the embedded error message.
fn render_response(
    resolved: &SessionId,
    cmd: &BusCommand,
    response: &Value,
) -> Result<(), CliError> {
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let msg = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("supervisor returned error")
            .to_string();
        let verb = match cmd {
            BusCommand::Intent { .. } => "intent",
            BusCommand::Emit { .. } => "emit",
        };
        return Err(CliError::Generic {
            reason: format!("bus {verb} failed for {}: {msg}", resolved.as_str()),
        });
    }
    let summary = match cmd {
        BusCommand::Intent { .. } => {
            let name = response
                .pointer("/data/dispatched")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            format!("intent {name}")
        }
        BusCommand::Emit { .. } => {
            let name = response
                .pointer("/data/broadcast")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let recv = response
                .pointer("/data/receivers")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!("emit {name} ({recv} receivers)")
        }
    };
    println!("ark bus {summary} -> {}", resolved.as_str());
    Ok(())
}

/// Same shape as the mapping used by `ark kill` / `ark scene reload` —
/// keep error exit codes consistent across agent-targeting commands.
fn map_resolve_err(e: ResolveError, query: &str) -> CliError {
    match e {
        ResolveError::NotFound { .. } => CliError::NotFound {
            what: query.to_string(),
        },
        ResolveError::AmbiguousPrefix { candidates, .. }
        | ResolveError::AmbiguousSubstring { candidates, .. }
        | ResolveError::AmbiguousName { candidates, .. } => CliError::Ambiguous {
            what: query.to_string(),
            candidates: candidates
                .into_iter()
                .map(|c| c.as_str().to_string())
                .collect(),
        },
        ResolveError::Io(err) => CliError::Generic {
            reason: format!("resolve session: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: BusArgs,
    }

    // ------- clap parse surface -------

    #[test]
    fn intent_verb_parses_with_json() {
        let h = Host::try_parse_from([
            "bus",
            "intent",
            "--json",
            r#"{"name":"ark.core.ping","args":{}}"#,
        ])
        .expect("parse");
        match h.args.command {
            BusCommand::Intent { json } => {
                assert!(json.contains("ark.core.ping"));
            }
            _ => panic!("expected Intent"),
        }
    }

    #[test]
    fn emit_verb_parses_with_json() {
        let h = Host::try_parse_from([
            "bus",
            "emit",
            "--json",
            r#"{"event":"x","payload":{},"source":"ext:ark-bus"}"#,
        ])
        .expect("parse");
        match h.args.command {
            BusCommand::Emit { json } => assert!(json.contains("\"event\"")),
            _ => panic!("expected Emit"),
        }
    }

    #[test]
    fn session_flag_is_captured() {
        let h = Host::try_parse_from([
            "bus",
            "--session",
            "myfeat",
            "intent",
            "--json",
            r#"{"name":"x"}"#,
        ])
        .expect("parse");
        assert_eq!(h.args.session.as_deref(), Some("myfeat"));
    }

    #[test]
    fn intent_requires_json_flag() {
        let err = Host::try_parse_from(["bus", "intent"]).expect_err("json is required");
        assert!(err.to_string().to_lowercase().contains("json"));
    }

    // ------- build_*_request shapes -------

    #[test]
    fn build_intent_request_wraps_name_and_args() {
        let req = build_intent_request(r#"{"name":"ark.core.focus","args":{"handle":"@a"}}"#)
            .expect("ok");
        assert_eq!(req["cmd"], "Intent");
        assert_eq!(req["args"]["name"], "ark.core.focus");
        assert_eq!(req["args"]["args"]["handle"], "@a");
    }

    #[test]
    fn build_intent_request_defaults_args_to_empty_object() {
        let req = build_intent_request(r#"{"name":"ark.core.ping"}"#).expect("ok");
        assert_eq!(req["cmd"], "Intent");
        assert_eq!(req["args"]["name"], "ark.core.ping");
        assert!(req["args"]["args"].is_object());
        assert_eq!(req["args"]["args"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn build_intent_request_rejects_non_object_json() {
        let err = build_intent_request("[1,2,3]").expect_err("must error");
        assert!(matches!(err, CliError::Generic { .. }));
    }

    #[test]
    fn build_intent_request_rejects_missing_name() {
        let err = build_intent_request(r#"{"args":{}}"#).expect_err("must error");
        match err {
            CliError::Generic { reason } => assert!(reason.contains("name")),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn build_intent_request_rejects_bad_json() {
        let err = build_intent_request("not json").expect_err("must error");
        assert!(matches!(err, CliError::Generic { .. }));
    }

    #[test]
    fn build_emit_request_wraps_event_and_payload() {
        let req = build_emit_request(
            r#"{"event":"ark.zellij.pane_closed","payload":{"pane_id":7},"source":"ext:ark-bus"}"#,
        )
        .expect("ok");
        assert_eq!(req["cmd"], "Emit");
        assert_eq!(req["args"]["event"], "ark.zellij.pane_closed");
        assert_eq!(req["args"]["payload"]["pane_id"], 7);
        assert_eq!(req["args"]["source"], "ext:ark-bus");
    }

    #[test]
    fn build_emit_request_defaults_source_and_payload() {
        let req = build_emit_request(r#"{"event":"ark.zellij.foo"}"#).expect("ok");
        assert_eq!(req["args"]["event"], "ark.zellij.foo");
        assert_eq!(req["args"]["source"], "ext:ark-bus");
        assert!(req["args"]["payload"].is_object());
    }

    #[test]
    fn build_emit_request_rejects_missing_event() {
        let err = build_emit_request(r#"{"payload":{}}"#).expect_err("must error");
        match err {
            CliError::Generic { reason } => assert!(reason.contains("event")),
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    // ------- session resolution -------

    fn layout_ctx(base: PathBuf) -> Ctx {
        Ctx {
            no_color: false,
            log_level: "info".into(),
            state_dir: base.clone(),
            config_dir: base.join("cfg"),
            runtime_dir: base.join("rt"),
        }
    }

    fn seed_session(layout: &StateLayout, id: &SessionId) {
        std::fs::create_dir_all(layout.session_dir(id)).expect("mkdir session");
    }

    #[test]
    fn resolve_active_session_errors_when_none() {
        let tmp = tempdir().unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let err = resolve_active_session(None, &layout).expect_err("no sessions");
        assert!(
            matches!(err, CliError::NotFound { ref what } if what.contains("no active session"))
        );
    }

    #[test]
    fn resolve_active_session_picks_unique_session() {
        let tmp = tempdir().unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("lonely");
        seed_session(&layout, &id);

        let got = resolve_active_session(None, &layout).expect("unique resolves");
        assert_eq!(got.as_str(), id.as_str());
    }

    #[test]
    fn resolve_active_session_ambiguous_when_two() {
        let tmp = tempdir().unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let a = SessionId::new("one");
        let b = SessionId::new("two");
        seed_session(&layout, &a);
        seed_session(&layout, &b);

        let err = resolve_active_session(None, &layout).expect_err("ambiguous");
        match err {
            CliError::Ambiguous { candidates, .. } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_active_session_explicit_fragment_resolves() {
        let tmp = tempdir().unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("authflow");
        seed_session(&layout, &id);

        let got = resolve_active_session(Some("authflow"), &layout).expect("fragment resolves");
        assert_eq!(got.as_str(), id.as_str());
    }

    // ------- render_response classification -------

    #[test]
    fn render_response_ok_intent_prints_summary() {
        let id = SessionId::new("bus-rs");
        let resp = json!({
            "ok": true,
            "data": {
                "dispatched": "ark.core.ping",
                "result": null,
            }
        });
        render_response(
            &id,
            &BusCommand::Intent {
                json: String::new(),
            },
            &resp,
        )
        .expect("ok");
    }

    #[test]
    fn render_response_ok_emit_prints_receiver_count() {
        let id = SessionId::new("bus-rs");
        let resp = json!({
            "ok": true,
            "data": {
                "broadcast": "ark.zellij.pane_closed",
                "receivers": 3,
            }
        });
        render_response(
            &id,
            &BusCommand::Emit {
                json: String::new(),
            },
            &resp,
        )
        .expect("ok");
    }

    #[test]
    fn render_response_surfaces_error_envelope() {
        let id = SessionId::new("bus-err");
        let resp = json!({
            "ok": false,
            "error": "intents disabled (no IntentRegistry wired)",
        });
        let err = render_response(
            &id,
            &BusCommand::Intent {
                json: String::new(),
            },
            &resp,
        )
        .expect_err("must err");
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("intents disabled"));
                assert!(reason.contains("intent"));
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    // ------- end-to-end via live UnixSocket (reduced supervisor) -------
    //
    // A real supervisor needs the full T-069 orchestration scaffold; for
    // the bus CLI's contract we only need the wire shape. This spins a
    // tiny NDJSON echo server at `$runtime/sessions/<id>.sock` and
    // asserts: (a) `ark bus intent` writes the documented bytes, (b) a
    // response envelope round-trips, (c) session resolution picks the
    // unique session when `--session` is absent. The full integration
    // against the real supervisor is gap-flagged in the packet report.

    use std::io::{BufRead as _, BufReader as StdBufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn spin_echo_supervisor(
        layout: &StateLayout,
        id: &SessionId,
        response: Value,
    ) -> thread::JoinHandle<Value> {
        // Make sure sessions/<id>.sock parent exists with the expected
        // perms; ark-core has an `ensure_sessions_dir` helper but we
        // avoid pulling it in (sync-only test).
        let sock = layout.session_socket_path(id);
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind test socket");
        thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut reader = StdBufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).expect("read req");
            let req: Value = serde_json::from_str(line.trim()).expect("parse req");
            let mut bytes = serde_json::to_vec(&response).unwrap();
            bytes.push(b'\n');
            conn.write_all(&bytes).expect("write resp");
            req
        })
    }

    #[test]
    fn end_to_end_intent_roundtrips_through_echo_supervisor() {
        // Use /tmp so the socket path stays well under SUN_LEN on macOS.
        let tmp = tempfile::Builder::new()
            .prefix("arkbus")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("bus-e2e");
        seed_session(&layout, &id);

        let server_resp = json!({
            "ok": true,
            "data": {
                "dispatched": "ark.core.ping",
                "result": null,
            }
        });
        let server = spin_echo_supervisor(&layout, &id, server_resp);

        // Invoke the full `run` path with the unique-session fallback.
        let args = BusArgs {
            session: None,
            command: BusCommand::Intent {
                json: r#"{"name":"ark.core.ping","args":{}}"#.to_string(),
            },
        };
        run(args, &ctx).expect("round-trip");

        let observed_req = server.join().expect("server joined");
        assert_eq!(observed_req["cmd"], "Intent");
        assert_eq!(observed_req["args"]["name"], "ark.core.ping");
        assert!(observed_req["args"]["args"].is_object());
    }

    #[test]
    fn end_to_end_emit_roundtrips_through_echo_supervisor() {
        let tmp = tempfile::Builder::new()
            .prefix("arkbus")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("bus-emit");
        seed_session(&layout, &id);

        let server_resp = json!({
            "ok": true,
            "data": {
                "broadcast": "ark.zellij.pane_closed",
                "receivers": 1,
            }
        });
        let server = spin_echo_supervisor(&layout, &id, server_resp);

        let args = BusArgs {
            session: Some(id.as_str().to_string()),
            command: BusCommand::Emit {
                json: r#"{"event":"ark.zellij.pane_closed","payload":{"pane_id":7},"source":"ext:ark-bus"}"#
                    .to_string(),
            },
        };
        run(args, &ctx).expect("round-trip");

        let observed_req = server.join().expect("server joined");
        assert_eq!(observed_req["cmd"], "Emit");
        assert_eq!(observed_req["args"]["event"], "ark.zellij.pane_closed");
        assert_eq!(observed_req["args"]["source"], "ext:ark-bus");
        assert_eq!(observed_req["args"]["payload"]["pane_id"], 7);
    }

    #[test]
    fn end_to_end_supervisor_error_surfaces_as_generic() {
        let tmp = tempfile::Builder::new()
            .prefix("arkbus")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("bus-err");
        seed_session(&layout, &id);

        let server_resp = json!({
            "ok": false,
            "error": "intents disabled (no IntentRegistry wired)",
        });
        let _server = spin_echo_supervisor(&layout, &id, server_resp);

        let args = BusArgs {
            session: None,
            command: BusCommand::Intent {
                json: r#"{"name":"ark.core.ping"}"#.to_string(),
            },
        };
        let err = run(args, &ctx).expect_err("must surface supervisor error");
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("intents disabled"));
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn end_to_end_connect_to_missing_socket_is_orphan_or_dead() {
        // Seed a session dir but do NOT bind the socket — the
        // supervisor is "gone" from the bus client's perspective.
        let tmp = tempfile::Builder::new()
            .prefix("arkbus")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("bus-dead");
        seed_session(&layout, &id);

        let args = BusArgs {
            session: None,
            command: BusCommand::Intent {
                json: r#"{"name":"ark.core.ping"}"#.to_string(),
            },
        };
        let err = run(args, &ctx).expect_err("supervisor missing");
        assert!(
            matches!(err, CliError::OrphanOrDead { .. }),
            "expected OrphanOrDead, got {err:?}"
        );
    }
}
