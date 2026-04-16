//! ACP health-check diagnostics for `ark doctor` (T-109).
//!
//! This module provides pure, I/O-free logic for describing ACP checks
//! and producing actionable diagnostic messages from check results.
//! Actual process spawning and network I/O are the caller's responsibility
//! (CLI layer).

use std::time::Duration;

/// Result of an ACP health check.
#[derive(Debug, Clone)]
pub struct AcpHealthCheck {
    /// Whether the engine is healthy, unhealthy, or not configured.
    pub status: AcpHealthStatus,
    /// Observed round-trip latency of the `initialize` handshake, if it completed.
    pub latency: Option<Duration>,
    /// Human-readable summary of the check result.
    pub message: String,
}

/// Possible outcomes of an ACP health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpHealthStatus {
    /// Engine responded to `initialize` within the timeout.
    Healthy,
    /// Engine could not be reached or failed the handshake.
    Unhealthy,
    /// No ACP engine command is configured.
    NotConfigured,
}

/// Specification for an ACP check that the CLI layer can execute.
#[derive(Debug, Clone)]
pub struct AcpCheckSpec {
    /// Command to launch the engine (e.g. `"ark-engine"`).
    pub command: String,
    /// Arguments to pass (always includes `"--acp"`).
    pub args: Vec<String>,
    /// Maximum time to wait for the `initialize` round-trip.
    pub timeout: Duration,
    /// Human-readable description of the success criterion.
    pub expected: &'static str,
}

/// Describe the ACP check to perform for a given engine launch command.
///
/// Returns a pure data spec — no I/O happens here.
pub fn describe_acp_check(launch_command: &str) -> AcpCheckSpec {
    AcpCheckSpec {
        command: launch_command.to_string(),
        args: vec!["--acp".to_string()],
        timeout: Duration::from_secs(1),
        expected: "initialize response within 1s",
    }
}

/// Produce an actionable diagnostic message from a failed ACP check.
///
/// The `command` is the engine binary that was launched and `error` is the
/// raw error string from the failed attempt.
pub fn diagnose_acp_failure(command: &str, error: &str) -> String {
    if error.contains("not found") || error.contains("No such file") {
        format!(
            "`{command}` not on PATH. Install it or set `acp.command` in config."
        )
    } else if error.contains("timed out") || error.contains("timeout") {
        format!(
            "`{command} --acp` started but initialize handshake timed out (>1s). \
             Check that the engine supports ACP."
        )
    } else {
        format!("`{command} --acp` failed: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_acp_check_returns_correct_spec() {
        let spec = describe_acp_check("my-engine");

        assert_eq!(spec.command, "my-engine");
        assert_eq!(spec.args, vec!["--acp"]);
        assert_eq!(spec.timeout, Duration::from_secs(1));
        assert_eq!(spec.expected, "initialize response within 1s");
    }

    #[test]
    fn diagnose_not_found() {
        let msg = diagnose_acp_failure("ark-engine", "ark-engine: not found");
        assert!(
            msg.contains("not on PATH"),
            "expected PATH hint, got: {msg}"
        );
        assert!(msg.contains("acp.command"), "expected config hint, got: {msg}");
    }

    #[test]
    fn diagnose_no_such_file() {
        let msg =
            diagnose_acp_failure("ark-engine", "No such file or directory");
        assert!(
            msg.contains("not on PATH"),
            "expected PATH hint, got: {msg}"
        );
    }

    #[test]
    fn diagnose_timeout() {
        let msg = diagnose_acp_failure(
            "ark-engine",
            "operation timed out after 1s",
        );
        assert!(
            msg.contains("timed out"),
            "expected timeout hint, got: {msg}"
        );
        assert!(
            msg.contains("supports ACP"),
            "expected ACP hint, got: {msg}"
        );
    }

    #[test]
    fn diagnose_generic_error() {
        let msg =
            diagnose_acp_failure("ark-engine", "connection refused");
        assert!(
            msg.contains("connection refused"),
            "expected raw error, got: {msg}"
        );
        assert!(
            msg.starts_with("`ark-engine --acp` failed:"),
            "expected prefix, got: {msg}"
        );
    }
}
