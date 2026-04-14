//! zellij pipe forwarder (T-049, cavekit-hook-ipc.md R1 pipe clause).
//!
//! After translating a hook payload into one or more AgentEvents, the
//! sidecar forwards each serialized event to the `ark-status` and
//! `ark-picker` pipe targets via
//! `zellij pipe --name <target> -- <payload>`.
//!
//! All failures (zellij not on PATH, pipe returns non-zero, target plugin
//! not loaded) are **fail-open** (R3): we log a warning to stderr and
//! return `Ok(())` so claude is never blocked.
//!
//! Large payloads are truncated at a char boundary to a soft cap of
//! 100KB before piping. Zellij's pipe payload surface is not a hot path
//! for huge blobs; this is purely a safety valve.

use std::io;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use tracing::{debug, warn};

/// Soft cap on pipe payload size (bytes). Oversized payloads are
/// truncated at a char boundary before piping. Documented as a soft
/// limit in v1.
pub const PIPE_PAYLOAD_MAX_BYTES: usize = 100 * 1024;

/// Bounded wait for `zellij pipe` subprocesses.
pub const ZELLIJ_PIPE_TIMEOUT: Duration = Duration::from_secs(2);

/// Known zellij pipe targets forwarded by ark-hook.
pub const TARGET_ARK_STATUS: &str = "ark-status";
pub const TARGET_ARK_PICKER: &str = "ark-picker";

/// Truncate `payload` at a char boundary to at most
/// [`PIPE_PAYLOAD_MAX_BYTES`] bytes. Returns the slice length actually
/// kept.
fn truncate_at_char_boundary(payload: &str) -> &str {
    if payload.len() <= PIPE_PAYLOAD_MAX_BYTES {
        return payload;
    }
    // Walk backward from the cap to the nearest char boundary.
    let mut end = PIPE_PAYLOAD_MAX_BYTES;
    while end > 0 && !payload.is_char_boundary(end) {
        end -= 1;
    }
    &payload[..end]
}

/// Forward `payload` to zellij's pipe named `target`.
///
/// Returns `Ok(())` on success **and on every failure** (fail-open per
/// R3). Internal errors are logged to stderr via `tracing::warn`.
pub fn pipe_to_zellij(target: &str, payload: &str) -> anyhow::Result<()> {
    pipe_with(target, payload, run_zellij_pipe)
}

/// Testable core: pipe via a caller-supplied `run_fn` so unit tests can
/// simulate zellij's presence / exit status without a real binary.
pub fn pipe_with<F>(target: &str, payload: &str, run_fn: F) -> anyhow::Result<()>
where
    F: FnOnce(&str, &str) -> io::Result<ExitStatus>,
{
    let trimmed = truncate_at_char_boundary(payload);
    if trimmed.len() < payload.len() {
        debug!(
            target,
            original_bytes = payload.len(),
            kept_bytes = trimmed.len(),
            "pipe payload truncated to soft cap"
        );
    }

    match run_fn(target, trimmed) {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => {
            warn!(
                target,
                code = status.code().unwrap_or(-1),
                "zellij pipe returned non-zero; fail-open"
            );
            Ok(())
        }
        Err(e) => {
            warn!(
                target,
                error = %e,
                "zellij pipe spawn failed (likely not on PATH); fail-open"
            );
            Ok(())
        }
    }
}

/// Spawn `zellij pipe --name <target> -- <payload>` and wait up to
/// [`ZELLIJ_PIPE_TIMEOUT`] for it to exit. The timeout is a belt-and-suspenders
/// guard — zellij's pipe call is fast under normal operation.
fn run_zellij_pipe(target: &str, payload: &str) -> io::Result<ExitStatus> {
    let mut child = Command::new("zellij")
        .arg("pipe")
        .arg("--name")
        .arg(target)
        .arg("--")
        .arg(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // std::process::Child doesn't have a wait_timeout on stable, so we
    // poll in a short loop. 2s budget, 10ms granularity = 200 polls.
    let deadline = std::time::Instant::now() + ZELLIJ_PIPE_TIMEOUT;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    // Report as a timeout-flavored io error; caller will warn + fail-open.
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "zellij pipe exceeded 2s timeout",
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn zellij_not_on_path_returns_ok_and_warns() {
        let simulated_enoent = |_target: &str, _payload: &str| -> io::Result<ExitStatus> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no zellij"))
        };
        let r = pipe_with(TARGET_ARK_STATUS, "{}", simulated_enoent);
        assert!(r.is_ok());
    }

    #[test]
    fn zellij_exit_non_zero_returns_ok_and_warns() {
        let simulated_nonzero = |_target: &str, _payload: &str| -> io::Result<ExitStatus> {
            Ok(ExitStatus::from_raw(1 << 8)) // exit code 1
        };
        let r = pipe_with(TARGET_ARK_STATUS, "{}", simulated_nonzero);
        assert!(r.is_ok());
    }

    #[test]
    fn zellij_success_returns_ok() {
        let simulated_ok = |_target: &str, _payload: &str| -> io::Result<ExitStatus> {
            Ok(ExitStatus::from_raw(0))
        };
        let r = pipe_with(TARGET_ARK_PICKER, "{\"ok\":true}", simulated_ok);
        assert!(r.is_ok());
    }

    #[test]
    fn large_payload_is_truncated_at_char_boundary() {
        // 200 KB of multi-byte chars guarantees we cross a codepoint
        // at the naive byte cap and have to walk back to a char boundary.
        let big = "é".repeat(100_000); // 200_000 bytes
        let trimmed = truncate_at_char_boundary(&big);
        assert!(trimmed.len() <= PIPE_PAYLOAD_MAX_BYTES);
        assert!(trimmed.is_char_boundary(trimmed.len()));
        // Verify the caller-visible path passes the truncated payload.
        let mut captured: Vec<(String, usize)> = Vec::new();
        let run = |target: &str, payload: &str| -> io::Result<ExitStatus> {
            captured.push((target.to_string(), payload.len()));
            Ok(ExitStatus::from_raw(0))
        };
        pipe_with(TARGET_ARK_STATUS, &big, run).unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].1 <= PIPE_PAYLOAD_MAX_BYTES);
    }

    #[test]
    fn small_payload_not_truncated() {
        let s = "hello";
        assert_eq!(truncate_at_char_boundary(s), s);
    }
}
