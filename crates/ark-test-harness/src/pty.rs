//! PTY wrapper — spawns a child under a master/slave pty pair and
//! drains its output into an in-memory buffer via a reader thread.
//!
//! Built on `portable-pty` 0.8 (already used elsewhere in the
//! workspace). The wrapper intentionally does not parse VT100 codes —
//! callers that need a cell grid can feed [`PtyProcess::snapshot_bytes`]
//! into the `vt100` crate themselves.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// PTY-attached child process with a background reader draining the
/// master side into a shared buffer.
pub struct PtyProcess {
    /// PTY master writer. Mutex-wrapped so `&self` `send_input` calls
    /// can serialize write access.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Child-process handle. Mutex-wrapped so `&self` methods can
    /// `try_wait` / `kill` without taking the whole struct by value.
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    /// Shared output buffer populated by the reader thread.
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl std::fmt::Debug for PtyProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyProcess")
            .field(
                "buffer_len",
                &self.buffer.lock().map(|b| b.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl PtyProcess {
    /// Spawn `program args...` under a new pty pair.
    ///
    /// Environment handling:
    ///   * `env` pairs are applied first (overrides inherited values).
    ///   * `remove_env` keys are removed AFTER `env` is applied (so
    ///     callers can prepend a PATH shim via `env` and still drop
    ///     `$ZELLIJ` unambiguously).
    ///
    /// Returns immediately after spawn; the caller is responsible for
    /// waiting / killing via [`PtyProcess::shutdown_with_timeout`] or
    /// [`PtyProcess::kill`].
    pub fn spawn(
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        remove_env: &[&str],
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pty = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .with_context(|| "openpty failed")?;

        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        for k in remove_env {
            cmd.env_remove(k);
        }

        let child = pty
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to spawn `{}` under pty", program.display()))?;

        // Drop the slave once the child has it — keeps the master as
        // the sole remaining holder so EOF propagates cleanly when the
        // child exits.
        drop(pty.slave);

        let mut reader = pty
            .master
            .try_clone_reader()
            .with_context(|| "try_clone_reader failed on pty master")?;
        let writer = pty
            .master
            .take_writer()
            .with_context(|| "take_writer failed on pty master")?;

        let buffer = Arc::new(Mutex::new(Vec::<u8>::with_capacity(16 * 1024)));
        let sink = Arc::clone(&buffer);

        std::thread::Builder::new()
            .name("ark-test-harness-pty-reader".to_string())
            .spawn(move || {
                use std::io::Read;
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => break, // EOF on master — child exited + pty closed.
                        Ok(n) => {
                            if let Ok(mut guard) = sink.lock() {
                                // Cap the buffer at ~256 KiB so a
                                // runaway child doesn't OOM a host.
                                const MAX: usize = 256 * 1024;
                                if guard.len() + n > MAX {
                                    let overflow = guard.len() + n - MAX;
                                    guard.drain(..overflow);
                                }
                                guard.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .with_context(|| "failed to spawn pty reader thread")?;

        Ok(Self {
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            buffer,
        })
    }

    /// Write `text` verbatim to the pty master.
    pub fn send_input(&self, text: &str) -> Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| anyhow!("pty writer mutex poisoned"))?;
        w.write_all(text.as_bytes())
            .with_context(|| "write to pty master failed")?;
        w.flush().with_context(|| "flush on pty master failed")?;
        Ok(())
    }

    /// Snapshot the cumulative pty output as raw bytes.
    pub fn snapshot_bytes(&self) -> Vec<u8> {
        self.buffer.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Lossy UTF-8 snapshot — convenient for `contains` style assertions.
    pub fn snapshot_lossy(&self) -> String {
        match self.buffer.lock() {
            Ok(g) => String::from_utf8_lossy(&g).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Kill the pty child immediately.
    pub fn kill(&self) -> Result<()> {
        let mut c = self
            .child
            .lock()
            .map_err(|_| anyhow!("pty child mutex poisoned"))?;
        c.kill()
            .map_err(|e| anyhow!("failed to kill pty child: {e}"))
    }

    /// Is the child still running?
    pub fn is_running(&self) -> bool {
        match self.child.lock() {
            Ok(mut c) => matches!(c.try_wait(), Ok(None)),
            Err(_) => false,
        }
    }

    /// Kill the child and wait up to `timeout` for it to reap.
    pub fn shutdown_with_timeout(self, timeout: Duration) -> Result<()> {
        {
            let mut c = self
                .child
                .lock()
                .map_err(|_| anyhow!("pty child mutex poisoned"))?;
            let _ = c.kill();
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match c.try_wait() {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawns `/bin/echo hello` under a pty and asserts the output
    /// lands in the buffer. Exercises the reader thread + snapshot.
    #[test]
    fn pty_spawn_captures_stdout() {
        let echo = Path::new("/bin/echo");
        if !echo.is_file() {
            eprintln!("SKIP: /bin/echo missing");
            return;
        }
        let args = vec!["hello-from-pty".to_string()];
        let pty = match PtyProcess::spawn(echo, &args, &[], &[], 24, 80) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("SKIP: pty spawn unsupported: {e}");
                return;
            }
        };

        // Give the reader thread up to 2 s to drain.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let snap = pty.snapshot_lossy();
            if snap.contains("hello-from-pty") {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "pty buffer never saw `hello-from-pty`; got:\n{}",
            pty.snapshot_lossy()
        );
    }
}
