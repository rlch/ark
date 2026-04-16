//! Control ops — T-051, R7.
//!
//! * [`ExecOp`]         — `exec script="…" [shell="…"] [timeout_ms=N]`.
//! * [`ReloadSceneOp`]  — `reload_scene` (stub; wired to supervisor in T-083).
//!
//! `exec` does real work today — it spawns a subprocess via
//! [`tokio::process::Command`], captures stdout / stderr / exit code,
//! and enforces a timeout. Scripts run in `sh -c <script>` by default;
//! scenes needing bash extensions pass `shell="bash"`.
//!
//! `reload_scene` is a deliberate stub: the supervisor integration
//! lands in Tier-14 (T-083 hot reload). Dispatching it today logs and
//! returns `Ok(IntentValue::None)` so scenes authored against the full
//! R7 surface parse + compile cleanly.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use kdl::KdlNode;
use tokio::process::Command;

use crate::error::SceneError;
use crate::intent::{
    Intent, IntentContext, IntentValue, property_str, property_u64,
};

/// Default shell used when `shell=` is absent. `sh` is ubiquitous on
/// ark's target platforms (Linux + macOS) and portable enough to run
/// any legal `script=` value.
pub const DEFAULT_SHELL: &str = "sh";

/// Default `exec` timeout in ms. 30 seconds matches the "reactions
/// should not wedge the event loop" principle in R4.
pub const DEFAULT_EXEC_TIMEOUT_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// `exec script="…" [shell="…"] [timeout_ms=N]` — run a shell script.
///
/// Returns `IntentValue::Integer(exit_code)` on success so follow-up
/// ops in the same reaction can branch on the exit status (once op→op
/// result chaining lands in v0.2; for Tier 5 the value is informational).
/// Timeouts surface as `op/failed` with a clear message.
#[derive(Debug, Default)]
pub struct ExecOp;

const EXEC_NAME: &str = "ark.core.exec";

#[async_trait]
impl Intent for ExecOp {
    async fn dispatch(
        &self,
        args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        let script = property_str(args, "script").ok_or_else(|| SceneError::OpFailed {
            op: EXEC_NAME.to_string(),
            message: "missing required property `script=`".to_string(),
        })?;
        let shell = property_str(args, "shell").unwrap_or_else(|| DEFAULT_SHELL.to_string());
        let timeout_ms =
            property_u64(args, "timeout_ms").unwrap_or(DEFAULT_EXEC_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);

        let mut cmd = Command::new(&shell);
        cmd.arg("-c").arg(&script);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::info!(
            target: "scene::ops",
            op = EXEC_NAME,
            shell = %shell,
            timeout_ms = timeout.as_millis() as u64,
            origin = %ctx.origin,
            "exec spawning"
        );

        let run = async {
            let output = cmd.output().await?;
            Ok::<_, std::io::Error>(output)
        };

        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Err(SceneError::OpFailed {
                    op: EXEC_NAME.to_string(),
                    message: format!("exec spawn failed: {e}"),
                });
            }
            Err(_elapsed) => {
                return Err(SceneError::OpFailed {
                    op: EXEC_NAME.to_string(),
                    message: format!(
                        "exec timed out after {}ms (script={:?})",
                        timeout.as_millis(),
                        truncate(&script, 80)
                    ),
                });
            }
        };

        let code = output.status.code().unwrap_or(-1);
        tracing::info!(
            target: "scene::ops",
            op = EXEC_NAME,
            exit_code = code,
            success = output.status.success(),
            "exec finished"
        );
        Ok(IntentValue::Integer(code as i64))
    }
}

/// Trim `s` to at most `max` chars + `"..."` suffix when longer. Used
/// in error messages so a multi-kilobyte script doesn't flood the log.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("...");
        out
    }
}

// ---------------------------------------------------------------------------
// reload_scene (stub)
// ---------------------------------------------------------------------------

/// `reload_scene` — re-parse the scene and apply deltas.
///
/// Stub for Tier 5: logs a tracing line and returns
/// `Ok(IntentValue::None)`. Real wiring through the supervisor's
/// `SceneReloader` lands in Tier-14 (T-083).
#[derive(Debug, Default)]
pub struct ReloadSceneOp;

const RELOAD_SCENE_NAME: &str = "ark.core.reload_scene";

#[async_trait]
impl Intent for ReloadSceneOp {
    async fn dispatch(
        &self,
        _args: &KdlNode,
        ctx: &IntentContext,
    ) -> Result<IntentValue, SceneError> {
        tracing::info!(
            target: "scene::ops",
            op = RELOAD_SCENE_NAME,
            origin = %ctx.origin,
            "reload_scene (stub — Tier 14 wires to supervisor)"
        );
        Ok(IntentValue::None)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::IntentContext;
    use crate::intent::tests::{MockBus, MockMux, node_from, test_scene_id};
    use std::sync::Arc;

    fn ctx() -> IntentContext {
        let mux = Arc::new(MockMux::default());
        let bus = Arc::new(MockBus::default());
        IntentContext::new(test_scene_id(), "scene")
            .with_mux(mux)
            .with_bus(bus)
    }

    #[tokio::test]
    async fn exec_runs_script_and_returns_exit_code() {
        let node = node_from(r#"exec script="exit 0""#);
        let v = ExecOp.dispatch(&node, &ctx()).await.expect("ok");
        assert_eq!(v, IntentValue::Integer(0));
    }

    #[tokio::test]
    async fn exec_captures_non_zero_exit() {
        let node = node_from(r#"exec script="exit 7""#);
        let v = ExecOp.dispatch(&node, &ctx()).await.expect("ok");
        assert_eq!(v, IntentValue::Integer(7));
    }

    #[tokio::test]
    async fn exec_times_out() {
        // Sleep for 2s with a 50ms timeout — must surface as op/failed.
        let node = node_from(r#"exec script="sleep 2" timeout_ms=50"#);
        let err = ExecOp.dispatch(&node, &ctx()).await.expect_err("must timeout");
        if let SceneError::OpFailed { message, .. } = err {
            assert!(
                message.contains("timed out"),
                "expected timed-out message, got: {message}"
            );
        } else {
            panic!("expected OpFailed");
        }
    }

    #[tokio::test]
    async fn exec_missing_script_errors() {
        let node = node_from(r#"exec"#);
        let err = ExecOp.dispatch(&node, &ctx()).await.expect_err("must error");
        assert!(matches!(err, SceneError::OpFailed { .. }));
    }

    #[tokio::test]
    async fn reload_scene_stub_returns_ok() {
        let node = node_from(r#"reload_scene"#);
        let v = ReloadSceneOp.dispatch(&node, &ctx()).await.expect("ok");
        assert_eq!(v, IntentValue::None);
    }

    #[test]
    fn truncate_short_strings_unchanged() {
        assert_eq!(truncate("short", 80), "short");
    }

    #[test]
    fn truncate_long_strings_get_ellipsis() {
        let s: String = "x".repeat(100);
        let t = truncate(&s, 10);
        assert_eq!(t.chars().count(), 13); // 10 + "..."
        assert!(t.ends_with("..."));
    }
}
