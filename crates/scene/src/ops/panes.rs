//! Pane lifecycle ops — R7 #5–6.
//!
//! * `split_pane into=<str> side=<"left"|"right"|"up"|"down"> [size=<percent|int>] { command <str>; args <str>*; cwd <str>? }`
//! * `close_pane (id=<str>|selector=<str>)`
//!
//! Both STUBS per the `crates/scene/src/ops/mod.rs` contract —
//! [`MuxPlaceholder`] has no real API yet. Args round-trip via facet-kdl
//! so `ark scene check` catches schema errors in advance.
//!
//! TODO(T-5.x): replace `tracing::info!` stubs with real mux calls.

use async_trait::async_trait;
use facet::Facet;
use facet_kdl as kdl;

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

// ---------------------------------------------------------------------------
// split_pane
// ---------------------------------------------------------------------------

/// Side at which `split_pane` should attach the new pane. Kept as a
/// `String` in the Args struct (facet-kdl doesn't derive Facet on
/// arbitrary enums transparently); the op validates it against the
/// R7 set at dispatch.
pub const SPLIT_PANE_SIDES: &[&str] = &["left", "right", "up", "down"];

/// Args to the `split_pane` op — the flat body (positional / property
/// surface). The nested `command`/`args`/`cwd` children live under
/// [`SplitPaneBody`] to model the KDL `{ … }` block R7 describes.
#[derive(Facet, Debug)]
pub struct SplitPaneArgs {
    /// Target pane or tab to split. Cross-referenced against the
    /// scene's `layout { tab name="X" }` declarations at compile time
    /// (T-4.3).
    #[facet(kdl::property)]
    pub into: String,

    /// One of `left`, `right`, `up`, `down`. Validated at dispatch.
    #[facet(kdl::property)]
    pub side: String,

    /// Optional size — percent (`"50%"`) or cell count (`"40"`). Kept as
    /// `String` so both shapes round-trip.
    #[facet(kdl::property, default)]
    pub size: Option<String>,

    /// Optional `command <str>` child node.
    #[facet(kdl::child, default)]
    pub command: Option<SplitPaneCommandNode>,

    /// Optional `cwd <str>` child node.
    #[facet(kdl::child, default)]
    pub cwd: Option<SplitPaneCwdNode>,
}

/// `command "<shell-cmd>"` child of a `split_pane` body. Separate type
/// (vs. an inline `String`) because facet-kdl's derive surface routes
/// child nodes via field name + `rename=`.
#[derive(Facet, Debug)]
pub struct SplitPaneCommandNode {
    /// Shell command string (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// `cwd "<path>"` child of a `split_pane` body.
#[derive(Facet, Debug)]
pub struct SplitPaneCwdNode {
    /// Working directory (first positional argument).
    #[facet(kdl::argument)]
    pub value: String,
}

/// facet-kdl document wrapper for [`SplitPaneArgs`].
#[derive(Facet, Debug)]
pub struct SplitPaneDoc {
    /// The `split_pane` node body.
    #[facet(kdl::child, rename = "split_pane")]
    pub split_pane: SplitPaneArgs,
}

/// `split_pane` op — always side-effects (a fresh pane is materialized
/// on each fire); see [`Idempotency::AlwaysSideEffect`].
#[derive(Debug, Default)]
pub struct SplitPaneOp;

impl SplitPaneOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::AlwaysSideEffect;
}

#[async_trait]
impl Intent for SplitPaneOp {
    type Args = SplitPaneDoc;
    const NAME: &'static str = "ark.core.split_pane";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        if !SPLIT_PANE_SIDES.contains(&args.split_pane.side.as_str()) {
            return Err(IntentError::failed(
                Self::NAME,
                format!(
                    "invalid `side=\"{}\"`; expected one of {:?}",
                    args.split_pane.side, SPLIT_PANE_SIDES
                )
                .into(),
            ));
        }
        // TODO(T-5.x): call `ctx.mux.split_pane(&args.split_pane.into, side, size, command, cwd)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            into = %args.split_pane.into,
            side = %args.split_pane.side,
            size = ?args.split_pane.size,
            command = ?args.split_pane.command.as_ref().map(|c| &c.value),
            cwd = ?args.split_pane.cwd.as_ref().map(|c| &c.value),
            "split_pane (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// close_pane
// ---------------------------------------------------------------------------

/// Args to the `close_pane` op.
///
/// R7 shape: `close_pane (id=<str>|selector=<str>)`. Exactly one of
/// `id=` / `selector=` is required; enforced at dispatch.
#[derive(Facet, Debug)]
pub struct ClosePaneArgs {
    /// Opaque pane id (mux-owned identifier).
    #[facet(kdl::property, default)]
    pub id: Option<String>,

    /// Pane selector (e.g. `"name=editor"`). Grammar owned by the mux.
    #[facet(kdl::property, default)]
    pub selector: Option<String>,
}

/// facet-kdl document wrapper for [`ClosePaneArgs`].
#[derive(Facet, Debug)]
pub struct ClosePaneDoc {
    /// The `close_pane` node body.
    #[facet(kdl::child, rename = "close_pane")]
    pub close_pane: ClosePaneArgs,
}

/// `close_pane` op — noop when the pane is already absent.
#[derive(Debug, Default)]
pub struct ClosePaneOp;

impl ClosePaneOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for ClosePaneOp {
    type Args = ClosePaneDoc;
    const NAME: &'static str = "ark.core.close_pane";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        match (&args.close_pane.id, &args.close_pane.selector) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    "both `id=` and `selector=` provided; specify exactly one"
                        .to_string()
                        .into(),
                ))
            }
            (None, None) => {
                return Err(IntentError::failed(
                    Self::NAME,
                    "neither `id=` nor `selector=` provided; specify exactly one"
                        .to_string()
                        .into(),
                ))
            }
        }
        // TODO(T-5.x): call `ctx.mux.close_pane(selector)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            id = ?args.close_pane.id,
            selector = ?args.close_pane.selector,
            "close_pane (stub: awaiting real mux handle)"
        );
        Ok(None)
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

    #[tokio::test]
    async fn split_pane_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(SplitPaneOp).await;
        let n = node(r#"split_pane into="work" side="right" size="50%" { command "vim"; cwd "/tmp" }"#);
        reg.dispatch_dyn(SplitPaneOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn split_pane_rejects_unknown_side() {
        let reg = IntentRegistry::new();
        reg.register(SplitPaneOp).await;
        let n = node(r#"split_pane into="work" side="diagonal""#);
        let err = reg
            .dispatch_dyn(SplitPaneOp::NAME, &n, &ctx())
            .await
            .expect_err("should reject");
        assert!(matches!(err, IntentError::Failed { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn close_pane_accepts_id() {
        let reg = IntentRegistry::new();
        reg.register(ClosePaneOp).await;
        let n = node(r#"close_pane id="p-1""#);
        reg.dispatch_dyn(ClosePaneOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn close_pane_requires_one_selector() {
        let reg = IntentRegistry::new();
        reg.register(ClosePaneOp).await;
        let n = node(r#"close_pane"#);
        let err = reg
            .dispatch_dyn(ClosePaneOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }));
    }

    #[test]
    fn pane_ops_idempotency_matrix() {
        assert_eq!(SplitPaneOp::IDEMPOTENCY, Idempotency::AlwaysSideEffect);
        assert_eq!(ClosePaneOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
    }
}
