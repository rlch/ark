//! Signal handling + socket cleanup (T-067).
//!
//! cavekit-supervisor.md R7 specifies that the per-agent control socket
//! must be unlinked on:
//!
//! - **Normal exit** — the `ControlListener` Drop guard (via
//!   `reclaim_name(true)`) unlinks the socket file. No further work here.
//! - **SIGTERM / SIGINT** — Drop does *not* fire; this module installs a
//!   `signal_hook_tokio` handler that explicitly `remove_file`s the
//!   socket path and then fires the supervisor's cancel token so the
//!   tokio runtime unwinds.
//! - **SIGKILL / hard crash** — uncatchable; the socket file is left
//!   stale and is GC'd on the next picker/CLI scan
//!   (cavekit-hook-ipc.md R4).
//!
//! # SIGABRT / `panic = "abort"`
//!
//! Out of scope for T-067. Drop does not run on panic-with-abort, so a
//! separate `signal_hook::low_level::register` for `SIGABRT` + `remove_file`
//! would be needed to cover that path. Leave the socket to the next
//! picker-GC scan for now.

use std::path::PathBuf;

use anyhow::Context;
use ark_core::control_socket::unlink_if_exists;
use futures::stream::StreamExt;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook_tokio::Signals;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::control_socket::ControlSocketHandle;

/// Handle returned by [`install_signal_handlers`].
///
/// Dropping (or calling [`SignalTaskHandle::abort`]) stops the background
/// signal task. [`SignalTaskHandle::join`] awaits its completion — useful
/// for the supervisor's shutdown protocol.
pub struct SignalTaskHandle {
    // `Option` so [`SignalTaskHandle::join`] can move the handle out while
    // the wrapper keeps its Drop guarantee for the "abandon" path.
    task: Option<JoinHandle<()>>,
}

impl SignalTaskHandle {
    /// Stop the signal task. Idempotent.
    pub fn abort(&self) {
        if let Some(t) = &self.task {
            t.abort();
        }
    }

    /// Await the signal task's completion.
    pub async fn join(mut self) -> Result<(), tokio::task::JoinError> {
        match self.task.take() {
            Some(t) => t.await,
            None => Ok(()),
        }
    }

    /// True if the background task has finished (signalled or aborted).
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().map(|t| t.is_finished()).unwrap_or(true)
    }
}

impl Drop for SignalTaskHandle {
    fn drop(&mut self) {
        if let Some(t) = &self.task {
            if !t.is_finished() {
                t.abort();
            }
        }
    }
}

/// Install a `signal_hook_tokio` handler covering `SIGTERM` and `SIGINT`.
///
/// On any incoming signal the handler:
///
/// 1. `std::fs::remove_file`s `socket_path` (swallowing `NotFound`).
/// 2. Cancels `cancel` so the tokio accept loop (and anything else tied
///    to that token) unwinds.
///
/// The returned [`SignalTaskHandle`] must outlive the control socket
/// listener (otherwise the handler disappears before any signal can
/// arrive). The supervisor's exit path should `shutdown` the socket
/// *first* (step 1 in cavekit-supervisor.md R3 step 17) and then drop /
/// abort the signal handle (step 2). That order matters: if you drop the
/// signal handle while the socket is still bound, the only remaining
/// cleanup path is the Drop guard inside the listener — fine for clean
/// shutdown but leaves no coverage for signals.
pub async fn install_signal_handlers(
    socket_path: PathBuf,
    cancel: CancellationToken,
) -> anyhow::Result<SignalTaskHandle> {
    let signals = Signals::new([SIGTERM, SIGINT]).context("register SIGTERM/SIGINT handler")?;
    debug!(
        path = %socket_path.display(),
        "signal handler installed for SIGTERM + SIGINT"
    );
    let task = tokio::spawn(run_signal_loop(signals, socket_path, cancel));
    Ok(SignalTaskHandle { task: Some(task) })
}

async fn run_signal_loop(mut signals: Signals, socket_path: PathBuf, cancel: CancellationToken) {
    // First signal triggers cleanup + cancel; subsequent signals are a
    // no-op (idempotent).
    if let Some(sig) = signals.next().await {
        debug!(signal = sig, path = %socket_path.display(), "signal received; unlinking socket");
        unlink_if_exists(&socket_path);
        cancel.cancel();
    } else {
        warn!("signal stream closed before any signal arrived");
    }
}

/// RAII guard that owns a live [`ControlSocketHandle`] and guarantees the
/// socket file is unlinked when the guard is dropped.
///
/// Drop cannot `await` — so `shutdown` of the async accept loop is done
/// on a best-effort basis:
///
/// - If a tokio runtime is active on the current thread
///   ([`tokio::runtime::Handle::try_current`] succeeds), we spawn the
///   async shutdown onto a detached task there. The socket file is also
///   unlinked synchronously so the filesystem state is clean even if
///   the runtime is already stopping.
/// - Otherwise we fall back to a synchronous `remove_file` — sufficient
///   for the cavekit-supervisor R7 "clean up on normal exit" contract.
///
/// The primary, preferred teardown path is an explicit async call:
///
/// ```ignore
/// let guard = ControlSocketGuard::new(handle);
/// // ... supervisor work ...
/// guard.shutdown().await?;   // graceful, awaits accept loop drain
/// ```
pub struct ControlSocketGuard {
    // Option so Drop can take ownership.
    inner: Option<ControlSocketHandle>,
    path: PathBuf,
}

impl ControlSocketGuard {
    /// Wrap the given handle. The socket path is cached so Drop can
    /// unlink without touching the inner handle.
    pub fn new(handle: ControlSocketHandle) -> Self {
        let path = handle.path().to_path_buf();
        Self {
            inner: Some(handle),
            path,
        }
    }

    /// Path of the wrapped socket.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Graceful async shutdown: cancel accept loop, drain the join set,
    /// unlink the socket file. Consumes the guard.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(handle) = self.inner.take() {
            crate::control_socket::shutdown(handle).await?;
        }
        unlink_if_exists(&self.path);
        Ok(())
    }
}

impl Drop for ControlSocketGuard {
    fn drop(&mut self) {
        // Best-effort sync cleanup. The ControlListener's reclaim_name
        // Drop guard will also unlink the file when its task finishes,
        // but we don't own that task here — we can't await.
        if let Some(handle) = self.inner.take() {
            if let Ok(handle_rt) = tokio::runtime::Handle::try_current() {
                // Inside a runtime: spawn the async shutdown and let the
                // runtime drive it. We do NOT block on it (blocking from
                // Drop inside a tokio worker would deadlock).
                handle_rt.spawn(async move {
                    if let Err(err) = crate::control_socket::shutdown(handle).await {
                        warn!(%err, "control socket shutdown during Drop failed");
                    }
                });
            } else {
                // No runtime available — drop the handle (cancels the
                // token; the accept loop task will never get polled
                // again in this thread anyway) and fall through to the
                // synchronous unlink below.
                drop(handle);
            }
        }
        unlink_if_exists(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use signal_hook::low_level::raise;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn short_tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("sig")
            .tempdir_in("/tmp")
            .expect("short tempdir under /tmp")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sigterm_unlinks_socket_and_cancels() {
        // Pre-create a file at the target path so unlink has something to
        // reclaim.
        let tmp = short_tempdir();
        let sock = tmp.path().join("s.sock");
        std::fs::write(&sock, b"").unwrap();
        assert!(sock.exists());

        let cancel = CancellationToken::new();
        let handle = install_signal_handlers(sock.clone(), cancel.clone())
            .await
            .expect("install");

        // Fire SIGTERM at ourselves. `signal_hook_tokio`'s Signals
        // instance registers the handler with the process-wide
        // `signal_hook` infrastructure — raising in-process is the
        // canonical test path (same pattern used by signal-hook-tokio's
        // own integration tests).
        raise(SIGTERM).unwrap();

        // Wait (bounded) for the handler to finish.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(cancel.is_cancelled(), "cancel must fire on SIGTERM");
        assert!(!sock.exists(), "socket must be unlinked on SIGTERM");

        // Let the task complete.
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlink_is_idempotent_when_socket_already_gone() {
        let tmp = short_tempdir();
        let sock = tmp.path().join("s2.sock");
        // Intentionally no pre-create — file is already "gone".
        assert!(!sock.exists());

        let cancel = CancellationToken::new();
        let handle = install_signal_handlers(sock.clone(), cancel.clone())
            .await
            .expect("install");

        raise(SIGINT).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            cancel.is_cancelled(),
            "cancel must fire even when socket absent"
        );
        // No panic expected; nothing to unlink.
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.join()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_stops_the_signal_task_without_cancel() {
        let tmp = short_tempdir();
        let sock = tmp.path().join("s3.sock");
        std::fs::write(&sock, b"").unwrap();

        let cancel = CancellationToken::new();
        let handle = install_signal_handlers(sock.clone(), cancel.clone())
            .await
            .expect("install");
        assert!(!handle.is_finished());

        handle.abort();
        // Give the task a moment to observe the abort.
        for _ in 0..20 {
            if handle.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(handle.is_finished(), "aborted task should finish");
        // Abort does NOT unlink or cancel — that only happens when a
        // signal fires. Verify the token stayed pristine.
        assert!(!cancel.is_cancelled());
        // Drain the handle so the spawned task's JoinError (from abort)
        // is observed and doesn't show up as an unhandled panic.
        let _ = handle.join().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_socket_guard_shutdown_unlinks_socket() {
        let tmp = short_tempdir();
        let layout = ark_types::StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = ark_types::SessionId::new("guard");
        let handle = crate::bind_control_socket(&layout, &id, Arc::new(crate::NoopHandler))
            .await
            .expect("bind");
        let path = handle.path().to_path_buf();
        assert!(path.exists());

        let guard = ControlSocketGuard::new(handle);
        assert_eq!(guard.path(), path.as_path());
        guard.shutdown().await.unwrap();

        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_socket_guard_drop_cleans_up_best_effort() {
        let tmp = short_tempdir();
        let layout = ark_types::StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = ark_types::SessionId::new("guarddrop");
        let handle = crate::bind_control_socket(&layout, &id, Arc::new(crate::NoopHandler))
            .await
            .expect("bind");
        let path = handle.path().to_path_buf();

        {
            let _guard = ControlSocketGuard::new(handle);
            // Scope-drop fires the Drop impl: spawns the async shutdown
            // on the current runtime.
        }

        // Let the spawned shutdown task run. We can't join it directly
        // (it's detached), so poll the filesystem for up to a short
        // window.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while path.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            !path.exists(),
            "Drop should eventually unlink {}",
            path.display()
        );
    }
}
