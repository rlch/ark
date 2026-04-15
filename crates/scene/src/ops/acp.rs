//! ACP-interaction ops — R7 #14–17 (cavekit-scene R7 + R17).
//!
//! * `prompt text=<str>` — dispatch `session/prompt`.
//! * `acp_cancel` — dispatch `session/cancel` + block up to 5 s for
//!   the matching `StopReason::Cancelled` response.
//! * `acp_permit request_id=<str> outcome=<str> [option_id=<str>]` —
//!   respond to an outstanding `session/request_permission`
//!   (identified by the `request_id` the permission-requested
//!   UserEvent carries).
//! * `set_mode mode=<str>` — dispatch `session/set_mode`.
//!
//! Each op expects an [`AcpClient`] to be installed on
//! [`IntentContext::acp`]. Before T-ACP.4a wires that up in the
//! supervisor, dispatch returns `op/failed` with a clear "ACP client
//! not wired" message — this keeps compile-time checks green without
//! masking the intent at runtime.
//!
//! # Unstable ACP ops (gated)
//!
//! Per R17, unstable ACP surface — `session/fork`, `nes/*`,
//! `elicitation/*`, … — is NOT registered in v1. Those remain as
//! capability-flagged extensions surfaced through
//! [`acp_client::AcpClient::events`] but do not get dedicated
//! `ark.core.*` ops. When the ACP crate stabilises a surface, a
//! follow-up task adds ops here gated on a per-scene capability
//! flag.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use facet::Facet;
use facet_kdl as kdl;

use acp_client::{AcpClient, AcpError, PermitOutcome};

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

/// Helper — fetch the installed ACP client or surface a uniform
/// `op/failed` error pointing at the unwired state.
fn require_client(
    op_name: &'static str,
    ctx: &IntentContext,
) -> Result<Arc<AcpClient>, IntentError> {
    ctx.acp.get().ok_or_else(|| {
        IntentError::failed(
            op_name,
            "ACP client not wired (T-ACP.4a wires the supervisor handle)"
                .to_string()
                .into(),
        )
    })
}

/// Map an [`AcpError`] into the scene op's [`IntentError::Failed`]
/// variant with a uniform message. Keeps op-level error surfaces
/// consistent across the four ACP ops.
fn lift_acp_error(op_name: &'static str, err: AcpError) -> IntentError {
    IntentError::failed(op_name, err.to_string().into())
}

// ---------------------------------------------------------------------------
// prompt
// ---------------------------------------------------------------------------

/// Args for the `prompt` op — R7 #14. The single positional argument
/// is the prompt text; later variants will accept richer
/// `ContentBlock` bodies (images, resource links, etc.) once the
/// scene schema grows a shape for them.
#[derive(Facet, Debug)]
pub struct PromptArgs {
    /// Prompt text. Runtime-template-rendered (T-4.4) against the
    /// firing event's context before dispatch.
    #[facet(kdl::property)]
    pub text: String,
}

/// facet-kdl document wrapper for [`PromptArgs`].
#[derive(Facet, Debug)]
pub struct PromptDoc {
    /// The `prompt` node body.
    #[facet(kdl::child, rename = "prompt")]
    pub prompt: PromptArgs,
}

/// `prompt` op — always side-effects (every fire starts a new turn).
#[derive(Debug, Default)]
pub struct PromptOp;

impl PromptOp {
    /// Idempotency class for this op. Prompts always fire.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for PromptOp {
    type Args = PromptDoc;
    const NAME: &'static str = "ark.core.prompt";

    async fn dispatch(
        &self,
        args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let client = require_client(Self::NAME, ctx)?;
        let handle = client
            .prompt(&args.prompt.text)
            .await
            .map_err(|e| lift_acp_error(Self::NAME, e))?;

        // Return the JSON-RPC id + session id so cascading reactions
        // (e.g. `emit "my.prompt_started"`) can thread it through.
        //
        // The `stop` receiver stays on the `PromptHandle` — the op
        // does NOT await it here: the response comes back via the
        // `ark.acp.*` event stream, and waiting would serialize the
        // reaction dispatcher behind the turn.
        Ok(Some(serde_json::json!({
            "jsonrpc_id": handle.jsonrpc_id,
            "session_id": handle.session_id,
        })))
    }
}

// ---------------------------------------------------------------------------
// acp_cancel
// ---------------------------------------------------------------------------

/// Window to wait for the `StopReason::Cancelled` confirmation after
/// firing `session/cancel`. Per R7 #15: "blocks up to 5 s for the
/// `stopReason: cancelled` response." In v1 we only issue the
/// notification — the actual stop reason propagates through the
/// event stream as `ark.acp.*` — but the bound is recorded here so
/// future tighter coupling has a pinned budget to respect.
pub const ACP_CANCEL_WAIT_BUDGET: Duration = Duration::from_secs(5);

/// Args for the `acp_cancel` op — R7 #15. No arguments (the target
/// session is implicit from the [`IntentContext`]'s scene).
#[derive(Facet, Debug, Default)]
pub struct AcpCancelArgs {}

/// facet-kdl document wrapper for [`AcpCancelArgs`].
#[derive(Facet, Debug)]
pub struct AcpCancelDoc {
    /// The `acp_cancel` node body.
    #[facet(kdl::child, rename = "acp_cancel")]
    #[allow(dead_code)]
    pub acp_cancel: AcpCancelArgs,
}

/// `acp_cancel` op — always side-effects (fires the cancel notification).
#[derive(Debug, Default)]
pub struct CancelOp;

impl CancelOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for CancelOp {
    type Args = AcpCancelDoc;
    const NAME: &'static str = "ark.core.acp_cancel";

    async fn dispatch(
        &self,
        _args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let client = require_client(Self::NAME, ctx)?;
        // Wrap the cancel call in a 5s timeout (per R7 #15). In
        // practice `session/cancel` is a notification — no response
        // channel — but we still bound the send itself.
        match tokio::time::timeout(ACP_CANCEL_WAIT_BUDGET, client.cancel()).await {
            Ok(Ok(())) => Ok(None),
            Ok(Err(e)) => Err(lift_acp_error(Self::NAME, e)),
            Err(_elapsed) => Err(IntentError::failed(
                Self::NAME,
                format!(
                    "acp_cancel timed out after {}s awaiting dispatch",
                    ACP_CANCEL_WAIT_BUDGET.as_secs()
                )
                .into(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// acp_permit
// ---------------------------------------------------------------------------

/// Valid `outcome=` values for the `acp_permit` op. `"selected"`
/// requires `option_id=<str>`; `"cancelled"` forbids it.
pub const PERMIT_OUTCOMES: &[&str] = &["selected", "cancelled"];

/// Args for the `acp_permit` op — R7 #16.
///
/// Shape: `acp_permit request_id=<str> outcome=<str> [option_id=<str>]`.
///
/// The `request_id` is the opaque correlation key the
/// `ark.acp.permission_requested` event carried.
#[derive(Facet, Debug)]
pub struct AcpPermitArgs {
    /// Correlation key from the fired permission-requested event.
    #[facet(kdl::property)]
    pub request_id: String,

    /// One of [`PERMIT_OUTCOMES`].
    #[facet(kdl::property)]
    pub outcome: String,

    /// When `outcome="selected"`, the picked option's `option_id`.
    /// Ignored when `outcome="cancelled"`.
    #[facet(kdl::property, default)]
    pub option_id: Option<String>,
}

/// facet-kdl document wrapper for [`AcpPermitArgs`].
#[derive(Facet, Debug)]
pub struct AcpPermitDoc {
    /// The `acp_permit` node body.
    #[facet(kdl::child, rename = "acp_permit")]
    pub acp_permit: AcpPermitArgs,
}

/// `acp_permit` op — always side-effects.
#[derive(Debug, Default)]
pub struct PermitOp;

impl PermitOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for PermitOp {
    type Args = AcpPermitDoc;
    const NAME: &'static str = "ark.core.acp_permit";

    async fn dispatch(
        &self,
        args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let client = require_client(Self::NAME, ctx)?;
        let outcome = match args.acp_permit.outcome.as_str() {
            "selected" => {
                let option_id = args.acp_permit.option_id.ok_or_else(|| {
                    IntentError::failed(
                        Self::NAME,
                        "acp_permit outcome=\"selected\" requires option_id=\"…\""
                            .to_string()
                            .into(),
                    )
                })?;
                PermitOutcome::Selected { option_id }
            }
            "cancelled" => PermitOutcome::Cancelled,
            other => {
                return Err(IntentError::failed(
                    Self::NAME,
                    format!(
                        "invalid outcome=\"{other}\"; expected one of {PERMIT_OUTCOMES:?}"
                    )
                    .into(),
                ));
            }
        };
        client
            .permit(&args.acp_permit.request_id, outcome)
            .await
            .map_err(|e| lift_acp_error(Self::NAME, e))?;
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// set_mode
// ---------------------------------------------------------------------------

/// Args for the `set_mode` op — R7 #17.
#[derive(Facet, Debug)]
pub struct SetModeArgs {
    /// Mode id to activate. Scope-checked at scene compile time
    /// against the engine's advertised `AgentCapabilities::modes`
    /// (T-ACP.3 lowers that surface).
    #[facet(kdl::property)]
    pub mode: String,
}

/// facet-kdl document wrapper for [`SetModeArgs`].
#[derive(Facet, Debug)]
pub struct SetModeDoc {
    /// The `set_mode` node body.
    #[facet(kdl::child, rename = "set_mode")]
    pub set_mode: SetModeArgs,
}

/// `set_mode` op — always side-effects.
#[derive(Debug, Default)]
pub struct SetModeOp;

impl SetModeOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for SetModeOp {
    type Args = SetModeDoc;
    const NAME: &'static str = "ark.core.set_mode";

    async fn dispatch(
        &self,
        args: Self::Args,
        ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        let client = require_client(Self::NAME, ctx)?;
        client
            .set_mode(&args.set_mode.mode)
            .await
            .map_err(|e| lift_acp_error(Self::NAME, e))?;
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Canonical ordered list of every ACP-interaction op NAME. Exposed
/// alongside [`CORE_OP_NAMES`](super::CORE_OP_NAMES) so
/// `ark scene check` enumerates the R7 #14–17 surface.
pub const ACP_OP_NAMES: &[&str] = &[
    PromptOp::NAME,
    CancelOp::NAME,
    PermitOp::NAME,
    SetModeOp::NAME,
];

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

    // -- client-not-wired surface ---------------------------------------

    #[tokio::test]
    async fn prompt_without_client_returns_failed() {
        let reg = IntentRegistry::new();
        reg.register(PromptOp).await;
        let n = node(r#"prompt text="hello""#);
        let err = reg
            .dispatch_dyn(PromptOp::NAME, &n, &ctx())
            .await
            .expect_err("must fail");
        match err {
            IntentError::Failed { name, message, .. } => {
                assert_eq!(name, PromptOp::NAME);
                assert!(message.contains("ACP client not wired"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_without_client_returns_failed() {
        let reg = IntentRegistry::new();
        reg.register(CancelOp).await;
        let n = node(r#"acp_cancel"#);
        let err = reg
            .dispatch_dyn(CancelOp::NAME, &n, &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[tokio::test]
    async fn permit_without_client_returns_failed() {
        let reg = IntentRegistry::new();
        reg.register(PermitOp).await;
        let n = node(r#"acp_permit request_id="r1" outcome="cancelled""#);
        let err = reg
            .dispatch_dyn(PermitOp::NAME, &n, &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[tokio::test]
    async fn set_mode_without_client_returns_failed() {
        let reg = IntentRegistry::new();
        reg.register(SetModeOp).await;
        let n = node(r#"set_mode mode="yolo""#);
        let err = reg
            .dispatch_dyn(SetModeOp::NAME, &n, &ctx())
            .await
            .expect_err("must fail");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    // -- args validation -------------------------------------------------

    /// Exercise `PermitOp::dispatch` directly with an empty
    /// `IntentContext` — the client-unwired branch fires
    /// uniformly regardless of outcome value. The outcome-parse
    /// branch is covered by tests that install a wired client at
    /// T-ACP.4a (an in-process ACP stub) rather than trying to
    /// spawn a subprocess from a unit test.
    #[tokio::test]
    async fn permit_rejects_unknown_outcome_via_unwired_branch() {
        let reg = IntentRegistry::new();
        reg.register(PermitOp).await;
        let c = ctx();
        let n = node(r#"acp_permit request_id="r1" outcome="maybe""#);
        let err = reg
            .dispatch_dyn(PermitOp::NAME, &n, &c)
            .await
            .expect_err("must fail");
        match err {
            IntentError::Failed { message, .. } => {
                // The unwired branch runs first; this is intentional
                // to keep scenes authored against v1 compiling until
                // the supervisor wiring lands.
                assert!(message.contains("ACP client not wired"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // -- registration shape ----------------------------------------------

    #[tokio::test]
    async fn registers_four_ops() {
        let reg = IntentRegistry::new();
        reg.register(PromptOp).await;
        reg.register(CancelOp).await;
        reg.register(PermitOp).await;
        reg.register(SetModeOp).await;
        assert_eq!(reg.len().await, 4);
        assert_eq!(ACP_OP_NAMES.len(), 4);
        for name in ACP_OP_NAMES {
            assert!(
                name.starts_with("ark.core."),
                "acp op {name:?} not ark.core.* prefixed"
            );
        }
    }

    #[test]
    fn idempotency_matrix() {
        assert_eq!(PromptOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(CancelOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(PermitOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(SetModeOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
    }

}
