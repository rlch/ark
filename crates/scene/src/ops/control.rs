//! Control ops — R7 #12–13.
//!
//! * `exec script=<str> [shell=<str>] [timeout_ms=<int>] [cwd=<str>] [env { <kv>* }]`
//! * `reload_scene` — re-parse scene + apply deltas (respects the
//!   turn-inflight guard per R17 / R14).
//!
//! `exec` is REAL — it spawns a subprocess via
//! [`tokio::process::Command`] and returns the rendered stdout/stderr
//! plus the exit code as a structured [`IntentValue`]. No placeholder
//! dependency.
//!
//! `reload_scene` is a STUB: it depends on the
//! supervisor handle ([`crate::intent::SupervisorHandle`]) which is a
//! placeholder. TODO(T-5.x) covers swapping in the real reload path.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use facet::Facet;
use facet_kdl as kdl;
use tokio::process::Command;

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// Default shell used when `shell=` is absent. `sh` is ubiquitous on
/// ark's target platforms (Linux + macOS) and portable enough to run
/// any legal `script=` value. Scenes needing bash-specific syntax pass
/// `shell="bash"`.
pub const DEFAULT_SHELL: &str = "sh";

/// Default timeout in ms for `exec`. 30 seconds matches the
/// "reactions should not wedge the event loop" principle in R4.
pub const DEFAULT_EXEC_TIMEOUT_MS: u64 = 30_000;

/// Args to the `exec` op.
///
/// R7 shape: `exec script=<str> [shell=<str>] [timeout_ms=<int>]
/// [cwd=<str>] [env { <kv>* }]`. The `env { }` child carries `kv`
/// pairs — each a `<NAME> "<value>"` KDL node. Represented as
/// [`ExecEnvNode`] for facet-kdl compatibility.
#[derive(Facet, Debug)]
pub struct ExecArgs {
    /// Shell script to run. Rendered with runtime templating at dispatch
    /// time (T-4.4 — `cargo test {{ payload.filter }}` resolves the
    /// template against the firing event's context).
    #[facet(kdl::property)]
    pub script: String,

    /// Shell used to interpret `script`. Defaults to [`DEFAULT_SHELL`].
    #[facet(kdl::property, default)]
    pub shell: Option<String>,

    /// Timeout in milliseconds. Defaults to [`DEFAULT_EXEC_TIMEOUT_MS`].
    /// Exceeding is surfaced as `op/failed` with `"exec: timed out"`.
    #[facet(kdl::property, default)]
    pub timeout_ms: Option<u64>,

    /// Working directory. Defaults to the current process cwd.
    #[facet(kdl::property, default)]
    pub cwd: Option<String>,

    /// Optional `env { NAME "value" ... }` child.
    #[facet(kdl::child, default)]
    pub env: Option<ExecEnvBlock>,
}

/// `env { NAME "value" ... }` child of an `exec` body. Each child node
/// is one env var (node name = var name, first positional argument =
/// value). Represented through the [`ExecEnvNode`] struct inside a
/// `children` vector.
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct ExecEnvBlock {
    /// One `<NAME> "<value>"` entry per env var.
    #[facet(kdl::children, default)]
    pub vars: Vec<ExecEnvVarNode>,
}

/// A single env-var entry under `exec { env { … } }`. facet-kdl does
/// not expose "arbitrary node name = key, arg = value" today — each
/// entry must have a fixed node name. For v1 the only accepted shape
/// is `var NAME="FOO" value="bar"`; this is a deliberate stand-in that
/// will be widened when facet-kdl grows a raw-node-name capture.
///
/// TODO(facet-kdl-raw-name): replace with `<NAME> "<value>"` when
/// facet-kdl can capture the node name as a field.
#[derive(Facet, Debug)]
pub struct ExecEnvVarNode {
    /// Env var name. Caller writes `var name="FOO" value="bar"`.
    #[facet(kdl::property)]
    pub name: String,
    /// Env var value.
    #[facet(kdl::property)]
    pub value: String,
}

/// facet-kdl document wrapper for [`ExecArgs`].
#[derive(Facet, Debug)]
pub struct ExecDoc {
    /// The `exec` node body.
    #[facet(kdl::child, rename = "exec")]
    pub exec: ExecArgs,
}

/// `exec` op — spawns a subprocess and captures its output. Always
/// side-effects.
#[derive(Debug, Default)]
pub struct ExecOp;

impl ExecOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for ExecOp {
    type Args = ExecDoc;
    const NAME: &'static str = "ark.core.exec";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let ExecArgs {
            script,
            shell,
            timeout_ms,
            cwd,
            env,
        } = args.exec;

        let shell = shell.unwrap_or_else(|| DEFAULT_SHELL.to_string());
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_EXEC_TIMEOUT_MS));

        let mut cmd = Command::new(&shell);
        cmd.arg("-c").arg(&script);
        if let Some(dir) = &cwd {
            cmd.current_dir(dir);
        }
        if let Some(env_block) = &env {
            for v in &env_block.vars {
                cmd.env(&v.name, &v.value);
            }
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            shell = %shell,
            timeout_ms = timeout.as_millis() as u64,
            cwd = ?cwd,
            "exec spawning"
        );

        let run = async {
            let output = cmd.output().await?;
            Ok::<_, std::io::Error>(output)
        };

        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    format!("exec: spawn failed: {e}").into(),
                ));
            }
            Err(_elapsed) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    format!(
                        "exec: timed out after {}ms (script={:?})",
                        timeout.as_millis(),
                        truncate(&script, 80)
                    )
                    .into(),
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let status = output.status;
        let code = status.code().unwrap_or(-1);

        Ok(Some(serde_json::json!({
            "exit_code": code,
            "success": status.success(),
            "stdout": stdout,
            "stderr": stderr,
        })))
    }
}

/// Trim `s` to at most `max` bytes + `"..."` suffix when longer. Used in
/// error messages so a multi-kilobyte script doesn't flood the log.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("...");
        out
    }
}

// ---------------------------------------------------------------------------
// reload_scene
// ---------------------------------------------------------------------------

/// Args to the `reload_scene` op — the op takes no body, but we still
/// need a typed `Args` for the Intent trait. An empty struct satisfies
/// facet-kdl.
#[derive(Facet, Debug)]
pub struct ReloadSceneArgs {}

/// facet-kdl document wrapper for [`ReloadSceneArgs`].
#[derive(Facet, Debug)]
pub struct ReloadSceneDoc {
    /// The `reload_scene` node body (no args).
    #[facet(kdl::child, rename = "reload_scene")]
    #[allow(dead_code)]
    pub reload_scene: ReloadSceneArgs,
}

/// `reload_scene` op — re-parses the scene and applies deltas. Single-
/// slot re-entry guard (R14): concurrent calls while a reload is active
/// are dropped with a debug log. Here classified as
/// `NoopOnAbsent` because a reload with nothing queued is a no-op.
#[derive(Debug, Default)]
pub struct ReloadSceneOp;

impl ReloadSceneOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for ReloadSceneOp {
    type Args = ReloadSceneDoc;
    const NAME: &'static str = "ark.core.reload_scene";

    async fn dispatch(
        &self,
        _args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        // TODO(T-5.x): call `ctx.supervisor.reload_scene()` once
        // `SupervisorHandle` is replaced with the real facade. The real
        // implementation must honor the turn-inflight guard per R14.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            "reload_scene (stub: awaiting real supervisor handle)"
        );
        Ok(None)
    }
}

/// Structured result from a successful `exec` op, provided to callers
/// that prefer typed access over drilling into the raw `IntentValue`
/// JSON. Not used by the op itself; exposed for downstream ergonomic
/// helpers (e.g. reaction cascades that want to branch on `exit_code`).
#[derive(Debug, Clone)]
pub struct ExecResult {
    /// Shell exit code, or `-1` when the process was signalled.
    pub exit_code: i32,
    /// True iff `exit_code == 0`.
    pub success: bool,
    /// UTF-8-lossy rendering of captured stdout.
    pub stdout: String,
    /// UTF-8-lossy rendering of captured stderr.
    pub stderr: String,
}

impl ExecResult {
    /// Parse an [`IntentValue`] produced by [`ExecOp`] into a typed
    /// [`ExecResult`]. Returns `None` when the value does not match the
    /// expected shape (e.g. caller passed a non-`exec` result).
    pub fn from_value(v: &IntentValue) -> Option<Self> {
        let map = v.as_object()?;
        let mut env_map = BTreeMap::new();
        for (k, v) in map {
            env_map.insert(k.clone(), v.clone());
        }
        Some(Self {
            exit_code: env_map.get("exit_code")?.as_i64()? as i32,
            success: env_map.get("success")?.as_bool()?,
            stdout: env_map.get("stdout")?.as_str()?.to_string(),
            stderr: env_map.get("stderr")?.as_str()?.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SceneId;
    use crate::intent::IntentRegistry;
    use ::kdl::{KdlDocument, KdlNode};
    use std::path::PathBuf;

    fn ctx() -> IntentContext {
        IntentContext::placeholder(SceneId::from_bytes(
            PathBuf::from("/tmp/scene.kdl"),
            b"scene \"x\" { }",
        ))
    }

    fn node(src: &str) -> KdlNode {
        let doc: KdlDocument = src.parse().expect("parse");
        doc.nodes().first().cloned().expect("node")
    }

    // -- exec -----------------------------------------------------------

    #[tokio::test]
    async fn exec_prints_stdout() {
        let reg = IntentRegistry::new();
        reg.register(ExecOp).await;
        let n = node(r#"exec script="printf hello""#);
        let ret = reg
            .dispatch_dyn(ExecOp::NAME, &n, &ctx())
            .await
            .expect("dispatch")
            .expect("some value");
        let res = ExecResult::from_value(&ret).expect("shape");
        assert!(res.success);
        assert_eq!(res.exit_code, 0);
        assert_eq!(res.stdout, "hello");
        assert!(res.stderr.is_empty());
    }

    #[tokio::test]
    async fn exec_captures_nonzero_exit() {
        let reg = IntentRegistry::new();
        reg.register(ExecOp).await;
        let n = node(r#"exec script="exit 7""#);
        let ret = reg
            .dispatch_dyn(ExecOp::NAME, &n, &ctx())
            .await
            .expect("dispatch")
            .expect("value");
        let res = ExecResult::from_value(&ret).expect("shape");
        assert!(!res.success);
        assert_eq!(res.exit_code, 7);
    }

    #[tokio::test]
    async fn exec_honors_env_block() {
        let reg = IntentRegistry::new();
        reg.register(ExecOp).await;
        let n = node(
            r#"exec script="printf %s $OP_TEST_VAR" { env { var name="OP_TEST_VAR" value="42" } }"#,
        );
        let ret = reg
            .dispatch_dyn(ExecOp::NAME, &n, &ctx())
            .await
            .expect("dispatch")
            .expect("value");
        let res = ExecResult::from_value(&ret).expect("shape");
        assert_eq!(res.stdout, "42");
    }

    #[tokio::test]
    async fn exec_timeout_surfaces_as_failed() {
        let reg = IntentRegistry::new();
        reg.register(ExecOp).await;
        let n = node(r#"exec script="sleep 5" timeout_ms=100"#);
        let err = reg
            .dispatch_dyn(ExecOp::NAME, &n, &ctx())
            .await
            .expect_err("must timeout");
        match err {
            IntentError::Failed { message, .. } => {
                assert!(
                    message.contains("timed out"),
                    "expected timeout message, got {message:?}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // -- reload_scene ---------------------------------------------------

    #[tokio::test]
    async fn reload_scene_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(ReloadSceneOp).await;
        let n = node(r#"reload_scene"#);
        reg.dispatch_dyn(ReloadSceneOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[test]
    fn control_ops_idempotency_matrix() {
        assert_eq!(ExecOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(ReloadSceneOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
    }
}
