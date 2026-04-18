//! Command executor abstraction over `tokio::process::Command`.
//!
//! Exists so `ZellijMux` can be unit-tested without spawning real zellij
//! processes. See cavekit-mux-zellij.md R6:
//!
//! > Commands spawned with `zellij` use `tokio::process::Command`, capture
//! > stderr for error reporting.
//! > All zellij invocations run with PATH only; no fancy shell expansion.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Mutex;
use tokio::process::Command;

/// Result of running a single command.
#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Abstraction over `tokio::process::Command` so tests can inject a stub
/// that records calls rather than spawning zellij.
#[async_trait]
pub trait CommandExecutor: Send + Sync {
    /// Run `program args...` and return the combined exit status + captured
    /// stdout/stderr.
    ///
    /// The caller controls arguments verbatim — this is not a shell, so
    /// there is no expansion, globbing, or quoting. Stdin is null; stdout
    /// and stderr are piped and captured.
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput>;
}

/// Real executor — spawns processes via `tokio::process`.
#[derive(Debug, Default, Clone)]
pub struct RealExecutor;

impl RealExecutor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CommandExecutor for RealExecutor {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // No env overrides — inherit the supervisor's environment
            // (which the supervisor itself is responsible for scrubbing to
            // PATH + a minimal whitelist before spawning the mux).
            .output()
            .await?;
        Ok(CommandOutput {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// Test executor — records every call as `(program, args)` tuples and
/// returns queued responses in FIFO order.
#[derive(Debug, Default)]
pub struct StubExecutor {
    pub calls: Mutex<Vec<(String, Vec<String>)>>,
    pub responses: Mutex<VecDeque<CommandOutput>>,
}

impl StubExecutor {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(VecDeque::new()),
        }
    }

    /// Enqueue a response to be returned by the next `run` call.
    pub fn queue_response(&self, output: CommandOutput) {
        self.responses.lock().unwrap().push_back(output);
    }

    /// Snapshot of the recorded calls so far.
    pub fn recorded_calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandExecutor for StubExecutor {
    async fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        self.calls.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        match self.responses.lock().unwrap().pop_front() {
            Some(output) => Ok(output),
            None => Err(std::io::Error::other("no queued response")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn real_executor_echo_captures_stdout() {
        let exec = RealExecutor::new();
        let out = exec.run("echo", &["hello"]).await.expect("echo must run");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello\n");
        assert!(out.stderr.is_empty());
    }

    #[tokio::test]
    async fn real_executor_false_is_nonzero() {
        let exec = RealExecutor::new();
        let out = exec
            .run("false", &[])
            .await
            .expect("false binary must exist");
        assert!(!out.status.success());
    }

    #[tokio::test]
    async fn real_executor_missing_binary_errors() {
        let exec = RealExecutor::new();
        let res = exec.run("nonexistent-binary-xxxx", &[]).await;
        assert!(res.is_err(), "missing binary should return io::Error");
    }

    #[tokio::test]
    async fn stub_records_calls_and_pops_queued() {
        let stub = StubExecutor::new();
        // Fabricate a successful output. We can't construct ExitStatus
        // directly on all platforms, so run `true` once with RealExecutor
        // to borrow a status.
        let real = RealExecutor::new();
        let seed = real.run("true", &[]).await.expect("true must run");
        stub.queue_response(CommandOutput {
            status: seed.status,
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
        });

        let out = stub.run("zellij", &["--version"]).await.unwrap();
        assert_eq!(out.stdout, b"ok");

        let calls = stub.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "zellij");
        assert_eq!(calls[0].1, vec!["--version".to_string()]);
    }

    #[tokio::test]
    async fn stub_errors_when_no_response_queued() {
        let stub = StubExecutor::new();
        let res = stub.run("zellij", &[]).await;
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
        assert!(err.to_string().contains("no queued response"));
    }
}
