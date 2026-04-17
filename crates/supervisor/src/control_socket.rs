//! Per-supervisor control socket bind + serve loop
//! (cavekit-supervisor.md R7, cavekit-hook-ipc.md R4).
//!
//! This module is intentionally narrow: it reuses the
//! [`ark_core::control_socket::ControlListener`] primitive and the
//! NDJSON request/response codec from `ark-core`. What we add on top is:
//!
//! 1. **Path resolution** — the per-agent `.sock` path is resolved via
//!    [`ark_types::StateLayout::agent_socket_path`] + the runtime dir is
//!    tightened to mode `0700` via [`ark_core::socket_paths::ensure_sessions_dir`].
//! 2. **Bind + accept loop** — spawns the accept loop on an internal
//!    [`tokio::task::JoinSet`] and dispatches each inbound connection to a
//!    pluggable [`ControlCommandHandler`].
//! 3. **Explicit shutdown** — [`shutdown`] cancels the accept loop, drains
//!    the JoinSet, and unlinks the socket path (belt and suspenders with
//!    the `Drop` guard on `ControlListener` which `reclaim_name` already
//!    fires on normal exit).
//!
//! Per-command handlers (Status, Kill, Rename, Forget, Ping) live in
//! T-066. This module ships a [`NoopHandler`] so tests (and partial
//! integrations) can drive the accept loop end-to-end.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use ark_core::control_socket::{
    ControlListener, Response, handle_single_request, unlink_if_exists,
};
use ark_core::socket_paths::ensure_sessions_dir;
use ark_types::{SessionId, StateLayout};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Pluggable handler for inbound control-socket commands.
///
/// Each inbound connection reads exactly one NDJSON request, calls
/// [`ControlCommandHandler::handle`] with the parsed JSON value, and writes
/// the returned JSON value back as a newline-terminated response. The
/// handler is responsible for embedding the full success/error shape
/// (typically via [`ark_core::Response`]).
///
/// T-066 wires this trait to the supervisor's concrete command set; T-065
/// only wires the plumbing + ships [`NoopHandler`] for exercise / tests.
pub trait ControlCommandHandler: Send + Sync {
    fn handle(
        &self,
        req: serde_json::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send + '_>>;
}

/// Default handler that responds `{"ok": true, "data": "noop"}` to every
/// request. Used by tests and as a stand-in before T-066 lands the real
/// command set.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopHandler;

impl ControlCommandHandler for NoopHandler {
    fn handle(
        &self,
        _req: serde_json::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send + '_>> {
        Box::pin(async move {
            serde_json::to_value(Response::ok("noop")).expect("serialize noop response")
        })
    }
}

/// Owning handle to a live control socket + its accept loop.
///
/// Dropping the handle WITHOUT calling [`shutdown`] will cancel the accept
/// loop (via the cancel token's own Drop) but the JoinSet drain and
/// explicit socket-file unlink only run inside [`shutdown`]. The
/// `ControlListener`'s `reclaim_name(true)` Drop guard also fires on
/// normal exit and unlinks the file, so the leak on an abandoned handle
/// is bounded.
pub struct ControlSocketHandle {
    /// Resolved socket path (`$runtime/agents/{id}.sock`).
    pub path: PathBuf,
    joinset: JoinSet<anyhow::Result<()>>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for ControlSocketHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlSocketHandle")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ControlSocketHandle {
    /// Resolved socket path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Cancellation token shared with the accept loop; useful for callers
    /// that want to tie cancellation to an external signal source.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Bind the per-agent control socket and spawn its accept loop.
///
/// Bind flow (cavekit-supervisor R7):
///
/// 1. Resolve path via [`StateLayout::agent_socket_path`].
/// 2. Ensure `$runtime/agents/` exists at mode `0700` via
///    [`ensure_sessions_dir`].
/// 3. Bind a [`ControlListener`] with `try_overwrite(true)` +
///    `reclaim_name(true)` + `mode(0o600)` on the socket file.
/// 4. Spawn the accept loop onto the returned [`JoinSet`]; each inbound
///    connection is another JoinSet child running a single-request NDJSON
///    handler.
/// 5. Bind failure is fatal — the returned `Err` propagates to the
///    caller, which exits the supervisor non-zero (cavekit-supervisor
///    R7 "bind failure is fatal").
///
/// The caller MUST be running inside a tokio runtime; `interprocess`'s
/// tokio listener panics otherwise (cavekit-supervisor R7 "panic if used
/// outside a Tokio runtime context").
pub async fn bind_control_socket(
    state_layout: &StateLayout,
    agent_id: &SessionId,
    handler: Arc<dyn ControlCommandHandler>,
) -> anyhow::Result<ControlSocketHandle> {
    // Resolve path + ensure parent dir is 0700.
    let runtime_root = state_layout.runtime();
    ensure_sessions_dir(runtime_root).with_context(|| {
        format!(
            "ensure sessions dir at {} (mode 0700)",
            runtime_root.display()
        )
    })?;
    let path = state_layout.session_socket_path(agent_id);

    // Bind listener. Any failure here is fatal — surface as Err.
    let listener = ControlListener::bind(&path)
        .await
        .with_context(|| format!("bind control socket at {}", path.display()))?;
    debug!(path = %path.display(), agent = %agent_id.as_str(), "control socket bound");

    let cancel = CancellationToken::new();
    let mut joinset: JoinSet<anyhow::Result<()>> = JoinSet::new();

    // Spawn the accept loop as a JoinSet child. Each accepted connection
    // becomes another spawned task inside `ControlListener::serve`; this
    // wrapper just threads our handler through.
    let accept_cancel = cancel.clone();
    let accept_path = path.clone();
    joinset.spawn(async move {
        let handler = handler.clone();
        listener
            .serve(
                move |stream| {
                    let handler = handler.clone();
                    let accept_path = accept_path.clone();
                    async move {
                        if let Err(err) = handle_single_request::<
                            serde_json::Value,
                            serde_json::Value,
                            _,
                            _,
                        >(stream, |req| {
                            let handler = handler.clone();
                            async move {
                                // Run the pluggable handler. We expect it
                                // to return a JSON-encoded Response (shape:
                                // {"ok": bool, "data"?, "error"?}). If the
                                // handler returns something that isn't
                                // already in Response shape we still pass
                                // it through — the NDJSON codec doesn't
                                // force a specific envelope here because
                                // the bytes-in / bytes-out contract is
                                // already satisfied.
                                let data = handler.handle(req).await;
                                // Re-wrap as Response<Value> so the
                                // serialised form always carries `ok`.
                                // If the handler ALREADY produced a
                                // Response-shaped object (has "ok"), pass
                                // through unchanged.
                                wrap_as_response(data)
                            }
                        })
                        .await
                        {
                            warn!(path = %accept_path.display(), %err, "control socket handler failed");
                        }
                    }
                },
                accept_cancel,
            )
            .await
    });

    Ok(ControlSocketHandle {
        path,
        joinset,
        cancel,
    })
}

/// If the handler already returned a `Response`-shaped object (has `ok`
/// field) we pass it through; otherwise we wrap as a successful
/// `Response::ok(value)`. This lets handlers either (a) build a
/// [`Response`] explicitly or (b) return a raw data payload.
fn wrap_as_response(value: serde_json::Value) -> Response<serde_json::Value> {
    if let Some(obj) = value.as_object() {
        if obj.contains_key("ok") {
            // Already shaped — best-effort deserialize. If decoding fails
            // (e.g. "ok" isn't bool), fall back to wrapping the raw JSON.
            if let Ok(resp) = serde_json::from_value::<Response<serde_json::Value>>(value.clone()) {
                return resp;
            }
        }
    }
    Response::ok(value)
}

/// Gracefully tear down a control socket.
///
/// 1. Cancels the accept loop's `CancellationToken`.
/// 2. Drains the `JoinSet`, waiting for the accept loop task (and any
///    in-flight connection handlers it spawned) to complete.
/// 3. Explicitly unlinks the socket file — belt-and-suspenders on top of
///    `ControlListener`'s `reclaim_name(true)` Drop guard, since the
///    [`ControlListener`] is owned by the spawned accept loop task and
///    only drops when that task finishes.
pub async fn shutdown(mut handle: ControlSocketHandle) -> anyhow::Result<()> {
    handle.cancel.cancel();
    // Drain every task; surface the FIRST error (if any) but finish
    // draining before returning so no task is leaked.
    let mut first_err: Option<anyhow::Error> = None;
    while let Some(joined) = handle.joinset.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if first_err.is_none() {
                    first_err = Some(err);
                }
            }
            Err(join_err) => {
                if first_err.is_none() {
                    first_err = Some(anyhow::Error::new(join_err));
                }
            }
        }
    }
    // Explicit unlink (Drop on ControlListener already does this when the
    // accept loop task finishes, but we call again for clarity — it's
    // idempotent).
    unlink_if_exists(&handle.path);

    match first_err {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::SessionId;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use interprocess::local_socket::{ConnectOptions, GenericFilePath, ToFsName};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    /// Allocate a short-path tempdir under `/tmp` — on macOS
    /// `$TMPDIR` resolves to `/var/folders/...` which is long enough that
    /// the rendered socket path (tmp + `agents/` + full agent-id +
    /// `.sock`) regularly exceeds the 104-byte `sun_path` cap. Using
    /// `/tmp` directly keeps us well under the cap.
    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("sv")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    async fn connect_retry(path: &std::path::Path) -> interprocess::local_socket::tokio::Stream {
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

    async fn send_line_and_read_reply(
        stream: interprocess::local_socket::tokio::Stream,
        line: &[u8],
    ) -> String {
        let (r, w) = stream.split();
        let mut reader = BufReader::new(r);
        let mut w = w;
        w.write_all(line).await.unwrap();
        w.flush().await.unwrap();
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn bind_creates_socket_with_correct_modes() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("bindmode");

        let handle = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect("bind");
        assert!(handle.path().exists(), "socket file should exist");

        // Parent dir mode 0700.
        let parent_mode = std::fs::metadata(handle.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700, "parent dir should be 0700");

        // Socket file mode 0600.
        let sock_mode = std::fs::metadata(handle.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(sock_mode, 0o600, "socket file should be 0600");

        shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn noop_handler_responds_to_any_request() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("noop");

        let handle = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect("bind");

        let client = connect_retry(handle.path()).await;
        let resp = send_line_and_read_reply(client, b"{\"cmd\":\"Anything\"}\n").await;
        let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(true));
        assert_eq!(v["data"], serde_json::Value::String("noop".into()));

        shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn malformed_request_yields_error_response_and_next_request_succeeds() {
        // Per cavekit-hook-ipc R4 and cavekit-supervisor R7: malformed
        // requests get a `{"ok": false, "error": ...}` response and the
        // LISTENER keeps serving. Per-connection is one-shot (single
        // request/response), so we verify two sequential connections: the
        // first carries garbage, the second a valid request.
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("malformed");

        let handle = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect("bind");

        // First connection: malformed body.
        let c1 = connect_retry(handle.path()).await;
        let r1 = send_line_and_read_reply(c1, b"not valid json\n").await;
        let v1: serde_json::Value = serde_json::from_str(r1.trim()).unwrap();
        assert_eq!(v1["ok"], serde_json::Value::Bool(false));
        assert!(
            v1["error"].as_str().unwrap().contains("malformed"),
            "should report malformed, got {r1}"
        );

        // Second connection: valid request — listener must still be
        // accepting.
        let c2 = connect_retry(handle.path()).await;
        let r2 = send_line_and_read_reply(c2, b"{\"cmd\":\"Ping\"}\n").await;
        let v2: serde_json::Value = serde_json::from_str(r2.trim()).unwrap();
        assert_eq!(v2["ok"], serde_json::Value::Bool(true));

        shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_cancels_drains_and_unlinks_socket_file() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("shutdown");

        let handle = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect("bind");
        let path = handle.path().to_path_buf();
        assert!(path.exists());

        shutdown(handle).await.unwrap();

        // File must be gone (cavekit-supervisor R7 "explicit unlink" on
        // shutdown path).
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bind_succeeds_over_stale_socket_file() {
        // Simulate a crashed prior supervisor: a leftover regular file at
        // the agent socket path. try_overwrite(true) must replace it.
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("stale");

        // Pre-create runtime dir + plant a stale file at the target.
        let agents = ensure_sessions_dir(layout.runtime()).unwrap();
        let stale_path = agents.join(format!("{}.sock", id.as_str()));
        std::fs::write(&stale_path, b"leftover").unwrap();
        assert!(stale_path.exists());

        let handle = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect("bind should overwrite stale file");
        assert_eq!(handle.path(), stale_path.as_path());

        // Sanity: can still drive a request through.
        let client = connect_retry(handle.path()).await;
        let resp = send_line_and_read_reply(client, b"{\"cmd\":\"Ping\"}\n").await;
        let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(true));

        shutdown(handle).await.unwrap();
    }

    #[tokio::test]
    async fn bind_fails_when_parent_path_is_occupied_by_a_file() {
        // Can't create $runtime/agents/ because $runtime itself is a
        // regular file. Bind must surface an error (cavekit-supervisor
        // R7 "bind failure is fatal").
        let tmp = short_tempdir();
        // Create a regular file at the path where the runtime dir would
        // live, so `create_dir_all` fails.
        let rt_as_file = tmp.path().join("rt");
        std::fs::write(&rt_as_file, b"").unwrap();
        let layout = StateLayout::new(tmp.path().join("state"), rt_as_file, tmp.path().join("cfg"));
        let id = SessionId::new("badparent");

        let err = bind_control_socket(&layout, &id, Arc::new(NoopHandler))
            .await
            .expect_err("bind should fail when parent dir is unreachable");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ensure sessions dir") || msg.contains("bind control socket"),
            "error should mention bind/ensure, got {msg}"
        );
    }

    // Custom handler exercising the handler trait end-to-end with a
    // non-trivial JSON shape.
    struct EchoHandler;
    impl ControlCommandHandler for EchoHandler {
        fn handle(
            &self,
            req: serde_json::Value,
        ) -> Pin<Box<dyn std::future::Future<Output = serde_json::Value> + Send + '_>> {
            Box::pin(async move {
                // Return an already-Response-shaped object so
                // wrap_as_response passes it through unchanged.
                serde_json::json!({ "ok": true, "data": { "echo": req } })
            })
        }
    }

    #[tokio::test]
    async fn custom_handler_replaces_noop() {
        let tmp = short_tempdir();
        let layout = layout_at(tmp.path());
        let id = SessionId::new("echo");

        let handle = bind_control_socket(&layout, &id, Arc::new(EchoHandler))
            .await
            .expect("bind");
        let client = connect_retry(handle.path()).await;
        let resp = send_line_and_read_reply(client, b"{\"cmd\":\"Status\",\"args\":{}}\n").await;
        let v: serde_json::Value = serde_json::from_str(resp.trim()).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(true));
        assert_eq!(
            v["data"]["echo"]["cmd"],
            serde_json::Value::String("Status".into())
        );

        shutdown(handle).await.unwrap();
    }
}
