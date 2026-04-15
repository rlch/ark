//! Plugin lifecycle ops — R7 #7–8.
//!
//! * `mount_plugin name=<str> [at=<str>] [into=<str>]` — launch-or-focus
//!   an already-configured plugin (the `plugin { }` block at scene root
//!   owns the `source`, `mount`, `config` side).
//! * `unmount_plugin name=<str>` — dismiss a summon/event-mount plugin.
//!
//! Both STUBS at this tier — zellij's `launch-or-focus-plugin`
//! primitive lives behind [`MuxPlaceholder`]. Args are real.
//!
//! TODO(T-5.x): replace tracing stubs with real mux calls.

use async_trait::async_trait;
use facet::Facet;
use facet_kdl as kdl;

use crate::intent::{Intent, IntentContext, IntentError, IntentValue};

use super::Idempotency;

// ---------------------------------------------------------------------------
// mount_plugin
// ---------------------------------------------------------------------------

/// Args to the `mount_plugin` op.
///
/// R7 shape: `mount_plugin name=<str> [at=<str>] [into=<str>]`. `name` is
/// cross-referenced against the scene's `plugin "<name>" { }` blocks at
/// compile time (T-4.3).
#[derive(Facet, Debug)]
pub struct MountPluginArgs {
    /// Plugin name — must match a `plugin "<name>" { }` block in scope.
    #[facet(kdl::property)]
    pub name: String,

    /// Optional override of the plugin's declared mount target
    /// (`status-bar` / `floating` / `pane` / `hidden`). When absent, the
    /// plugin's own `mount` child is used.
    #[facet(kdl::property, default)]
    pub at: Option<String>,

    /// Optional named pane slot to fill. Mirrors `MountNode::into` on
    /// the scene-root plugin block.
    #[facet(kdl::property, default)]
    pub into: Option<String>,
}

/// facet-kdl document wrapper for [`MountPluginArgs`].
#[derive(Facet, Debug)]
pub struct MountPluginDoc {
    /// The `mount_plugin` node body.
    #[facet(kdl::child, rename = "mount_plugin")]
    pub mount_plugin: MountPluginArgs,
}

/// `mount_plugin` op — delegates to zellij's `launch-or-focus-plugin`
/// primitive; see [`Idempotency::LaunchOrFocus`].
#[derive(Debug, Default)]
pub struct MountPluginOp;

impl MountPluginOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::LaunchOrFocus;
}

#[async_trait]
impl Intent for MountPluginOp {
    type Args = MountPluginDoc;
    const NAME: &'static str = "ark.core.mount_plugin";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        // TODO(T-5.x): call `ctx.mux.launch_or_focus_plugin(...)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = %args.mount_plugin.name,
            at = ?args.mount_plugin.at,
            into = ?args.mount_plugin.into,
            "mount_plugin (stub: awaiting real mux handle)"
        );
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// unmount_plugin
// ---------------------------------------------------------------------------

/// Args to the `unmount_plugin` op.
///
/// R7 shape: `unmount_plugin name=<str>`. Noop when the plugin isn't
/// currently mounted.
#[derive(Facet, Debug)]
pub struct UnmountPluginArgs {
    /// Plugin name to unmount.
    #[facet(kdl::property)]
    pub name: String,
}

/// facet-kdl document wrapper for [`UnmountPluginArgs`].
#[derive(Facet, Debug)]
pub struct UnmountPluginDoc {
    /// The `unmount_plugin` node body.
    #[facet(kdl::child, rename = "unmount_plugin")]
    pub unmount_plugin: UnmountPluginArgs,
}

/// `unmount_plugin` op — idempotent-noop-on-absent.
#[derive(Debug, Default)]
pub struct UnmountPluginOp;

impl UnmountPluginOp {
    /// Idempotency class for this op.
    pub const IDEMPOTENCY: Idempotency = Idempotency::NoopOnAbsent;
}

#[async_trait]
impl Intent for UnmountPluginOp {
    type Args = UnmountPluginDoc;
    const NAME: &'static str = "ark.core.unmount_plugin";

    async fn dispatch(
        &self,
        args: Self::Args,
        _ctx: &IntentContext,
    ) -> Result<Option<IntentValue>, IntentError> {
        // TODO(T-5.x): call `ctx.mux.unmount_plugin(&args.unmount_plugin.name)`.
        tracing::info!(
            target = "scene::ops",
            op = Self::NAME,
            name = %args.unmount_plugin.name,
            "unmount_plugin (stub: awaiting real mux handle)"
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
    async fn mount_plugin_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(MountPluginOp).await;
        let n = node(r#"mount_plugin name="picker" at="floating""#);
        reg.dispatch_dyn(MountPluginOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[tokio::test]
    async fn mount_plugin_missing_name_is_args_invalid() {
        let reg = IntentRegistry::new();
        reg.register(MountPluginOp).await;
        let n = node(r#"mount_plugin"#);
        let err = reg
            .dispatch_dyn(MountPluginOp::NAME, &n, &ctx())
            .await
            .expect_err("required arg");
        assert!(matches!(err, IntentError::ArgsInvalid { .. }));
    }

    #[tokio::test]
    async fn unmount_plugin_round_trip() {
        let reg = IntentRegistry::new();
        reg.register(UnmountPluginOp).await;
        let n = node(r#"unmount_plugin name="picker""#);
        reg.dispatch_dyn(UnmountPluginOp::NAME, &n, &ctx())
            .await
            .expect("dispatch");
    }

    #[test]
    fn plugin_ops_idempotency_matrix() {
        assert_eq!(MountPluginOp::IDEMPOTENCY, Idempotency::LaunchOrFocus);
        assert_eq!(UnmountPluginOp::IDEMPOTENCY, Idempotency::NoopOnAbsent);
    }
}
