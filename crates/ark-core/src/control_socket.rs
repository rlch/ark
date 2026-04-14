//! Per-supervisor control socket primitive
//! (cavekit-hook-ipc.md R4, cavekit-supervisor.md R7).
//!
//! Wraps `interprocess::local_socket::tokio::Listener` and exposes a thin
//! bind/accept/serve API plus an NDJSON request/response envelope. One socket
//! per supervisor — no daemon, no shared listener (kakoune model).
//!
//! # Options applied
//! - `try_overwrite(true)` — unlinks a stale socket from a crashed prior
//!   supervisor on `AddrInUse`.
//! - `reclaim_name(true)` (default) — `Drop` unlinks the socket on normal
//!   exit.
//! - Socket file mode set to `0o600` via an explicit `chmod` after bind
//!   (portable across Linux + macOS; the builder-side `ListenerOptionsExt::mode`
//!   is not supported on macOS).
//!
//! # Cleanup caveats
//! `Drop` does not fire on SIGTERM/SIGKILL or panic-with-abort. The caller
//! MUST also install a signal handler that calls [`unlink_if_exists`] before
//! terminating (that concern lives in T-067).

use std::path::{Path, PathBuf};

use anyhow::Context;
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName};
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

/// NDJSON request/response envelope used by the control protocol.
///
/// One JSON object per line. Successful responses carry `data`; failures
/// carry `error`. Missing fields are elided from the wire form
/// (`skip_serializing_if = "Option::is_none"`).
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct Response<T: Serialize> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> Response<T> {
    /// Build a successful response with the given payload.
    pub fn ok(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    /// Build an error response with the given message.
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Owning wrapper around a Tokio-based local socket listener.
///
/// Constructed post-setsid + StateDir creation, before the supervisor signals
/// readiness to its parent CLI (see cavekit-supervisor.md R3 step 3).
pub struct ControlListener {
    path: PathBuf,
    inner: Listener,
}

impl ControlListener {
    /// Bind a new per-supervisor socket at `path`.
    ///
    /// Applies `try_overwrite(true)` + `reclaim_name(true)` via the
    /// `interprocess` builder, then explicitly `chmod 0o600`s the resulting
    /// socket file. Any failure at any stage is returned as an error with
    /// context; callers should treat bind failure as fatal (supervisor exits
    /// non-zero per cavekit-supervisor R7).
    pub async fn bind(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let name = path
            .as_os_str()
            .to_fs_name::<GenericFilePath>()
            .with_context(|| format!("construct socket name for {}", path.display()))?;

        let inner = ListenerOptions::new()
            .name(name)
            .reclaim_name(true)
            .try_overwrite(true)
            .create_tokio()
            .with_context(|| format!("bind control socket at {}", path.display()))?;

        // Tighten perms explicitly — the builder-side `mode()` hook is
        // Unix-ext-only and unsupported on macOS. An explicit chmod after
        // bind is portable.
        #[cfg(unix)]
        {
            use std::fs;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 on control socket {}", path.display()))?;
        }

        Ok(Self { path, inner })
    }

    /// Path the listener is bound to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept exactly one connection. Returns the stream for the caller to
    /// run its own request/response loop.
    pub async fn accept(&self) -> anyhow::Result<Stream> {
        self.inner
            .accept()
            .await
            .with_context(|| format!("accept on control socket {}", self.path.display()))
    }

    /// Run an accept loop, dispatching each connection to `handle`. Returns
    /// when `cancel` fires.
    ///
    /// Each connection is spawned onto a detached Tokio task, so one
    /// misbehaving client cannot stall the accept loop. Handler errors are
    /// logged at `warn` and do not propagate — the listener keeps serving.
    pub async fn serve<F, Fut>(
        self,
        handle: F,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<()>
    where
        F: Fn(Stream) -> Fut + Send + Sync + Clone + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(path = %self.path.display(), "control socket serve cancelled");
                    return Ok(());
                }
                accepted = self.inner.accept() => {
                    match accepted {
                        Ok(stream) => {
                            let h = handle.clone();
                            tokio::spawn(async move {
                                h(stream).await;
                            });
                        }
                        Err(err) => {
                            warn!(path = %self.path.display(), %err, "control socket accept failed");
                        }
                    }
                }
            }
        }
    }
}

impl Drop for ControlListener {
    fn drop(&mut self) {
        // The interprocess `reclaim_name(true)` option also does this on Drop
        // for its own bookkeeping; calling `unlink_if_exists` here is
        // idempotent and defensive.
        unlink_if_exists(&self.path);
    }
}

/// Unlink the given socket path if it exists. Idempotent.
///
/// Call this from signal handlers (SIGTERM/SIGINT/SIGABRT) since `Drop` does
/// not run on signal-triggered exit or panic-with-abort.
pub fn unlink_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(_) => debug!(?path, "control socket unlinked"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!(?path, %err, "control socket unlink failed"),
    }
}

/// Read one NDJSON request, dispatch to `handler`, write the response, flush,
/// close. Caller spawns this per connection in its serve loop.
///
/// The wire format is **exactly one** newline-terminated JSON object in each
/// direction. For multi-request-per-connection streaming, roll your own loop
/// — this helper is deliberately one-shot.
pub async fn handle_single_request<Req, Resp, F, Fut>(
    stream: Stream,
    handler: F,
) -> anyhow::Result<()>
where
    Req: DeserializeOwned,
    Resp: Serialize,
    F: FnOnce(Req) -> Fut,
    Fut: std::future::Future<Output = Response<Resp>>,
{
    let (read_half, write_half) = {
        use interprocess::local_socket::traits::tokio::Stream as _;
        stream.split()
    };
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .context("read NDJSON request line")?;
    if n == 0 {
        return Err(anyhow::anyhow!("peer closed before sending a request"));
    }

    // Strip trailing newline(s).
    while matches!(line.chars().last(), Some('\n' | '\r')) {
        line.pop();
    }

    let response: Response<Resp> = match serde_json::from_str::<Req>(&line) {
        Ok(req) => handler(req).await,
        Err(err) => Response::<Resp>::err(format!("malformed request: {err}")),
    };

    let mut body = serde_json::to_vec(&response).context("serialize response")?;
    body.push(b'\n');

    let mut write_half = write_half;
    write_half
        .write_all(&body)
        .await
        .context("write response")?;
    write_half.flush().await.context("flush response")?;
    // Dropping closes; explicit shutdown is a no-op for this impl but keeps
    // intent obvious.
    let _ = write_half.shutdown().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::traits::tokio::Stream as _;
    use serde::Deserialize;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;

    #[derive(Debug, Deserialize, Serialize)]
    struct Ping {
        cmd: String,
    }

    async fn connect_client(path: &Path) -> Stream {
        use interprocess::local_socket::ConnectOptions;
        let name = path.as_os_str().to_fs_name::<GenericFilePath>().unwrap();
        // Retry briefly — the serve loop may not have reached its first
        // accept poll at the moment we spawn a client.
        let mut last_err = None;
        for _ in 0..20 {
            match ConnectOptions::new()
                .name(name.clone())
                .connect_tokio()
                .await
            {
                Ok(s) => return s,
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        panic!("failed to connect: {:?}", last_err);
    }

    #[test]
    fn response_ok_omits_error_field() {
        let resp = Response::ok("pong");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"data\":\"pong\""));
        assert!(
            !json.contains("\"error\""),
            "error field should be elided, got {json}"
        );
    }

    #[test]
    fn response_err_omits_data_field() {
        let resp: Response<String> = Response::err("boom");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("\"error\":\"boom\""));
        assert!(
            !json.contains("\"data\""),
            "data field should be elided, got {json}"
        );
    }

    #[test]
    fn unlink_if_exists_is_idempotent_and_missing_safe() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.sock");
        // No-op on missing.
        unlink_if_exists(&path);
        unlink_if_exists(&path);

        // Now create and remove.
        std::fs::write(&path, b"").unwrap();
        assert!(path.exists());
        unlink_if_exists(&path);
        assert!(!path.exists());
        unlink_if_exists(&path); // still idempotent
    }

    #[tokio::test]
    async fn bind_accept_ndjson_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.sock");

        let listener = ControlListener::bind(&path).await.expect("bind");
        assert_eq!(listener.path(), path.as_path());
        assert!(path.exists(), "socket file should exist after bind");

        // Verify perms are 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "socket should be chmod 0600, got {mode:o}");
        }

        // Spawn a server task that accepts one connection and runs
        // handle_single_request with a ping handler.
        let server_path = path.clone();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.expect("accept");
            handle_single_request::<Ping, &str, _, _>(stream, |req| async move {
                assert_eq!(req.cmd, "Ping");
                Response::ok("pong")
            })
            .await
            .expect("handler");
            // Keep listener alive until the client finishes reading.
            drop(listener);
            server_path
        });

        // Client: connect, send ping, read response.
        let client = connect_client(&path).await;
        let (r, w) = client.split();
        let mut reader = BufReader::new(r);
        let mut w = w;
        w.write_all(b"{\"cmd\":\"Ping\"}\n").await.unwrap();
        w.flush().await.unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(resp_line.trim()).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(true));
        assert_eq!(v["data"], serde_json::Value::String("pong".into()));
        assert!(v.get("error").is_none());

        server.await.unwrap();
    }

    #[tokio::test]
    async fn bind_with_try_overwrite_replaces_stale_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("stale.sock");

        // Plant a dummy file at the target path (simulating a crashed
        // supervisor's leftover socket).
        std::fs::write(&path, b"leftover").unwrap();
        assert!(path.exists());

        let listener = ControlListener::bind(&path)
            .await
            .expect("bind should overwrite stale");
        assert!(path.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn drop_unlinks_socket_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("dropme.sock");
        {
            let _listener = ControlListener::bind(&path).await.expect("bind");
            assert!(path.exists());
        }
        // After drop, the socket file should be gone.
        assert!(!path.exists(), "Drop should unlink {}", path.display());
    }

    #[tokio::test]
    async fn handle_single_request_reports_malformed_json_as_error() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("bad.sock");
        let listener = ControlListener::bind(&path).await.unwrap();

        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            handle_single_request::<Ping, &str, _, _>(stream, |_| async move {
                Response::ok("should not reach")
            })
            .await
            .unwrap();
            drop(listener);
        });

        let client = connect_client(&path).await;
        let (r, w) = client.split();
        let mut reader = BufReader::new(r);
        let mut w = w;
        w.write_all(b"not valid json\n").await.unwrap();
        w.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["ok"], serde_json::Value::Bool(false));
        assert!(
            v["error"].as_str().unwrap().contains("malformed"),
            "error should mention malformed, got {line}"
        );

        server.await.unwrap();
    }

    #[tokio::test]
    async fn serve_dispatches_connections_until_cancelled() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("serve.sock");
        let listener = ControlListener::bind(&path).await.unwrap();

        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let path_for_serve = path.clone();
        let server = tokio::spawn(async move {
            listener
                .serve(
                    move |stream| {
                        let counter = counter_clone.clone();
                        async move {
                            let _ = handle_single_request::<Ping, &str, _, _>(stream, |_req| {
                                let counter = counter.clone();
                                async move {
                                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                    Response::ok("pong")
                                }
                            })
                            .await;
                        }
                    },
                    cancel_clone,
                )
                .await
                .unwrap();
            path_for_serve
        });

        // Send two sequential pings.
        for _ in 0..2 {
            let client = connect_client(&path).await;
            let (r, mut w) = client.split();
            w.write_all(b"{\"cmd\":\"Ping\"}\n").await.unwrap();
            w.flush().await.unwrap();
            let mut buf = Vec::new();
            let mut reader = BufReader::new(r);
            // Read until the server closes or we hit newline.
            let _ = reader.read_until(b'\n', &mut buf).await.unwrap();
            assert!(!buf.is_empty());
        }

        // Give spawned handler tasks a moment to increment the counter.
        for _ in 0..20 {
            if counter.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);

        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap();
    }
}
