//! `ark ext <name> <verb> [args...]` — dispatch a control-verb to a
//! live extension via the supervisor's control socket (v0.2-backlog #4).
//!
//! # Wire contract
//!
//! Mirrors the NDJSON envelope documented on
//! `ark_supervisor::commands::SupervisorCommandHandler`:
//!
//! ```text
//! Request:  {"cmd":"ControlVerbInvoke","args":{"ext":"<name>","verb":"<verb>","args":[<str>, …]}}
//! Response: {"ok":true,"data":{"ext":"…","verb":"…","data":<handler-returned-json>}}
//!           {"ok":false,"error":"…"}
//! ```
//!
//! The supervisor consults the process-global
//! `ark_supervisor::ControlVerbDispatcher` to route the invocation to
//! the owning extension's handler. When no dispatcher is registered
//! (the wiring-gap case — see v0.2 supervisor boot follow-up), the
//! supervisor returns a clear error; the CLI surfaces that verbatim
//! so the gap is visible.
//!
//! # Session resolution
//!
//! Same priority chain as `ark bus`:
//!   1. `--session <id>` explicit arg (fuzzy-resolved).
//!   2. `$ARK_SESSION_ID` env var (full id; falls back to fuzzy).
//!   3. Unique session under `$STATE/sessions/`.
//!
//! # Example
//!
//! ```text
//! $ ark ext claude-code install-hooks
//! install-hooks ok: {"outcome":"NoChange"}
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ark_types::{SessionId, StateLayout};
use clap::Args;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, list_session_ids, resolve_session_id};

/// 10-second timeout per pipe direction. A verb like `install-hooks`
/// may touch disk + walk a settings.json; the budget is generous
/// compared to the 2s keybind budget used by `ark bus`.
const VERB_TIMEOUT: Duration = Duration::from_secs(10);

/// Arguments for `ark ext <name> <verb> [args...]`.
#[derive(Debug, Args)]
#[command(
    about = "Invoke an extension's control verb on a live session",
    long_about = "Invoke a control verb on a loaded extension.\n\
                  \n\
                  The verb list per extension is discoverable via the\n\
                  extension's `control_verbs` RPC; today the canonical\n\
                  reference is the extension's own source (e.g. the\n\
                  claude-code extension contributes `install-hooks`,\n\
                  `reinstall-hook-binary`, and `reload`).\n\
                  \n\
                  Examples:\n  \
                  ark ext claude-code install-hooks\n  \
                  ark ext claude-code reinstall-hook-binary\n  \
                  ark ext claude-code reload"
)]
pub struct VerbArgs {
    /// Extension name (from the ext's manifest / `control_verbs`
    /// contribution).
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Verb name.
    #[arg(value_name = "VERB")]
    pub verb: String,

    /// Positional args forwarded verbatim to the verb handler.
    #[arg(value_name = "ARGS", trailing_var_arg = true)]
    pub args: Vec<String>,

    /// Session id override (full / prefix / substring / spec.name).
    /// When omitted, resolves from `$ARK_SESSION_ID` or the unique
    /// session dir in `$STATE/sessions/`.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
}

/// Dispatch `ark ext <name> <verb>`.
pub fn run(args: VerbArgs, ctx: &Ctx) -> Result<(), CliError> {
    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );
    let resolved = resolve_active_session(args.session.as_deref(), &layout)?;

    let request = build_request(&args.name, &args.verb, &args.args);

    let sock = layout.session_socket_path(&resolved);
    let stream = UnixStream::connect(&sock).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            CliError::OrphanOrDead {
                reason: format!(
                    "supervisor socket for session {} is gone ({e})",
                    resolved.as_str()
                ),
            }
        }
        _ => CliError::Generic {
            reason: format!("connect supervisor socket {}: {e}", sock.display()),
        },
    })?;
    let _ = stream.set_read_timeout(Some(VERB_TIMEOUT));
    let _ = stream.set_write_timeout(Some(VERB_TIMEOUT));

    let resp = exchange(stream, &request)?;
    render_response(&args.name, &args.verb, &resp)
}

/// Build the `ControlVerbInvoke` NDJSON request body.
///
/// Exposed at module scope as a pure function so the unit tests can
/// assert the on-wire bytes without a live socket.
fn build_request(ext: &str, verb: &str, args: &[String]) -> Value {
    json!({
        "cmd": "ControlVerbInvoke",
        "args": {
            "ext": ext,
            "verb": verb,
            "args": args,
        }
    })
}

/// Resolve the active session id (copy of `commands::bus::resolve_active_session`
/// local to this module so the two dispatch paths evolve
/// independently — each surfaces slightly different error copy).
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
            reason: format!("resolve: {err}"),
        },
    }
}

fn exchange(mut stream: UnixStream, request: &Value) -> Result<Value, CliError> {
    let mut line = serde_json::to_vec(request).map_err(|e| CliError::Internal {
        reason: format!("encode ext verb request: {e}"),
    })?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| CliError::Generic {
        reason: format!("write ext verb request: {e}"),
    })?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| CliError::Generic {
        reason: format!("read ext verb response: {e}"),
    })?;
    if buf.trim().is_empty() {
        return Err(CliError::Generic {
            reason: "empty response from supervisor".to_string(),
        });
    }
    serde_json::from_str::<Value>(buf.trim()).map_err(|e| CliError::Generic {
        reason: format!("parse ext verb response: {e}"),
    })
}

/// Render the supervisor's response on stdout (ok) or surface it as
/// a `CliError::Generic` (err).
fn render_response(ext: &str, verb: &str, response: &Value) -> Result<(), CliError> {
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let msg = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("supervisor returned error")
            .to_string();
        return Err(CliError::Generic {
            reason: format!("ext {ext} {verb} failed: {msg}"),
        });
    }
    // `data.data` is the handler-returned JSON; surface it compactly so
    // scripts can pipe through jq. When missing (handler returned
    // `Null`) the line is just the verb name.
    let inner = response
        .pointer("/data/data")
        .cloned()
        .unwrap_or(Value::Null);
    if inner.is_null() {
        println!("{ext} {verb} ok");
    } else {
        // Compact single-line JSON — no pretty print — so scripts can
        // pipe cleanly.
        let rendered = serde_json::to_string(&inner).unwrap_or_else(|_| "<unrenderable>".into());
        println!("{ext} {verb} ok: {rendered}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::SessionId;
    use clap::Parser;
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: VerbArgs,
    }

    // -------- parse tests --------

    #[test]
    fn name_and_verb_required() {
        let err = Host::try_parse_from(["cmd"]).expect_err("name required");
        let msg = err.to_string();
        assert!(msg.contains("required") || msg.contains("NAME"));
    }

    #[test]
    fn positional_name_and_verb_parse() {
        let h = Host::try_parse_from(["cmd", "claude-code", "install-hooks"]).expect("parse");
        assert_eq!(h.args.name, "claude-code");
        assert_eq!(h.args.verb, "install-hooks");
        assert!(h.args.args.is_empty());
    }

    #[test]
    fn extra_positionals_collected_into_args() {
        let h = Host::try_parse_from(["cmd", "ext", "verb", "a", "b", "c"]).expect("parse");
        assert_eq!(h.args.args, vec!["a".to_string(), "b".into(), "c".into()]);
    }

    #[test]
    fn session_flag_parses() {
        let h = Host::try_parse_from(["cmd", "--session", "foo", "ext", "v"]).expect("parse");
        assert_eq!(h.args.session.as_deref(), Some("foo"));
    }

    // -------- build_request shape --------

    #[test]
    fn build_request_shape_has_cmd_and_args() {
        let v = build_request("claude-code", "install-hooks", &[]);
        assert_eq!(v["cmd"], "ControlVerbInvoke");
        assert_eq!(v["args"]["ext"], "claude-code");
        assert_eq!(v["args"]["verb"], "install-hooks");
        assert_eq!(v["args"]["args"], serde_json::json!([]));
    }

    #[test]
    fn build_request_preserves_arg_order() {
        let v = build_request("x", "y", &["a".into(), "b".into(), "c".into()]);
        assert_eq!(v["args"]["args"], serde_json::json!(["a", "b", "c"]));
    }

    // -------- resolve_active_session --------

    fn layout_ctx(base: PathBuf) -> Ctx {
        Ctx {
            no_color: false,
            log_level: "info".into(),
            state_dir: base.clone(),
            config_dir: base.join("cfg"),
            runtime_dir: base.join("rt"),
        }
    }

    #[test]
    fn resolve_active_session_errors_when_none() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::new(
            tmp.path().to_path_buf(),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let err = resolve_active_session(None, &layout).expect_err("no sessions");
        assert!(matches!(err, CliError::NotFound { .. }));
    }

    #[test]
    fn resolve_active_session_unique_session_picks_it() {
        let tmp = tempfile::Builder::new()
            .prefix("arkext")
            .tempdir_in("/tmp")
            .unwrap();
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("only");
        fs::create_dir_all(layout.session_dir(&id)).unwrap();

        let got = resolve_active_session(None, &layout).expect("unique");
        assert_eq!(got, id);
    }

    // -------- render_response --------

    #[test]
    fn render_response_ok_with_null_data_prints_no_payload() {
        let v = json!({"ok": true, "data": {"ext": "x", "verb": "y", "data": null}});
        render_response("x", "y", &v).expect("ok");
    }

    #[test]
    fn render_response_ok_with_data_prints_payload() {
        let v = json!({
            "ok": true,
            "data": {"ext": "x", "verb": "y", "data": {"hello": "world"}}
        });
        render_response("x", "y", &v).expect("ok with data");
    }

    #[test]
    fn render_response_err_bubbles_as_generic() {
        let v = json!({"ok": false, "error": "unknown extension: nope"});
        let err = render_response("nope", "v", &v).expect_err("err");
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("unknown extension"));
                assert!(reason.contains("nope"));
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    // -------- end-to-end via echo supervisor --------

    /// Spawn a throwaway UnixListener that reads one line and writes
    /// a canned response line. Mirrors the `commands::bus::tests`
    /// echo-supervisor harness — we are stand-alone because the bus
    /// module's helpers are private to that file.
    fn spawn_echo(sock_path: PathBuf, response: Value) -> thread::JoinHandle<Vec<u8>> {
        if let Some(parent) = sock_path.parent() {
            fs::create_dir_all(parent).expect("mkdir sock parent");
        }
        let _ = fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request_bytes = Vec::new();
            let mut buf = [0u8; 4096];
            // One read is sufficient; client sends a single NDJSON line.
            let n = stream.read(&mut buf).expect("read");
            request_bytes.extend_from_slice(&buf[..n]);
            let mut reply = serde_json::to_vec(&response).expect("encode");
            reply.push(b'\n');
            stream.write_all(&reply).ok();
            stream.flush().ok();
            request_bytes
        })
    }

    #[test]
    fn run_success_path_over_echo_supervisor() {
        let tmp = tempfile::Builder::new()
            .prefix("extverb")
            .tempdir_in("/tmp")
            .expect("tmp");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("s");
        fs::create_dir_all(layout.session_dir(&id)).unwrap();
        let sock = layout.session_socket_path(&id);
        let _ = fs::remove_file(&sock);

        let response = json!({
            "ok": true,
            "data": {
                "ext": "claude-code",
                "verb": "install-hooks",
                "data": {"outcome": "NoChange"}
            }
        });
        let handle = spawn_echo(sock.clone(), response);

        let args = VerbArgs {
            name: "claude-code".into(),
            verb: "install-hooks".into(),
            args: vec![],
            session: None,
        };
        run(args, &ctx).expect("run ok");

        let request_bytes = handle.join().expect("echo thread");
        let line = std::str::from_utf8(&request_bytes).expect("utf8");
        let parsed: Value = serde_json::from_str(line.trim()).expect("parse");
        assert_eq!(parsed["cmd"], "ControlVerbInvoke");
        assert_eq!(parsed["args"]["ext"], "claude-code");
        assert_eq!(parsed["args"]["verb"], "install-hooks");
    }

    #[test]
    fn run_err_path_over_echo_supervisor_surfaces_error() {
        let tmp = tempfile::Builder::new()
            .prefix("extverb2")
            .tempdir_in("/tmp")
            .expect("tmp");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("s");
        fs::create_dir_all(layout.session_dir(&id)).unwrap();
        let sock = layout.session_socket_path(&id);
        let _ = fs::remove_file(&sock);

        let response = json!({
            "ok": false,
            "error": "no control-verb dispatcher registered"
        });
        let handle = spawn_echo(sock.clone(), response);

        let args = VerbArgs {
            name: "foo".into(),
            verb: "bar".into(),
            args: vec![],
            session: None,
        };
        let err = run(args, &ctx).expect_err("err");
        handle.join().ok();
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("no control-verb dispatcher"), "{reason}");
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn run_missing_socket_returns_orphan_or_dead() {
        let tmp = tempfile::Builder::new()
            .prefix("extverb3")
            .tempdir_in("/tmp")
            .expect("tmp");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("dead");
        fs::create_dir_all(layout.session_dir(&id)).unwrap();
        // No socket bind — supervisor is gone.

        let args = VerbArgs {
            name: "x".into(),
            verb: "y".into(),
            args: vec![],
            session: None,
        };
        let err = run(args, &ctx).expect_err("err");
        assert!(matches!(err, CliError::OrphanOrDead { .. }));
    }
}
