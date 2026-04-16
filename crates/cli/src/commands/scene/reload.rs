//! `ark scene reload` — hot-reload via supervisor control socket.
//!
//! T-12.7 (cavekit-scene R13, R14). Sends a scene-reload request to
//! the supervisor over the per-agent control socket (cavekit-hook-ipc
//! R1). The reload itself runs inside the supervisor via the
//! `ark.core.reload_scene` intent (see `crates/scene/src/ops/control.rs`),
//! which wraps [`ark_scene::reload::SceneReloader`] and honours the
//! re-entry guard + turn-inflight gate (T-11.1).
//!
//! Wire shape: the existing `Intent { name, args }` control-socket
//! command is reused — no new supervisor variant needed. We send
//!
//! ```json
//! { "cmd": "Intent", "args": { "name": "ark.core.reload_scene", "args": {} } }
//! ```
//!
//! and parse the standard `{ok, data, error}` envelope the supervisor
//! echoes back.
//!
//! Session resolution:
//! * `--session <name>` — resolves against the on-disk agent layout
//!   via [`resolve_agent_id`] (the same helper `ark kill` uses), so
//!   users can pass any unambiguous id fragment (full id, prefix,
//!   substring, or `spec.json` name).
//! * When omitted, the command errors with a clear message pointing
//!   the user at `ark list` + `--session`. A "default session" would
//!   silently reload the wrong agent when multiple are running; the
//!   command is intentionally strict here.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use ark_types::StateLayout;
use clap::Args;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, resolve_agent_id};

/// Arguments for `ark scene reload`.
#[derive(Debug, Args)]
pub struct ReloadArgs {
    /// Agent session id (full / prefix / substring / spec.name).
    /// Required — omitting it errors with a list-pointer.
    #[arg(long)]
    pub session: Option<String>,
}

pub fn run(args: ReloadArgs, ctx: &Ctx) -> Result<(), CliError> {
    let Some(query) = args.session.as_deref() else {
        return Err(CliError::Generic {
            reason: "`ark scene reload` requires `--session <id>`; run `ark list` to pick"
                .to_string(),
        });
    };

    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );
    let resolved = resolve_agent_id(query, &layout).map_err(|e| map_resolve_err(e, query))?;

    let sock = layout.agent_socket_path(&resolved);
    let stream = UnixStream::connect(&sock).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            CliError::OrphanOrDead {
                reason: format!(
                    "agent {} has no live supervisor socket (already killed?)",
                    resolved.as_str()
                ),
            }
        }
        _ => CliError::Generic {
            reason: format!("connect supervisor socket {}: {e}", sock.display()),
        },
    })?;

    let request = build_request();
    let response = exchange(stream, &request)?;
    render_response(&resolved, &response)
}

/// Build the NDJSON request envelope. Pure — unit-tested for shape.
fn build_request() -> Value {
    json!({
        "cmd": "Intent",
        "args": {
            "name": "ark.core.reload_scene",
            "args": {}
        }
    })
}

/// Send one NDJSON line + read one NDJSON line response.
fn exchange(mut stream: UnixStream, request: &Value) -> Result<Value, CliError> {
    let mut bytes = serde_json::to_vec(request).map_err(|e| CliError::Internal {
        reason: format!("encode reload request: {e}"),
    })?;
    bytes.push(b'\n');
    stream.write_all(&bytes).map_err(|e| CliError::Generic {
        reason: format!("write reload request: {e}"),
    })?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| CliError::Generic {
        reason: format!("read reload response: {e}"),
    })?;
    if line.trim().is_empty() {
        return Err(CliError::Generic {
            reason: "empty response from supervisor".to_string(),
        });
    }
    serde_json::from_str::<Value>(line.trim()).map_err(|e| CliError::Generic {
        reason: format!("parse reload response: {e}"),
    })
}

/// Render the response envelope on stdout + classify errors.
fn render_response(
    resolved: &ark_types::AgentId,
    response: &Value,
) -> Result<(), CliError> {
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let msg = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("supervisor returned error")
            .to_string();
        return Err(CliError::Generic {
            reason: format!("reload failed for {}: {msg}", resolved.as_str()),
        });
    }

    let data = response.get("data").cloned().unwrap_or(Value::Null);
    // The `ark.core.reload_scene` intent wraps its payload in
    // `{dispatched, result}` via the supervisor's `Intent` handler.
    // Unwrap `result` if present, otherwise fall back to `data`.
    let result = data
        .get("result")
        .cloned()
        .unwrap_or(data.clone());

    // The reload payload shape (from SceneReloader::reload →
    // ReloadSceneOp) carries `status` and counters.
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("applied");
    let reactions_added = result
        .get("reactions_added")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reactions_removed = result
        .get("reactions_removed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let keybinds_changed = result
        .get("keybinds_changed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let plugins_changed = result
        .get("plugins_changed")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    println!(
        "scene reload ({id}) status={status}  reactions=+{ra}/-{rr}  keybinds={kc}  plugins={pc}",
        id = resolved.as_str(),
        status = status,
        ra = reactions_added,
        rr = reactions_removed,
        kc = keybinds_changed,
        pc = plugins_changed
    );
    if let Some(stage) = result
        .get("failed_stage")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        eprintln!("  (partial — failed stage: {stage})");
    }
    Ok(())
}

/// Map a [`ResolveError`] to the appropriate [`CliError`] variant —
/// same shape as `ark kill` so the exit codes stay consistent across
/// agent-targeting subcommands.
fn map_resolve_err(e: ResolveError, query: &str) -> CliError {
    match e {
        ResolveError::NotFound { .. } => CliError::NotFound {
            what: query.to_string(),
        },
        ResolveError::AmbiguousPrefix { candidates, .. }
        | ResolveError::AmbiguousSubstring { candidates, .. }
        | ResolveError::AmbiguousName { candidates, .. } => CliError::Ambiguous {
            what: query.to_string(),
            candidates: candidates.into_iter().map(|c| c.as_str().to_string()).collect(),
        },
        ResolveError::Io(err) => CliError::Generic {
            reason: format!("resolve session: {err}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::AgentId;

    #[test]
    fn build_request_has_expected_shape() {
        let v = build_request();
        assert_eq!(v["cmd"], "Intent");
        assert_eq!(v["args"]["name"], "ark.core.reload_scene");
        assert!(
            v["args"]["args"].is_object(),
            "args.args must be an object (the intent payload); got {v}"
        );
    }

    #[test]
    fn render_response_reports_ok() {
        let id = AgentId::new("cavekit", "scene-reload");
        let resp = serde_json::json!({
            "ok": true,
            "data": {
                "dispatched": "ark.core.reload_scene",
                "result": {
                    "status": "ok",
                    "duration_ms": 2,
                    "reactions_added": 1,
                    "reactions_removed": 0,
                    "keybinds_changed": 0,
                    "plugins_changed": 0,
                    "failed_stage": null,
                }
            }
        });
        render_response(&id, &resp).expect("ok response");
    }

    #[test]
    fn render_response_surfaces_error_envelope() {
        let id = AgentId::new("cavekit", "scene-reload");
        let resp = serde_json::json!({
            "ok": false,
            "error": "intents disabled (no IntentRegistry wired)"
        });
        let err = render_response(&id, &resp).expect_err("should be err");
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("intents disabled"), "got {reason}");
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn render_response_bubbles_partial_stage() {
        let id = AgentId::new("cavekit", "scene-reload");
        let resp = serde_json::json!({
            "ok": true,
            "data": {
                "result": {
                    "status": "partial",
                    "failed_stage": "layout",
                    "reactions_added": 0,
                    "reactions_removed": 0,
                    "keybinds_changed": 0,
                    "plugins_changed": 0,
                }
            }
        });
        render_response(&id, &resp).expect("partial is still ok");
    }

    #[test]
    fn run_errors_when_session_omitted() {
        let ctx = Ctx::default();
        let args = ReloadArgs { session: None };
        let err = run(args, &ctx).expect_err("session is required");
        match err {
            CliError::Generic { reason } => {
                assert!(
                    reason.contains("--session"),
                    "error should point at --session: {reason}"
                );
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }
}
