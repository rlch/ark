//! `ark kill` — terminate a live agent via its supervisor
//! control socket (cavekit-cli R4, T-089).
//!
//! Wire contract (mirrored from crates/supervisor/src/commands.rs
//! rather than imported — importing the supervisor crate would pull
//! in tokio, nix, interprocess, and audit-log deps just for the
//! request shape). The on-wire NDJSON shape is:
//!
//!   Request:  {"cmd":"Kill","args":{"remove_worktree":<bool>}}
//!             {"cmd":"ForceKill"}
//!   Response: {"ok":true,"data":...}  OR  {"ok":false,"error":"..."}
//!
//! Connection flow:
//!   1. Resolve the user's fragment via `resolve_agent_id` against
//!      the on-disk `StateLayout`.
//!   2. Connect to `${runtime}/agents/{id}.sock` as a `UnixStream`.
//!   3. Write one NDJSON line, read one NDJSON line back.
//!   4. Map response `ok:true` -> stdout `killed {id}`; `ok:false`
//!      -> `CliError::Generic`; ENOENT/refused -> `OrphanOrDead`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use ark_types::StateLayout;
use clap::Args;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, resolve_agent_id};

/// Arguments for `ark kill`.
#[derive(Debug, Args)]
#[command(
    about = "Terminate an agent (SIGTERM supervisor; 10s grace)",
    long_about = "Terminate an agent. Default: SIGTERM the supervisor\n\
                  with a 10s grace window for cleanup. Use --force for\n\
                  SIGKILL (orphan cleanup deferred to `ark doctor`).\n\
                  \n\
                  Examples:\n  \
                  ark kill myfeat\n  \
                  ark kill myfeat --force"
)]
pub struct KillArgs {
    /// Agent ID fragment (full / prefix / substring).
    #[arg(value_name = "ID")]
    pub id: String,

    /// SIGKILL immediately (orphan cleanup via `ark doctor`).
    #[arg(long)]
    pub force: bool,

    /// Keep worktree (currently the default in v1; reserved).
    #[arg(long = "keep-worktree")]
    pub keep_worktree: bool,
}

/// Build the NDJSON request payload sent to the supervisor.
///
/// Pure function so unit tests can assert the on-wire shape.
/// Invariant: `force` flips the envelope to `ForceKill`; the
/// worktree flag only applies on the default `Kill` path (mirrors
/// the supervisor's current `KillArgs` struct).
fn build_request(force: bool, keep_worktree: bool) -> Value {
    if force {
        json!({ "cmd": "ForceKill" })
    } else {
        // Supervisor's KillArgs uses `remove_worktree`. The CLI flag
        // is `--keep-worktree` (opt-in to preserve). Invert here.
        json!({
            "cmd": "Kill",
            "args": { "remove_worktree": !keep_worktree }
        })
    }
}

/// Map a [`ResolveError`] to the appropriate [`CliError`].
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

/// Map a socket-connect `io::Error` to the right `CliError` variant.
/// `NotFound` / `ConnectionRefused` indicate an orphaned agent dir
/// with no live supervisor listening — steer the user at `ark doctor`.
fn map_connect_err(err: std::io::Error, id: &str) -> CliError {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => CliError::OrphanOrDead {
            reason: format!("no live supervisor for {id}; try `ark doctor`"),
        },
        _ => CliError::Generic {
            reason: format!("connect supervisor socket: {err}"),
        },
    }
}

/// Send `request` over `stream`, read one NDJSON line reply.
fn exchange(mut stream: UnixStream, request: &Value) -> Result<Value, CliError> {
    let mut line = serde_json::to_vec(request).map_err(|e| CliError::Generic {
        reason: format!("encode request: {e}"),
    })?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| CliError::Generic {
        reason: format!("write request: {e}"),
    })?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| CliError::Generic {
        reason: format!("read response: {e}"),
    })?;
    if buf.trim().is_empty() {
        return Err(CliError::Generic {
            reason: "empty response from supervisor".to_string(),
        });
    }
    serde_json::from_str::<Value>(buf.trim()).map_err(|e| CliError::Generic {
        reason: format!("parse response: {e}"),
    })
}

/// Dispatch `ark kill` — T-089.
pub fn run(args: KillArgs, ctx: &Ctx) -> Result<(), CliError> {
    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );
    let resolved = resolve_agent_id(&args.id, &layout).map_err(|e| map_resolve_err(e, &args.id))?;

    let sock = layout.agent_socket_path(&resolved);
    let stream = UnixStream::connect(&sock).map_err(|e| map_connect_err(e, resolved.as_str()))?;

    let req = build_request(args.force, args.keep_worktree);
    let resp = exchange(stream, &req)?;

    let ok = resp.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        println!("killed {}", resolved.as_str());
        Ok(())
    } else {
        let msg = resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("supervisor returned error")
            .to_string();
        Err(CliError::Generic { reason: msg })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::AgentId;
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use ulid::Ulid;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: KillArgs,
    }

    // ---------- parse round-trip ----------

    #[test]
    fn id_is_required() {
        let err = Host::try_parse_from(["kill"]).expect_err("id required");
        assert!(err.to_string().contains("required") || err.to_string().contains("ID"));
    }

    #[test]
    fn id_positional_parses() {
        let h = Host::try_parse_from(["kill", "myfeat"]).expect("parse");
        assert_eq!(h.args.id, "myfeat");
        assert!(!h.args.force);
        assert!(!h.args.keep_worktree);
    }

    #[test]
    fn force_flag_parses() {
        let h = Host::try_parse_from(["kill", "myfeat", "--force"]).expect("parse");
        assert!(h.args.force);
    }

    #[test]
    fn keep_worktree_flag_parses() {
        let h = Host::try_parse_from(["kill", "myfeat", "--keep-worktree"]).expect("parse");
        assert!(h.args.keep_worktree);
    }

    // ---------- build_request shape ----------

    #[test]
    fn build_request_default_is_kill_with_remove_worktree_true() {
        // Default: no --keep-worktree => remove_worktree = true.
        let v = build_request(false, false);
        assert_eq!(v["cmd"], "Kill");
        assert_eq!(v["args"]["remove_worktree"], serde_json::Value::Bool(true));
    }

    #[test]
    fn build_request_keep_worktree_sets_remove_false() {
        let v = build_request(false, true);
        assert_eq!(v["cmd"], "Kill");
        assert_eq!(v["args"]["remove_worktree"], serde_json::Value::Bool(false));
    }

    #[test]
    fn build_request_force_flips_cmd_to_force_kill() {
        let v = build_request(true, false);
        assert_eq!(v["cmd"], "ForceKill");
        assert!(v.get("args").is_none());
    }

    #[test]
    fn build_request_force_ignores_keep_worktree() {
        // ForceKill has no args envelope in the supervisor impl.
        let v = build_request(true, true);
        assert_eq!(v["cmd"], "ForceKill");
        assert!(v.get("args").is_none());
    }

    // ---------- resolve error mapping ----------

    fn layout_ctx(base: PathBuf) -> Ctx {
        Ctx {
            no_color: false,
            log_level: "info".into(),
            state_dir: base.clone(),
            config_dir: base.join("cfg"),
            runtime_dir: base.join("rt"),
        }
    }

    fn seed_agent(layout: &StateLayout, id: &AgentId) {
        fs::create_dir_all(layout.agent_dir(id)).expect("mkdir");
    }

    fn ulid_a() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0123").expect("ulid a")
    }
    fn ulid_b() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0456").expect("ulid b")
    }

    #[test]
    fn run_returns_not_found_when_state_empty() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = KillArgs {
            id: "ghost".into(),
            force: false,
            keep_worktree: false,
        };
        let err = run(args, &ctx).expect_err("should not find");
        assert!(
            matches!(err, CliError::NotFound { ref what } if what == "ghost"),
            "expected NotFound, got {err:?}"
        );
    }

    #[test]
    fn run_returns_ambiguous_when_multiple_prefix_matches() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let a = AgentId::from_parts("cavekit", "auth", ulid_a());
        let b = AgentId::from_parts("cavekit", "auth", ulid_b());
        seed_agent(&layout, &a);
        seed_agent(&layout, &b);

        let args = KillArgs {
            id: "cavekit-auth".into(),
            force: false,
            keep_worktree: false,
        };
        let err = run(args, &ctx).expect_err("ambiguous");
        match err {
            CliError::Ambiguous { what, candidates } => {
                assert_eq!(what, "cavekit-auth");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn run_returns_orphan_when_socket_missing() {
        // Agent dir exists; socket does not — supervisor is dead.
        // Use /tmp so the socket path stays under SUN_LEN on macOS
        // (TMPDIR resolves to long /var/folders/... paths there).
        let tmp = tempfile::Builder::new()
            .prefix("arkkill")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = AgentId::from_parts("cavekit", "dead", ulid_a());
        seed_agent(&layout, &id);

        let args = KillArgs {
            id: id.as_str().to_string(),
            force: false,
            keep_worktree: false,
        };
        let err = run(args, &ctx).expect_err("no socket");
        match err {
            CliError::OrphanOrDead { reason } => {
                assert!(
                    reason.contains("ark doctor"),
                    "reason should steer at doctor: {reason}"
                );
                assert!(
                    reason.contains(id.as_str()),
                    "reason should include id: {reason}"
                );
            }
            other => panic!("expected OrphanOrDead, got {other:?}"),
        }
    }
}
