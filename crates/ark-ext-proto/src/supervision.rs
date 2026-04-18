//! Subprocess extension supervision (T-9.5.7).
//!
//! Wraps a `tokio::process::Child` running an extension binary and
//! provides:
//!
//! * A graceful shutdown ladder per `cavekit-scene.md` R16 — drop stdin
//!   first, wait `stdin_close_grace`, then SIGTERM, wait `sigterm_grace`,
//!   then SIGKILL.
//! * A bounded stderr log-tail buffer that captures the last
//!   `LOG_TAIL_LINES` of the child's stderr so crash diagnostics can
//!   include "what was the extension printing right before it died?"
//!   (R16 "Crash → log `error[ext/crashed]` + emit
//!   `UserEvent:ark.ext.crashed { name, exit_code, stderr_tail }`").
//! * A typed [`CrashReport`] payload the consumer (the supervisor crate)
//!   maps to `AgentEvent::UserEvent` — this crate intentionally does NOT
//!   depend on `ark-types` to avoid a cycle.
//!
//! The supervisor does **not** restart on crash (R16: "no auto-restart
//! v1"). Callers receive a [`CrashReport`] and decide what to do.

use std::collections::VecDeque;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Maximum number of stderr lines retained for crash diagnostics.
/// Lines beyond this are dropped FIFO. Matches R16 "stderr_tail" per
/// the `ark.ext.crashed` UserEvent payload (cap chosen so a single
/// extension crash log fits comfortably in a terminal redraw).
pub const LOG_TAIL_LINES: usize = 100;

/// Default grace period after stdin-close before escalating to SIGTERM
/// (R16 "stdin-close → wait 2s → SIGTERM").
pub const DEFAULT_STDIN_CLOSE_GRACE: Duration = Duration::from_secs(2);

/// Default grace period after SIGTERM before escalating to SIGKILL.
/// R16 doesn't pin a number; we use 1s — long enough for a sane Drop /
/// signal handler, short enough that the supervisor stays responsive.
pub const DEFAULT_SIGTERM_GRACE: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// CrashReport
// ---------------------------------------------------------------------------

/// Structured payload the supervisor crate maps to
/// `AgentEvent::UserEvent { name: "ark.ext.crashed", payload: {…} }`
/// (R16). Carried as a struct rather than a JSON `Value` so the
/// consumer's mapping code can stay type-safe.
///
/// This crate intentionally does NOT depend on `ark-types` (depending
/// on it would form a cycle: scene → supervisor → ext-proto → types →
/// scene). The consumer crate (`crates/supervisor`) translates this
/// struct into the canonical `UserEvent` shape on emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    /// Extension instance name (matches `ExtensionMetadata::name`).
    pub name: String,
    /// Process exit code if the OS surfaced one (`None` for
    /// signal-killed children whose `code()` is `None`).
    pub exit_code: Option<i32>,
    /// Last lines of stderr captured by the supervisor's log-tail
    /// buffer. Newline-joined, capped at [`LOG_TAIL_LINES`] entries.
    pub stderr_tail: String,
}

// ---------------------------------------------------------------------------
// SupervisorHandle
// ---------------------------------------------------------------------------

/// Lightweight clone-friendly handle carrying the diagnostics surface
/// (pid, stderr-tail snapshot accessor) without owning the `Child`.
///
/// Issued by [`ExtSupervisor::handle`] so other subsystems (status
/// plugin, `ark ext list`) can read pid / log tail without pinning the
/// supervisor itself.
#[derive(Debug, Clone)]
pub struct SupervisorHandle {
    /// Extension instance name, copied for diagnostic strings.
    pub name: String,
    /// Process id of the supervised child, captured at spawn. `None`
    /// if the OS didn't return one (in-process/test stubs).
    pub pid: Option<u32>,
    log_tail: Arc<Mutex<VecDeque<String>>>,
}

impl SupervisorHandle {
    /// Snapshot the current stderr log tail as a newline-joined
    /// string. Returned string holds the buffered lines in
    /// insertion order (oldest first).
    pub async fn stderr_tail(&self) -> String {
        let buf = self.log_tail.lock().await;
        buf.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

// ---------------------------------------------------------------------------
// ExtSupervisor
// ---------------------------------------------------------------------------

/// Subprocess extension supervisor.
///
/// Constructed with the extension's `Child` (and an optional
/// pre-detached stderr handle). The supervisor spawns a background
/// task that drains stderr line-by-line into a bounded log-tail
/// buffer. On [`ExtSupervisor::shutdown`] the supervisor walks the
/// graceful-termination ladder; on [`ExtSupervisor::wait_for_exit`]
/// it waits for an unsupervised exit and produces a [`CrashReport`].
///
/// `Drop` is intentionally permissive — dropping the supervisor does
/// NOT kill the child (the caller is responsible for invoking
/// `shutdown` first); this matches the existing `NdjsonClient`
/// ownership model where the transport client and the process handle
/// are intentionally independent.
pub struct ExtSupervisor {
    name: String,
    child: Child,
    log_tail: Arc<Mutex<VecDeque<String>>>,
    stderr_task: Option<JoinHandle<()>>,
    stdin_close_grace: Duration,
    sigterm_grace: Duration,
}

impl ExtSupervisor {
    /// Wrap a freshly spawned child. The child must have stderr piped
    /// (`Stdio::piped()`) for the log-tail to capture anything; if
    /// stderr is `None`, the buffer just stays empty.
    pub fn new(name: impl Into<String>, mut child: Child) -> Self {
        let log_tail = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_TAIL_LINES)));
        let stderr_task = child
            .stderr
            .take()
            .map(|stderr| spawn_stderr_pump(stderr, log_tail.clone()));
        Self {
            name: name.into(),
            child,
            log_tail,
            stderr_task,
            stdin_close_grace: DEFAULT_STDIN_CLOSE_GRACE,
            sigterm_grace: DEFAULT_SIGTERM_GRACE,
        }
    }

    /// Override the stdin-close grace period (default 2s per R16).
    /// Mainly useful for tests that don't want to sit on the default.
    pub fn with_stdin_close_grace(mut self, grace: Duration) -> Self {
        self.stdin_close_grace = grace;
        self
    }

    /// Override the SIGTERM grace period (default 1s).
    pub fn with_sigterm_grace(mut self, grace: Duration) -> Self {
        self.sigterm_grace = grace;
        self
    }

    /// Process id of the supervised child, if known.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Issue a [`SupervisorHandle`] for diagnostics. Cloneable;
    /// outlives this supervisor.
    pub fn handle(&self) -> SupervisorHandle {
        SupervisorHandle {
            name: self.name.clone(),
            pid: self.child.id(),
            log_tail: self.log_tail.clone(),
        }
    }

    /// Snapshot the current stderr log tail. Equivalent to calling
    /// [`SupervisorHandle::stderr_tail`] on this supervisor's handle
    /// without going through the clone.
    pub async fn stderr_tail(&self) -> String {
        let buf = self.log_tail.lock().await;
        buf.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Run the graceful-shutdown ladder per R16:
    ///
    /// 1. Drop stdin (closes the pipe). Wait `stdin_close_grace` for
    ///    the child to exit on its own.
    /// 2. If still alive, send SIGTERM. Wait `sigterm_grace`.
    /// 3. If still alive, send SIGKILL. Wait synchronously.
    ///
    /// Returns the final [`ExitStatus`]. The caller then drops the
    /// supervisor.
    pub async fn shutdown(mut self) -> std::io::Result<ExitStatus> {
        // Step 1: close stdin. Dropping the take()n value triggers
        // EOF on the child's read side; well-behaved extensions
        // notice and exit cleanly.
        drop(self.child.stdin.take());

        if let Ok(Ok(status)) = timeout(self.stdin_close_grace, self.child.wait()).await {
            self.cancel_stderr_task();
            return Ok(status);
        }

        // Step 2: SIGTERM. Use `nix` so we can target the pid without
        // relying on tokio's `kill()` which sends SIGKILL.
        if let Some(pid) = self.child.id() {
            send_signal(pid, Signal::SigTerm);
        }
        if let Ok(Ok(status)) = timeout(self.sigterm_grace, self.child.wait()).await {
            self.cancel_stderr_task();
            return Ok(status);
        }

        // Step 3: SIGKILL. tokio's `kill()` does this synchronously
        // and then waits — which is exactly what we want.
        let _ = self.child.start_kill();
        let status = self.child.wait().await?;
        self.cancel_stderr_task();
        Ok(status)
    }

    /// Wait for the child to exit on its own (no shutdown ladder).
    /// Used by the supervisor crate's "is the extension still alive"
    /// loop; on unexpected exit, builds a [`CrashReport`] from the
    /// captured stderr tail and returns it.
    ///
    /// The returned [`CrashReport`] is populated whether the exit was
    /// clean (`exit_code == Some(0)`) or a crash (`exit_code !=
    /// Some(0)`) — the consumer (the supervisor crate) decides which
    /// is "expected" based on whether `shutdown` had been called yet.
    /// When the consumer wants the typed error variant directly, use
    /// [`ExtSupervisor::wait_for_crash`].
    pub async fn wait_for_exit(mut self) -> std::io::Result<CrashReport> {
        let status = self.child.wait().await?;
        // Give the stderr pump a brief window to drain any final
        // lines the child wrote before exit; tokio's read loop sees
        // EOF and exits, but our pump task is on a separate scheduler
        // tick. 50ms is plenty for short tails and not enough to
        // measurably slow the supervisor-crash path.
        let _ = timeout(Duration::from_millis(50), async {
            while !self.log_tail.lock().await.is_empty()
                && self.stderr_task.as_ref().is_some_and(|h| !h.is_finished())
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        self.cancel_stderr_task();
        let stderr_tail = self.stderr_tail().await;
        Ok(CrashReport {
            name: self.name.clone(),
            exit_code: status.code(),
            stderr_tail,
        })
    }

    /// Specialisation of [`ExtSupervisor::wait_for_exit`] that returns
    /// `Err(ExtensionError::Crashed)` for non-zero exit codes (or
    /// signal-killed children), and `Ok(())` for clean exits.
    /// Convenience for call sites that want the typed error directly.
    pub async fn wait_for_crash(self) -> Result<(), crate::ExtensionError> {
        let report = self
            .wait_for_exit()
            .await
            .map_err(|e| crate::ExtensionError::Internal(e.to_string()))?;
        if report.exit_code == Some(0) {
            Ok(())
        } else {
            Err(crate::ExtensionError::Crashed {
                name: report.name,
                exit_code: report.exit_code,
                stderr_tail: report.stderr_tail,
            })
        }
    }

    fn cancel_stderr_task(&mut self) {
        if let Some(h) = self.stderr_task.take() {
            h.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Internals: stderr pump + signal helpers
// ---------------------------------------------------------------------------

fn spawn_stderr_pump(
    stderr: ChildStderr,
    log_tail: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        pump_lines(stderr, log_tail).await;
    })
}

async fn pump_lines<R: AsyncRead + Unpin>(reader: R, log_tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut buf = log_tail.lock().await;
        if buf.len() == LOG_TAIL_LINES {
            buf.pop_front();
        }
        buf.push_back(line);
    }
}

/// Cross-platform signal abstraction so the rest of the crate doesn't
/// need to `cfg!(unix)` switch. Posix-only for v1 (Windows requires a
/// completely different teardown contract — `TerminateProcess` /
/// CTRL+BREAK — which isn't worth wiring before there's a Windows port
/// of ark itself).
#[allow(dead_code)]
enum Signal {
    SigTerm,
    SigKill,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: Signal) {
    use nix::sys::signal::{Signal as NixSignal, kill};
    use nix::unistd::Pid;
    let nix_signal = match signal {
        Signal::SigTerm => NixSignal::SIGTERM,
        Signal::SigKill => NixSignal::SIGKILL,
    };
    // Best-effort: the child may have exited between our wait and the
    // kill, in which case `kill()` returns ESRCH — ignore.
    let _ = kill(Pid::from_raw(pid as i32), nix_signal);
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: Signal) {
    // Windows fallback: tokio's `start_kill()` is the only lever, and
    // the caller invokes it as the SIGKILL step. SIGTERM is a no-op
    // here.
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;

    /// Spawn `bash` with stdin/stderr piped so the supervisor has
    /// something to drive. Skipped on platforms without `bash`.
    fn spawn_bash(script: &str) -> std::io::Result<Child> {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd.spawn()
    }

    #[tokio::test]
    async fn shutdown_closes_stdin_and_child_exits_cleanly() {
        // Read one line then exit — the supervisor closes stdin which
        // pushes EOF; bash exits on EOF.
        let child = match spawn_bash("read line; echo done") {
            Ok(c) => c,
            Err(_) => return, // No bash on this platform — skip.
        };
        let sup = ExtSupervisor::new("test", child).with_stdin_close_grace(Duration::from_secs(2));
        let status = sup.shutdown().await.expect("shutdown completes");
        assert!(status.success(), "expected clean exit, got {status:?}");
    }

    #[tokio::test]
    async fn stderr_tail_buffers_recent_lines() {
        let child = match spawn_bash("echo line1 1>&2; echo line2 1>&2; echo line3 1>&2; sleep 0.2")
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let sup = ExtSupervisor::new("tail-test", child);
        // Give the stderr pump a moment to read.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let report = sup.wait_for_exit().await.expect("wait_for_exit");
        assert_eq!(
            report.stderr_tail, "line1\nline2\nline3",
            "expected 3 stderr lines, got {:?}",
            report.stderr_tail
        );
        assert_eq!(report.name, "tail-test");
    }

    #[tokio::test]
    async fn shutdown_escalates_when_child_ignores_stdin_close() {
        // Trap SIGTERM with a no-op handler so SIGTERM doesn't kill;
        // verify the supervisor escalates to SIGKILL.
        let script = "trap '' TERM; while true; do sleep 0.1; done";
        let child = match spawn_bash(script) {
            Ok(c) => c,
            Err(_) => return,
        };
        let sup = ExtSupervisor::new("escalate", child)
            .with_stdin_close_grace(Duration::from_millis(50))
            .with_sigterm_grace(Duration::from_millis(50));
        let status = sup.shutdown().await.expect("shutdown completes");
        // SIGKILL = signal 9; on unix, status.code() is None for
        // signal-killed children. Either way, the process is dead.
        assert!(
            !status.success(),
            "trapped child should have been SIGKILL'd, got {status:?}"
        );
    }

    #[tokio::test]
    async fn handle_outlives_supervisor_and_carries_pid() {
        let child = match spawn_bash("sleep 0.05") {
            Ok(c) => c,
            Err(_) => return,
        };
        let pid = child.id();
        let sup = ExtSupervisor::new("handle", child);
        let h = sup.handle();
        let _ = sup.wait_for_exit().await;
        assert_eq!(h.pid, pid);
        assert_eq!(h.name, "handle");
    }

    #[test]
    fn crash_report_carries_required_fields() {
        let r = CrashReport {
            name: "x".into(),
            exit_code: Some(42),
            stderr_tail: "boom".into(),
        };
        assert_eq!(r.name, "x");
        assert_eq!(r.exit_code, Some(42));
        assert_eq!(r.stderr_tail, "boom");
    }

    #[tokio::test]
    async fn wait_for_crash_surfaces_typed_error_on_nonzero_exit() {
        let child = match spawn_bash("echo boom 1>&2; exit 7") {
            Ok(c) => c,
            Err(_) => return,
        };
        let sup = ExtSupervisor::new("crash-test", child);
        // Allow the stderr pump to drain.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let err = sup.wait_for_crash().await.expect_err("non-zero exit");
        match err {
            crate::ExtensionError::Crashed {
                name,
                exit_code,
                stderr_tail,
            } => {
                assert_eq!(name, "crash-test");
                assert_eq!(exit_code, Some(7));
                assert!(stderr_tail.contains("boom"), "tail = {stderr_tail:?}");
            }
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_for_crash_returns_ok_on_clean_exit() {
        let child = match spawn_bash("exit 0") {
            Ok(c) => c,
            Err(_) => return,
        };
        let sup = ExtSupervisor::new("clean", child);
        sup.wait_for_crash().await.expect("clean exit should be Ok");
    }

    #[tokio::test]
    async fn log_tail_caps_at_max_lines() {
        // Spawn a script that emits 150 stderr lines; the buffer
        // should retain only the last LOG_TAIL_LINES (100).
        let script = "for i in $(seq 1 150); do echo line$i 1>&2; done; sleep 0.05";
        let child = match spawn_bash(script) {
            Ok(c) => c,
            Err(_) => return,
        };
        let sup = ExtSupervisor::new("cap", child);
        // Allow the stderr pump to drain all 150 lines.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let report = sup.wait_for_exit().await.unwrap();
        let lines: Vec<&str> = report.stderr_tail.lines().collect();
        assert_eq!(
            lines.len(),
            LOG_TAIL_LINES,
            "expected exactly LOG_TAIL_LINES, got {}",
            lines.len()
        );
        // First retained line is line51 (lines 1..50 dropped).
        assert_eq!(lines[0], "line51");
        assert_eq!(lines[LOG_TAIL_LINES - 1], "line150");
    }
}
