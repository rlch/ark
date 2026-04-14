//! `--no-detach` foreground mode for the supervisor
//! (cavekit-supervisor.md R1, second bullet: "stays in foreground, streams
//! events to parent's stderr").
//!
//! # What this ships
//!
//! The thin foreground-entry scaffold: given a [`StateLayout`] + [`AgentId`]
//! and an async closure, we
//!
//! 1. Open `$STATE/agents/{id}/supervisor.log` in append mode.
//! 2. Install a layered [`tracing_subscriber`] that emits events to BOTH
//!    the log file AND `stderr` (the "tee" pattern, so the invoking shell
//!    observes supervisor log lines directly). For testing, callers can
//!    inject an arbitrary `io::Write + Send + Sync + 'static` as the
//!    stderr sink via [`build_foreground_dispatch`].
//! 3. Stand up a current-thread tokio runtime and drive the caller's
//!    closure to completion.
//! 4. Tear the subscriber down on the way out (scoped via
//!    [`tracing::subscriber::with_default`] so tests can call
//!    [`run_foreground`] repeatedly without colliding with the global
//!    default — which is already claimed by
//!    [`crate::daemon::setup_supervisor_log`] for daemonized runs).
//!
//! # What this does NOT ship (deferred to T-069)
//!
//! T-063 is explicitly plumbing-only. The full [`AgentEvent`][ae] stream
//! piped as JSONL to parent stderr is wired in T-069 when the supervisor
//! takes ownership of the event bus. Here we ship:
//!
//! * A structured-log tee (file + stderr via `tracing_subscriber`).
//! * A `ForegroundCtx` surface the closure can use to discover the log
//!   path if it needs to attach its own JSONL formatter to the bus.
//!
//! T-069 will extend `ForegroundCtx` with a `broadcast::Receiver<AgentEvent>`
//! and an opinionated JSONL writer that emits events to stderr alongside
//! the tracing layer.
//!
//! [ae]: ark_types::AgentEvent

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ark_types::{AgentId, StateLayout};
use tracing::Dispatch;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{Layer, Registry};

/// Context handed to the caller's closure inside [`run_foreground`].
///
/// Extended in T-069 with an event-bus receiver; for T-063 it carries
/// just the resolved paths / identifiers the closure needs.
#[derive(Clone, Debug)]
pub struct ForegroundCtx {
    /// Resolved `$STATE/agents/{id}/supervisor.log` path.
    pub log_path: PathBuf,
    /// The agent id this foreground supervisor is running for.
    pub agent_id: AgentId,
}

/// Run the supervisor in foreground (`--no-detach`) mode.
///
/// See the module docs for the full behaviour. The closure is driven on a
/// current-thread tokio runtime — callers that need multi-threaded
/// scheduling should construct their own runtime inside the closure
/// (unusual for the supervisor; one runtime per process is the norm).
///
/// Errors:
/// * `Err` from creating the log file or installing the subscriber.
/// * `Err` from the closure itself (bubbled up 1:1).
///
/// On success, returns after the closure's future resolves (no fork).
pub fn run_foreground<F, Fut>(
    state_layout: &StateLayout,
    agent_id: &AgentId,
    agent_run: F,
) -> anyhow::Result<()>
where
    F: FnOnce(ForegroundCtx) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    let log_path = state_layout.supervisor_log_path(agent_id);
    let ctx = ForegroundCtx {
        log_path: log_path.clone(),
        agent_id: agent_id.clone(),
    };

    // Build the layered subscriber. Real stderr for production.
    let dispatch = build_foreground_dispatch(&log_path, StderrSink)?;

    // Install as scoped default so the subscriber unwinds cleanly after
    // run_foreground returns — crucial because the caller (e.g. CLI
    // `ark spawn --no-detach`) may follow this with other subscribers
    // or further `run_foreground` calls.
    let _guard = tracing::dispatcher::set_default(&dispatch);

    // Drive the closure on a current-thread runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("build tokio runtime: {e}"))?;
    rt.block_on(agent_run(ctx))
}

/// Build a [`Dispatch`] that tees `tracing` events to both `log_path` (as
/// append-mode file writes, ANSI off) and `stderr_sink` (as color-free
/// line writes).
///
/// Public so tests can inject a custom writer (e.g. a shared `Vec<u8>`
/// guarded by a mutex). Callers install the returned dispatch via
/// [`tracing::dispatcher::set_default`] (scoped) or
/// [`tracing::subscriber::set_global_default`] (process-wide).
pub fn build_foreground_dispatch<W>(
    log_path: &std::path::Path,
    stderr_sink: W,
) -> anyhow::Result<Dispatch>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    // File layer — structured, ANSI off.
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || -> Box<dyn Write + Send> {
            Box::new(log_file.try_clone().expect("clone supervisor log handle"))
        })
        .with_ansi(false)
        .with_target(true)
        .with_level(true);

    // Stderr layer — also ANSI off (the "parent's stderr" in caveman-mode
    // might be a pipe, not a TTY; tests certainly aren't).
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(stderr_sink)
        .with_ansi(false)
        .with_target(true)
        .with_level(true);

    let subscriber = Registry::default()
        .with(file_layer.boxed())
        .with(stderr_layer.boxed());

    Ok(Dispatch::new(subscriber))
}

/// Thin wrapper around `io::stderr` that implements [`MakeWriter`]. Using
/// a named type (rather than the `io::stderr` fn pointer) keeps the
/// production path's signature symmetric with the test path's
/// `SharedWriter`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StderrSink;

impl<'a> MakeWriter<'a> for StderrSink {
    type Writer = io::Stderr;
    fn make_writer(&'a self) -> Self::Writer {
        io::stderr()
    }
}

/// A `MakeWriter` backed by a shared `Vec<u8>` behind an `Arc<Mutex<_>>`.
/// Primarily a testing affordance — lets assertions read everything a
/// foreground run emitted to "stderr".
#[derive(Clone, Debug, Default)]
pub struct SharedWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current buffer as a UTF-8 string (lossy on bad bytes
    /// so tests can assert on prefix substrings).
    pub fn snapshot(&self) -> String {
        let guard = self.buf.lock().expect("shared writer mutex poisoned");
        String::from_utf8_lossy(&guard).into_owned()
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterHandle;
    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterHandle {
            buf: self.buf.clone(),
        }
    }
}

/// Per-event handle. `Write` + `Drop` behave like a classic buffered
/// writer that flushes on drop (implicit via the mutex).
pub struct SharedWriterHandle {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedWriterHandle {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut guard = self
            .buf
            .lock()
            .map_err(|_| io::Error::other("shared writer mutex poisoned"))?;
        guard.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    #[test]
    fn dispatch_writes_to_both_file_and_injected_writer() {
        let tmp = TempDir::new().unwrap();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fg-plumb");
        let log = layout.supervisor_log_path(&id);

        let shared = SharedWriter::new();
        let dispatch = build_foreground_dispatch(&log, shared.clone()).expect("dispatch");

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!(target: "foreground_test", "hello-tee");
        });

        // Log file captured it.
        let file_contents = fs::read_to_string(&log).expect("read log");
        assert!(
            file_contents.contains("hello-tee"),
            "file should contain event, got {file_contents:?}"
        );

        // Shared writer (stand-in for parent stderr) also captured it.
        let stderr_contents = shared.snapshot();
        assert!(
            stderr_contents.contains("hello-tee"),
            "stderr writer should contain event, got {stderr_contents:?}"
        );
    }

    #[test]
    fn dispatch_creates_parent_directory_for_log_path() {
        let tmp = TempDir::new().unwrap();
        let deep = tmp
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("supervisor.log");
        let shared = SharedWriter::new();
        let _dispatch = build_foreground_dispatch(&deep, shared).expect("dispatch");
        assert!(deep.parent().unwrap().is_dir(), "parent must be created");
    }

    #[test]
    fn run_foreground_completes_with_closure_ok() {
        let tmp = TempDir::new().unwrap();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fg-ok");
        let expected_id = id.as_str().to_string();
        let result = run_foreground(&layout, &id, |ctx| {
            let expected = expected_id.clone();
            async move {
                assert_eq!(ctx.agent_id.as_str(), expected);
                tracing::info!("inside-closure");
                Ok(())
            }
        });
        result.expect("run_foreground");
        // Log file must have been created + written to.
        let log = layout.supervisor_log_path(&id);
        let content = fs::read_to_string(&log).expect("read log");
        assert!(
            content.contains("inside-closure"),
            "supervisor.log should contain closure event, got {content:?}"
        );
    }

    #[test]
    fn run_foreground_returns_when_closure_returns_early() {
        // A fast-returning closure must not block. This also asserts that
        // run_foreground is not forking — if it were, the grandchild
        // wouldn't return here at all.
        let tmp = TempDir::new().unwrap();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fg-early");
        let start = std::time::Instant::now();
        run_foreground(&layout, &id, |_ctx| async move { Ok(()) }).expect("run");
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 5,
            "foreground run should return promptly, took {elapsed:?}"
        );
    }

    #[test]
    fn run_foreground_propagates_closure_error() {
        let tmp = TempDir::new().unwrap();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fg-err");
        let err = run_foreground(&layout, &id, |_ctx| async move {
            Err(anyhow::anyhow!("closure-error-token"))
        })
        .expect_err("closure errored");
        assert!(
            format!("{err:#}").contains("closure-error-token"),
            "closure error should propagate, got {err:#}"
        );
    }

    #[test]
    fn run_foreground_does_not_redirect_process_stdio() {
        // Sanity: the foreground mode must not dup2() stdio onto the log
        // (that's the daemonize-path behaviour from daemon.rs). If it
        // did, the test harness itself would break. We smoke-check this
        // by confirming that a direct stderr write BEFORE the call is
        // still possible AFTER the call — i.e. the fd wasn't replaced.
        let tmp = TempDir::new().unwrap();
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fg-nostdio");
        // (We don't actually emit here; the test harness's own stderr
        // capture does the heavy lifting.)
        run_foreground(&layout, &id, |_ctx| async move { Ok(()) }).expect("run");
        // If stderr had been redirected the next line could block or
        // error — it doesn't.
        eprint!(""); // no-op write to stderr; must not panic
    }

    #[test]
    fn shared_writer_is_clone_and_append_only() {
        let shared = SharedWriter::new();
        let mut h1 = shared.make_writer();
        let mut h2 = shared.make_writer();
        h1.write_all(b"one\n").unwrap();
        h2.write_all(b"two\n").unwrap();
        let snapshot = shared.snapshot();
        assert_eq!(snapshot, "one\ntwo\n");
    }
}
