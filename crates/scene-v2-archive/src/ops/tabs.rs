//! Tab lifecycle ops — R7 #1–4.
//!
//! * `open_tab name=<str> [layout=<str>] [focus=<bool>]`
//! * `close_tab (name=<str>|index=<int>)`
//! * `rename_tab (name=<str>|index=<int>) to=<str>`
//! * `focus_tab (name=<str>|index=<int>)`
//!
//! All four are STUBS at this tier: the zellij mux handle lives behind a
//! placeholder ([`MuxPlaceholder`]) so each op records a
//! `tracing::info!` line against the `scene::ops` target and returns
//! `Ok(None)`. The typed [`Args`](crate::intent::Intent::Args) structs
//! are real — facet-kdl parses them at dispatch time, so
//! `ark scene check` catches schema violations ahead of runtime even
//! though the dispatch itself is inert.
//!
//! TODO(T-5.x): replace every `tracing::info!` stub with real calls into
//! the zellij mux handle when [`MuxPlaceholder`] is swapped for the
//! concrete mux surface.

use async_trait::async_trait;
use facet::Facet;
use facet_kdl as kdl;

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

// ---------------------------------------------------------------------------
// open_tab
// ---------------------------------------------------------------------------

/// Args to the `open_tab` op.
///
/// R7 shape: `open_tab name=<str> [layout=<str>] [focus=<bool>]`.
#[derive(Facet, Debug)]
pub struct OpenTabArgs {
    /// Tab name. Matched against existing tabs under the
    /// if-absent-focus-else-create rule (see [`Idempotency`]).
    #[facet(kdl::property)]
    pub name: String,

    /// Optional zellij layout name to apply when CREATING a new tab.
    /// Ignored on the focus branch.
    #[facet(kdl::property, default)]
    pub layout: Option<String>,

    /// When `true`, focus the tab after create (or on focus branch).
    /// Defaults to `true` in R7; we surface it explicitly so scenes can
    /// opt out of stealing focus on a reaction.
    #[facet(kdl::property, default)]
    pub focus: Option<bool>,
}

/// facet-kdl document wrapper for [`OpenTabArgs`].
#[derive(Facet, Debug)]
pub struct OpenTabDoc {
    /// The `open_tab` node body.
    #[facet(kdl::child, rename = "open_tab")]
    pub open_tab: OpenTabArgs,
}

/// `open_tab` op — if a tab with `name=` exists, focus it; otherwise
/// create it. See [`Idempotency::IfAbsentFocusElseCreate`].
#[derive(Debug, Default)]
pub struct OpenTabOp;

impl OpenTabOp {
    /// Idempotency class for this op (consumed by the dispatch matrix).
    pub const IDEMPOTENCY: Idempotency = Idempotency::IfAbsentFocusElseCreate;
}

#[async_trait]
impl Intent for OpenTabOp {
    type Args = OpenTabDoc;
    const NAME: &'static str = "ark.core.open_tab";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        // TODO(T-5.x): call `ctx.mux.new_tab_or_focus(&args.open_tab.name, layout, focus)`
        // once `MuxPlaceholder` is replaced with the real handle.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = %args.open_tab.name,
            layout = ?args.open_tab.layout,
            focus = ?args.open_tab.focus,
            "open_tab (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// close_tab
// ---------------------------------------------------------------------------

/// Args to the `close_tab` op — identify the tab by either `name=` or
/// `index=`. R7 phrasing is `(name=<str>|index=<int>)`; facet-kdl does
/// not express one-of constraints directly, so both fields are
/// `Option<_>` and the op enforces exactly-one at dispatch.
#[derive(Facet, Debug)]
pub struct CloseTabArgs {
    /// Tab name (exclusive with `index`).
    #[facet(kdl::property, default)]
    pub name: Option<String>,

    /// Tab index (exclusive with `name`).
    #[facet(kdl::property, default)]
    pub index: Option<u32>,
}

/// facet-kdl document wrapper for [`CloseTabArgs`].
#[derive(Facet, Debug)]
pub struct CloseTabDoc {
    /// The `close_tab` node body.
    #[facet(kdl::child, rename = "close_tab")]
    pub close_tab: CloseTabArgs,
}

/// `close_tab` op — drop the tab identified by `name=` or `index=`. Noop
/// when the tab is already absent (see [`Idempotency::NoopOnAbsent`]).
#[derive(Debug, Default)]
pub struct CloseTabOp;

impl CloseTabOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for CloseTabOp {
    type Args = CloseTabDoc;
    const NAME: &'static str = "ark.core.close_tab";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        validate_tab_selector(Self::NAME, &args.close_tab.name, &args.close_tab.index)?;
        // TODO(T-5.x): call `ctx.mux.close_tab(selector)` (name or index).
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = ?args.close_tab.name,
            index = ?args.close_tab.index,
            "close_tab (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// rename_tab
// ---------------------------------------------------------------------------

/// Args to the `rename_tab` op.
///
/// R7 shape: `rename_tab (name=<str>|index=<int>) to=<str>`.
#[derive(Facet, Debug)]
pub struct RenameTabArgs {
    /// Current tab name (exclusive with `index`).
    #[facet(kdl::property, default)]
    pub name: Option<String>,

    /// Current tab index (exclusive with `name`).
    #[facet(kdl::property, default)]
    pub index: Option<u32>,

    /// New tab name.
    #[facet(kdl::property)]
    pub to: String,
}

/// facet-kdl document wrapper for [`RenameTabArgs`].
#[derive(Facet, Debug)]
pub struct RenameTabDoc {
    /// The `rename_tab` node body.
    #[facet(kdl::child, rename = "rename_tab")]
    pub rename_tab: RenameTabArgs,
}

/// `rename_tab` op — relabel the tab identified by `name=` or `index=`
/// to `to=`. Noop when the tab doesn't exist.
#[derive(Debug, Default)]
pub struct RenameTabOp;

impl RenameTabOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for RenameTabOp {
    type Args = RenameTabDoc;
    const NAME: &'static str = "ark.core.rename_tab";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        validate_tab_selector(Self::NAME, &args.rename_tab.name, &args.rename_tab.index)?;
        // TODO(T-5.x): call `ctx.mux.rename_tab(selector, &args.rename_tab.to)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = ?args.rename_tab.name,
            index = ?args.rename_tab.index,
            to = %args.rename_tab.to,
            "rename_tab (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// focus_tab
// ---------------------------------------------------------------------------

/// Args to the `focus_tab` op.
///
/// R7 shape: `focus_tab (name=<str>|index=<int>)`.
#[derive(Facet, Debug)]
pub struct FocusTabArgs {
    /// Tab name (exclusive with `index`).
    #[facet(kdl::property, default)]
    pub name: Option<String>,

    /// Tab index (exclusive with `name`).
    #[facet(kdl::property, default)]
    pub index: Option<u32>,
}

/// facet-kdl document wrapper for [`FocusTabArgs`].
#[derive(Facet, Debug)]
pub struct FocusTabDoc {
    /// The `focus_tab` node body.
    #[facet(kdl::child, rename = "focus_tab")]
    pub focus_tab: FocusTabArgs,
}

/// `focus_tab` op — switch focus to the tab identified by `name=` or
/// `index=`. Noop when the tab doesn't exist.
#[derive(Debug, Default)]
pub struct FocusTabOp;

impl FocusTabOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for FocusTabOp {
    type Args = FocusTabDoc;
    const NAME: &'static str = "ark.core.focus_tab";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        validate_tab_selector(Self::NAME, &args.focus_tab.name, &args.focus_tab.index)?;
        // TODO(T-5.x): call `ctx.mux.focus_tab(selector)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = ?args.focus_tab.name,
            index = ?args.focus_tab.index,
            "focus_tab (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Shared: (name|index) validator
// ---------------------------------------------------------------------------

/// Enforce the "exactly one of `name=`/`index=`" invariant. Returns an
/// [`IntentError::Failed`] when zero or both fields are set. Surfaces as
/// the canonical `op/failed` error code.
fn validate_tab_selector(
    op_name: &str,
    name: &Option<String>,
    index: &Option<u32>,
) -> Result<(), IntentError> {
    match (name.is_some(), index.is_some()) {
        (true, false) | (false, true) => Ok(()),
        (true, true) => Err(IntentError::failed(
            op_name,
            "both `name=` and `index=` provided; specify exactly one"
                .to_string()
                .into(),
        )),
        (false, false) => Err(IntentError::failed(
            op_name,
            "neither `name=` nor `index=` provided; specify exactly one"
                .to_string()
                .into(),
        )),
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

    // ---- open_tab ------------------------------------------------------

    #[tokio::test]
    async fn open_tab_args_round_trip_through_registry() {
        let reg = IntentRegistry::new();
        reg.register(OpenTabOp).await;

        let n = node(r#"open_tab name="work" layout="main" focus=#true"#);
        let r = reg
            .dispatch_dyn(OpenTabOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
        assert!(r.is_none(), "open_tab stub returns no value");
    }

    #[tokio::test]
    async fn open_tab_missing_name_surfaces_args_invalid() {
        let reg = IntentRegistry::new();
        reg.register(OpenTabOp).await;
        // `name=` is required.
        let n = node(r#"open_tab layout="main""#);
        let err = reg
            .dispatch_dyn(OpenTabOp::NAME, &n, &ctx())
            .await
            .expect_err("missing required arg");
        assert!(
            matches!(err, IntentError::ArgsInvalid { .. }),
            "expected ArgsInvalid, got {err:?}"
        );
    }

    #[tokio::test]
    async fn open_tab_idempotency_tag() {
        assert_eq!(OpenTabOp::IDEMPOTENCY, Idempotency::IfAbsentFocusElseCreate);
    }

    // ---- close_tab -----------------------------------------------------

    #[tokio::test]
    async fn close_tab_accepts_name() {
        let reg = IntentRegistry::new();
        reg.register(CloseTabOp).await;
        let n = node(r#"close_tab name="work""#);
        reg.dispatch_dyn(CloseTabOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn close_tab_accepts_index() {
        let reg = IntentRegistry::new();
        reg.register(CloseTabOp).await;
        let n = node(r#"close_tab index=2"#);
        reg.dispatch_dyn(CloseTabOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn close_tab_rejects_both_selectors() {
        let reg = IntentRegistry::new();
        reg.register(CloseTabOp).await;
        let n = node(r#"close_tab name="work" index=2"#);
        let err = reg
            .dispatch_dyn(CloseTabOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn close_tab_rejects_neither_selector() {
        let reg = IntentRegistry::new();
        reg.register(CloseTabOp).await;
        let n = node(r#"close_tab"#);
        let err = reg
            .dispatch_dyn(CloseTabOp::NAME, &n, &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, IntentError::Failed { .. }), "got {err:?}");
    }

    // ---- rename_tab ----------------------------------------------------

    #[tokio::test]
    async fn rename_tab_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(RenameTabOp).await;
        let n = node(r#"rename_tab name="old" to="new""#);
        reg.dispatch_dyn(RenameTabOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    // ---- focus_tab -----------------------------------------------------

    #[tokio::test]
    async fn focus_tab_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(FocusTabOp).await;
        let n = node(r#"focus_tab name="work""#);
        reg.dispatch_dyn(FocusTabOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    // ---- idempotency documentation check ------------------------------

    /// Idempotency classes match the matrix documented in `ops/mod.rs`.
    #[test]
    fn tab_ops_idempotency_matrix() {
        assert_eq!(OpenTabOp::IDEMPOTENCY, Idempotency::IfAbsentFocusElseCreate);
        assert_eq!(CloseTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(RenameTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
        assert_eq!(FocusTabOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
    }
}
