//! Per-supervisor control-socket command handlers (T-066).
//!
//! Implements the [`ControlCommandHandler`](crate::ControlCommandHandler)
//! protocol defined by cavekit-hook-ipc.md R5. Each supervisor owns one
//! socket (see cavekit-supervisor.md R7) and routes inbound requests here.
//!
//! # Request / response shape
//!
//! Request:
//! ```json
//! { "cmd": "<name>", "args": { ... } }
//! ```
//! `args` is optional and defaults to an empty object.
//!
//! Response:
//! ```json
//! { "ok": true, "data": <T> }
//! // or
//! { "ok": false, "error": "<message>" }
//! ```
//!
//! # Commands
//!
//! | Command     | Args                          | Effect                                                                        |
//! | ----------- | ----------------------------- | ----------------------------------------------------------------------------- |
//! | `Ping`      | -                             | Echoes `"pong"`.                                                              |
//! | `Status`    | `{}`                          | Reads this agent's `status.json` and returns its full JSON.                   |
//! | `Kill`      | `{ "remove_worktree": bool }` | `SIGTERM` this supervisor's own pid + fires `cancel`.                         |
//! | `ForceKill` | `{}`                          | `SIGKILL` this supervisor's own process group. Often kills us before reply.   |
//! | `Rename`    | `{ "new_name": "..." }`       | Mutates `spec.json.name` atomically. Session name stays frozen.               |
//! | `Forget`    | `{}`                          | Sets `status.json.hide = true` atomically so the picker omits this agent.     |
//!
//! Unknown commands return `{"ok": false, "error": "unknown command: <name>"}`
//! and the connection remains open per cavekit-hook-ipc.md R4.
//!
//! # Signal injection
//!
//! [`SupervisorCommandHandler`] signals via an injected
//! [`SignalSender`] so tests can record calls without actually signalling.
//! Production callers construct with
//! [`SupervisorCommandHandler::new`] (uses `nix::sys::signal::kill`);
//! tests use [`SupervisorCommandHandler::new_with_sender`] to pass a
//! recording closure.
//!
//! # `Kill` + `remove_worktree`
//!
//! The `remove_worktree` flag is accepted but the actual `git worktree
//! remove` call is deferred to Tier 4 (see `cavekit-cli.md`). T-066 records
//! the intent on the response but does not invoke git; the supervisor
//! cancel path already tears the agent down.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use ark_core::{Response, read_status};
use ark_types::{AgentId, EventSink, StateLayout};
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::control_socket::ControlCommandHandler;

/// Signature of the pluggable signal sender.
///
/// Production: `nix::sys::signal::kill`. Tests: a recording closure.
pub type SignalSender = Arc<dyn Fn(Pid, Option<Signal>) -> nix::Result<()> + Send + Sync>;

/// Context threaded into every command handler.
///
/// Constructed by the supervisor's R3 boot sequence (T-069 wires this
/// together). Kept a plain struct with `pub` fields so T-069 can build it
/// without an async factory.
#[derive(Clone)]
pub struct SupervisorCommandCtx {
    /// Identifier for the agent this supervisor owns.
    pub agent_id: AgentId,
    /// On-disk layout used to resolve `status.json` / `spec.json`.
    pub state_layout: StateLayout,
    /// Supervisor's own pid. Used as the SIGTERM target for `Kill` and
    /// as the **process group leader** for `ForceKill` (pgid == pid after
    /// `setsid`).
    pub pid: Pid,
    /// Fired by `Kill` so the orchestrator loop can unwind cleanly.
    pub cancel: CancellationToken,
    /// Held for future command-audit-log integration (cavekit-hook-ipc
    /// R5 audit log is T-068 — this field lets T-066 ship without
    /// coupling, and T-068 can emit from here without re-plumbing the
    /// struct).
    #[allow(dead_code)]
    pub event_bus: EventSink,
}

impl std::fmt::Debug for SupervisorCommandCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisorCommandCtx")
            .field("agent_id", &self.agent_id.as_str())
            .field("pid", &self.pid.as_raw())
            .field("cancel.cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// Handler implementing the R5 protocol.
pub struct SupervisorCommandHandler {
    ctx: SupervisorCommandCtx,
    signal: SignalSender,
}

impl SupervisorCommandHandler {
    /// Construct with the real `nix::sys::signal::kill`.
    pub fn new(ctx: SupervisorCommandCtx) -> Self {
        Self {
            ctx,
            signal: Arc::new(real_kill),
        }
    }

    /// Construct with an injected signal sender — for tests.
    pub fn new_with_sender(ctx: SupervisorCommandCtx, sender: SignalSender) -> Self {
        Self {
            ctx,
            signal: sender,
        }
    }

    /// Dispatch a single parsed request to the matching command.
    async fn dispatch(&self, req: Request) -> Response<JsonValue> {
        match req.cmd.as_str() {
            "Ping" => Response::ok(JsonValue::String("pong".to_string())),
            "Status" => handle_status(&self.ctx),
            "Kill" => {
                let args: KillArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(e.to_string()),
                };
                handle_kill(&self.ctx, &self.signal, args)
            }
            "ForceKill" => handle_force_kill(&self.ctx, &self.signal),
            "Rename" => {
                let args: RenameArgs = match req.args_as() {
                    Ok(a) => a,
                    Err(e) => return Response::err(e.to_string()),
                };
                handle_rename(&self.ctx, args)
            }
            "Forget" => handle_forget(&self.ctx),
            other => Response::err(format!("unknown command: {other}")),
        }
    }
}

impl ControlCommandHandler for SupervisorCommandHandler {
    fn handle(&self, req: JsonValue) -> Pin<Box<dyn Future<Output = JsonValue> + Send + '_>> {
        Box::pin(async move {
            let parsed = match serde_json::from_value::<Request>(req) {
                Ok(r) => r,
                Err(e) => {
                    return serde_json::to_value(Response::<JsonValue>::err(format!(
                        "malformed request: {e}"
                    )))
                    .expect("serialize err response");
                }
            };
            let resp = self.dispatch(parsed).await;
            serde_json::to_value(resp).expect("serialize response")
        })
    }
}

/// Wire-level request envelope. `args` is optional; missing = `{}`.
#[derive(Debug, Deserialize)]
struct Request {
    cmd: String,
    #[serde(default)]
    args: JsonValue,
}

impl Request {
    fn args_as<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        // Treat Null and missing as `{}` for ergonomics.
        if self.args.is_null() {
            serde_json::from_value(serde_json::json!({}))
        } else {
            serde_json::from_value(self.args.clone())
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct KillArgs {
    #[serde(default)]
    remove_worktree: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RenameArgs {
    new_name: String,
}

// ------- command implementations -----------------------------------------

fn handle_status(ctx: &SupervisorCommandCtx) -> Response<JsonValue> {
    match read_status(&ctx.state_layout, &ctx.agent_id) {
        Ok(Some(status)) => match serde_json::to_value(&status) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(format!("serialize status: {e}")),
        },
        Ok(None) => Response::err(format!(
            "status.json not found for agent {}",
            ctx.agent_id.as_str()
        )),
        Err(e) => Response::err(format!("read status: {e}")),
    }
}

fn handle_kill(
    ctx: &SupervisorCommandCtx,
    signal: &SignalSender,
    args: KillArgs,
) -> Response<JsonValue> {
    if args.remove_worktree {
        // Recorded intent only — actual `git worktree remove` goes through
        // ark-cli in Tier 4. Document via log.
        debug!(
            agent = ctx.agent_id.as_str(),
            "Kill with remove_worktree=true; cleanup deferred to ark-cli"
        );
    }
    if let Err(e) = (signal)(ctx.pid, Some(Signal::SIGTERM)) {
        return Response::err(format!("SIGTERM self failed: {e}"));
    }
    ctx.cancel.cancel();
    let data = serde_json::json!({
        "signaled": "SIGTERM",
        "remove_worktree": args.remove_worktree,
    });
    Response::ok(data)
}

fn handle_force_kill(ctx: &SupervisorCommandCtx, signal: &SignalSender) -> Response<JsonValue> {
    // Target the process *group*: Pid::from_raw(-pgid) with SIGKILL.
    // Supervisor has run `setsid`, so its pid == pgid. This call usually
    // kills the current process before we manage to write a reply — the
    // best-effort response is documented.
    let pgid = Pid::from_raw(-ctx.pid.as_raw());
    if let Err(e) = (signal)(pgid, Some(Signal::SIGKILL)) {
        return Response::err(format!("SIGKILL pgid failed: {e}"));
    }
    // If we somehow reach this line, still respond best-effort.
    Response::ok(serde_json::json!({ "signaled": "SIGKILL" }))
}

fn handle_rename(ctx: &SupervisorCommandCtx, args: RenameArgs) -> Response<JsonValue> {
    let spec_path = ctx.state_layout.spec_path(&ctx.agent_id);
    match read_json_file(&spec_path) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                // Session name is frozen — only mutate the human label.
                obj.insert("name".into(), JsonValue::String(args.new_name.clone()));
            } else {
                return Response::err(format!(
                    "spec.json at {} is not a JSON object",
                    spec_path.display()
                ));
            }
            if let Err(e) = write_json_atomic(&spec_path, &v) {
                return Response::err(format!("write spec.json: {e}"));
            }
            Response::ok(serde_json::json!({ "renamed_to": args.new_name }))
        }
        Err(e) => Response::err(format!("read spec.json: {e}")),
    }
}

fn handle_forget(ctx: &SupervisorCommandCtx) -> Response<JsonValue> {
    let status_path = ctx.state_layout.status_path(&ctx.agent_id);
    match read_json_file(&status_path) {
        Ok(mut v) => {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("hide".into(), JsonValue::Bool(true));
            } else {
                return Response::err(format!(
                    "status.json at {} is not a JSON object",
                    status_path.display()
                ));
            }
            if let Err(e) = write_json_atomic(&status_path, &v) {
                return Response::err(format!("write status.json: {e}"));
            }
            Response::ok(serde_json::json!({ "hidden": true }))
        }
        Err(e) => Response::err(format!("read status.json: {e}")),
    }
}

// ------- internals -------------------------------------------------------

fn read_json_file(path: &Path) -> std::io::Result<JsonValue> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Atomic write: temp file + rename. Same pattern as
/// [`ark_core::write_status_atomic`] but generic over any JSON value so we
/// can round-trip `spec.json` without forcing it through the `AgentSpec`
/// type (keeps the field set flexible against future additions).
fn write_json_atomic(path: &Path, value: &JsonValue) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut tmp = path.to_path_buf();
    let mut fname = tmp
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("out"));
    fname.push(".tmp");
    tmp.set_file_name(fname);

    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn real_kill(pid: Pid, sig: Option<Signal>) -> nix::Result<()> {
    match nix::sys::signal::kill(pid, sig) {
        Ok(()) => Ok(()),
        Err(err) => {
            warn!(pid = pid.as_raw(), ?sig, %err, "kill syscall failed");
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::write_status_atomic;
    use ark_types::{AgentSpec, AgentStatus, Phase, default_channel};
    use chrono::Utc;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use interprocess::local_socket::{ConnectOptions, GenericFilePath, ToFsName};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("sv")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    fn layout_at(base: &Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    fn sample_spec(id: &AgentId) -> AgentSpec {
        let mut s = AgentSpec::new(
            id.clone(),
            "friendly",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        s.env = BTreeMap::new();
        s
    }

    fn sample_status(id: &AgentId) -> AgentStatus {
        AgentStatus {
            spec: sample_spec(id),
            phase: Phase::Running,
            progress: Some((1, 3)),
            last_event_at: Utc::now(),
            last_event_summary: "running".into(),
            tab_handles: vec![],
            supervisor_pid: 4242,
            stalled_since: None,
            findings: Default::default(),
            hide: false,
        }
    }

    /// Record of calls made through the injected signal sender.
    #[derive(Default)]
    struct SignalRecorder {
        calls: Mutex<Vec<(i32, Option<Signal>)>>,
    }

    impl SignalRecorder {
        fn sender(self: &Arc<Self>) -> SignalSender {
            let me = self.clone();
            Arc::new(move |pid, sig| {
                me.calls.lock().unwrap().push((pid.as_raw(), sig));
                Ok(())
            })
        }
        fn calls(&self) -> Vec<(i32, Option<Signal>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn make_ctx(id: AgentId, layout: StateLayout) -> (SupervisorCommandCtx, CancellationToken) {
        let cancel = CancellationToken::new();
        let (tx, _rx) = default_channel();
        let ctx = SupervisorCommandCtx {
            agent_id: id,
            state_layout: layout,
            pid: Pid::from_raw(12345),
            cancel: cancel.clone(),
            event_bus: tx,
        };
        (ctx, cancel)
    }

    async fn bind_and_connect(
        handler: Arc<dyn ControlCommandHandler>,
        layout: &StateLayout,
        id: &AgentId,
    ) -> crate::ControlSocketHandle {
        crate::bind_control_socket(layout, id, handler)
            .await
            .expect("bind")
    }

    async fn connect_retry(path: &Path) -> interprocess::local_socket::tokio::Stream {
        let name = path.as_os_str().to_fs_name::<GenericFilePath>().unwrap();
        let mut last = None;
        for _ in 0..40 {
            match ConnectOptions::new()
                .name(name.clone())
                .connect_tokio()
                .await
            {
                Ok(s) => return s,
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        panic!("client connect failed: {last:?}");
    }

    async fn send_and_recv(path: &Path, line: &[u8]) -> JsonValue {
        let stream = connect_retry(path).await;
        let (r, w) = stream.split();
        let mut w = w;
        w.write_all(line).await.unwrap();
        w.flush().await.unwrap();
        let mut reader = BufReader::new(r);
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        serde_json::from_str(buf.trim()).unwrap()
    }

    // -------- direct dispatch tests -----------------------------------

    #[tokio::test]
    async fn ping_returns_pong() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "ping");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "Ping" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"], JsonValue::String("pong".into()));
    }

    #[tokio::test]
    async fn status_reads_existing_file() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "status");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({ "cmd": "Status", "args": {} }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["phase"], JsonValue::String("running".into()));
        assert_eq!(resp["data"]["supervisor_pid"], serde_json::json!(4242));
    }

    #[tokio::test]
    async fn status_missing_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "missing");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "Status" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].as_str().unwrap().contains("not found"),
            "error should mention missing status, got {resp}"
        );
    }

    #[tokio::test]
    async fn kill_sends_sigterm_and_cancels() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "kill");
        let (ctx, cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx.clone(), rec.sender());

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Kill",
                "args": { "remove_worktree": false }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(
            resp["data"]["signaled"],
            JsonValue::String("SIGTERM".into())
        );

        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ctx.pid.as_raw());
        assert_eq!(calls[0].1, Some(Signal::SIGTERM));
        assert!(cancel.is_cancelled(), "cancel token must fire");
    }

    #[tokio::test]
    async fn kill_with_remove_worktree_records_flag() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "killwt");
        let (ctx, _cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx, rec.sender());

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Kill",
                "args": { "remove_worktree": true }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["remove_worktree"], JsonValue::Bool(true));
    }

    #[tokio::test]
    async fn force_kill_targets_process_group_with_sigkill() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "force");
        let (ctx, _cancel) = make_ctx(id, layout);
        let rec = Arc::new(SignalRecorder::default());
        let h = SupervisorCommandHandler::new_with_sender(ctx.clone(), rec.sender());

        let resp = h.handle(serde_json::json!({ "cmd": "ForceKill" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(
            resp["data"]["signaled"],
            JsonValue::String("SIGKILL".into())
        );

        let calls = rec.calls();
        assert_eq!(calls.len(), 1);
        // Negative pid = process group.
        assert_eq!(calls[0].0, -ctx.pid.as_raw());
        assert_eq!(calls[0].1, Some(Signal::SIGKILL));
    }

    #[tokio::test]
    async fn rename_updates_spec_json_name_field() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "rename");
        // Pre-write a spec.json via the AgentSpec type so the file has the
        // full schema.
        let spec = sample_spec(&id);
        let spec_path = layout.spec_path(&id);
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({
                "cmd": "Rename",
                "args": { "new_name": "renamed-label" }
            }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));

        let raw = std::fs::read_to_string(&spec_path).unwrap();
        let parsed: JsonValue = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["name"], JsonValue::String("renamed-label".into()));
        // Session name remains the original derived session.
        let original_session = id.session_name();
        assert_eq!(parsed["session"], JsonValue::String(original_session));
    }

    #[tokio::test]
    async fn forget_sets_status_hide_true() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "forget");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h
            .handle(serde_json::json!({ "cmd": "Forget", "args": {} }))
            .await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));

        let read_back = ark_core::read_status(&layout, &id).unwrap().unwrap();
        assert!(read_back.hide, "hide flag must be set");
    }

    #[tokio::test]
    async fn unknown_command_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "unk");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        let resp = h.handle(serde_json::json!({ "cmd": "DoesNotExist" })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(
            resp["error"].as_str().unwrap().contains("unknown command"),
            "got {resp}"
        );
    }

    #[tokio::test]
    async fn malformed_request_returns_error() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "malf");
        let (ctx, _cancel) = make_ctx(id, layout);
        let h = SupervisorCommandHandler::new(ctx);

        // Missing "cmd" field.
        let resp = h.handle(serde_json::json!({ "oops": true })).await;
        assert_eq!(resp["ok"], JsonValue::Bool(false));
        assert!(resp["error"].as_str().unwrap().contains("malformed"));
    }

    // -------- end-to-end via live socket ------------------------------

    #[tokio::test]
    async fn over_socket_unknown_then_ping_survives() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "survive");
        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        // First connection: unknown command.
        let resp1 = send_and_recv(handle.path(), b"{\"cmd\":\"Bogus\"}\n").await;
        assert_eq!(resp1["ok"], JsonValue::Bool(false));
        assert!(resp1["error"].as_str().unwrap().contains("unknown command"));

        // Second connection: valid Ping — listener must still serve.
        let resp2 = send_and_recv(handle.path(), b"{\"cmd\":\"Ping\"}\n").await;
        assert_eq!(resp2["ok"], JsonValue::Bool(true));
        assert_eq!(resp2["data"], JsonValue::String("pong".into()));

        crate::shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn over_socket_malformed_json_does_not_kill_listener() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "resilience");
        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        // Garbage bytes.
        let resp1 = send_and_recv(handle.path(), b"not valid json\n").await;
        assert_eq!(resp1["ok"], JsonValue::Bool(false));
        // The wire-level NDJSON codec already flags "malformed request" —
        // whatever prefix string ark-core emits, the `ok: false` is what we
        // care about here.

        // Listener still serves.
        let resp2 = send_and_recv(handle.path(), b"{\"cmd\":\"Ping\"}\n").await;
        assert_eq!(resp2["ok"], JsonValue::Bool(true));

        crate::shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn over_socket_status_e2e() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "statuse2e");
        let status = sample_status(&id);
        write_status_atomic(&layout, &id, &status).unwrap();

        let (ctx, _cancel) = make_ctx(id.clone(), layout.clone());
        let handle =
            bind_and_connect(Arc::new(SupervisorCommandHandler::new(ctx)), &layout, &id).await;

        let resp = send_and_recv(handle.path(), b"{\"cmd\":\"Status\",\"args\":{}}\n").await;
        assert_eq!(resp["ok"], JsonValue::Bool(true));
        assert_eq!(resp["data"]["phase"], JsonValue::String("running".into()));

        crate::shutdown(handle).await.unwrap();
    }
}
