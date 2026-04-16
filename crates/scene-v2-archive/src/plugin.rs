//! Plugin lowering: typed `PluginNode` (AST) → `PluginDecl` (lifecycle-resolved).
//!
//! The scene AST in [`crate::ast::PluginNode`] models a `plugin "<name>" { … }`
//! block with each child node held as a typed `Option<…>` / `Vec<…>` field.
//! That shape preserves source fidelity but leaves the *lifecycle* of each
//! plugin implicit — the user writes `summon` or `on` (or neither), and the
//! runtime infers which mode applies per `cavekit-scene.md` R6.
//!
//! This module promotes that implicit choice to an explicit
//! [`Lifecycle`] enum on the lowered [`PluginDecl`] view. The lowering
//! function [`lower_plugin`] also enforces R6's mutual-exclusion rule:
//! a `plugin { }` body that declares **both** `summon` and `on` lifecycle
//! markers is ambiguous and surfaces as
//! [`SceneError::PluginAmbiguousLifecycle`] with both attributes
//! highlighted.
//!
//! ## Lifecycle inference rules (R6)
//!
//! | `summon` present | `on` (lifecycle marker) present | resolved [`Lifecycle`] |
//! |-----------------:|--------------------------------:|------------------------|
//! |             no   |                            no   | [`Lifecycle::Always`]  |
//! |            yes   |                            no   | [`Lifecycle::Summon`]  |
//! |             no   |                           yes   | [`Lifecycle::EventMount`] |
//! |            yes   |                           yes   | error — `scene/plugin-ambiguous-lifecycle` |
//!
//! Note: the `on` here is the **plugin-body** `on` lifecycle marker
//! (modeled as [`crate::ast::PluginOnNode`]), NOT a scene-root reaction.
//! Scope is enforced separately by [`crate::scope`].
//!
//! ## Span fidelity for the conflict diagnostic
//!
//! The pure-AST entry point [`lower_plugin`] cannot reach back to byte
//! offsets in the original KDL source — `facet-kdl` does not annotate
//! the typed AST with spans (see the design note at the top of
//! [`crate::ast`]). When this lowering pass fires from the eventual
//! compile-pipeline driver (T-3.x / T-4.x), the driver wraps
//! [`lower_plugin`] with [`enrich_ambiguous_lifecycle_spans`] (also in
//! this module) which walks the raw `kdl::KdlDocument` to attach precise
//! spans to the `summon` and `on` child nodes. The pure call uses
//! placeholder zero-length spans against an empty `NamedSource`.

use kdl::KdlDocument;
use miette::{NamedSource, SourceSpan};

use crate::ast::PluginNode;
use crate::error::SceneError;

/// Resolved plugin lifecycle per R6.
///
/// Distinct from the textual presence of `summon` / `on` children: the
/// lowering step in [`lower_plugin`] turns the implicit AST shape into
/// this explicit choice and rejects the `summon`-AND-`on` combination
/// as a hard error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// Plugin mounts at scene start and stays mounted for the session
    /// lifetime — the default when the body declares neither `summon`
    /// nor a plugin-body `on` lifecycle marker.
    Always,

    /// Plugin is dormant until its `summon` selector matches an event;
    /// a matching `dismiss` selector (if any) closes it again.
    Summon,

    /// Plugin mounts on every match of its body-level `on` selector
    /// (R6 "event-mount"). Closes via `dismiss` or its own dismiss
    /// signal — same surface as [`Lifecycle::Summon`].
    EventMount,
}

impl Lifecycle {
    /// Stable string form for diagnostics and debug logging. Lowercase
    /// kebab to match R6's spec wording (`always`, `summon`,
    /// `event-mount`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Always => "always",
            Lifecycle::Summon => "summon",
            Lifecycle::EventMount => "event-mount",
        }
    }
}

/// Lowered, lifecycle-resolved view of a `plugin "<name>" { … }` block.
///
/// Built by [`lower_plugin`] from a typed [`PluginNode`]. Carries the
/// borrowed shape of the AST node — the lowering step does not clone
/// the underlying configuration tree; later compile-pipeline passes
/// that need owned data can transform from this view as they go.
///
/// String fields are borrowed from the AST node so the lowering step
/// stays allocation-free apart from constructing the wrapper struct.
#[derive(Debug, Clone)]
pub struct PluginDecl<'a> {
    /// Plugin name, copied straight from [`PluginNode::name`].
    pub name: &'a str,

    /// `source "<uri>"` argument (R6). `None` iff the AST omitted the
    /// child node — the compile pipeline enforces presence as a
    /// separate `scene/grammar` violation.
    pub source: Option<&'a str>,

    /// `mount <target> …` target argument (R6). Same `None` semantics
    /// as [`Self::source`].
    pub mount: Option<&'a str>,

    /// Resolved lifecycle per R6 (see [`Lifecycle`]).
    pub lifecycle: Lifecycle,

    /// Subscriber selectors (`subscribes "<sel>"` children). Order
    /// preserved from the source file.
    pub subscribes: Vec<&'a str>,

    /// Whether a `config { }` block was provided. Schema-level
    /// validation of the block's contents happens in a later
    /// compile-pipeline pass once the plugin's manifest is resolved.
    pub has_config: bool,
}

/// Lower a typed [`PluginNode`] (AST) into a lifecycle-resolved
/// [`PluginDecl`].
///
/// On lifecycle conflict (R6: both `summon` and `on` present)
/// returns [`SceneError::PluginAmbiguousLifecycle`] with placeholder
/// zero-length spans against an empty `NamedSource`. The compile
/// pipeline driver re-runs [`enrich_ambiguous_lifecycle_spans`] to
/// attach precise spans before surfacing the error to the user — see
/// the module-level docs for the contract.
///
/// This function does NOT validate that `source` / `mount` are
/// present, that the `source` URI is well-formed, or that subscribers
/// match a known event grammar. Those checks live in dedicated passes
/// (T-3.x grammar refinement) so this lowering remains a pure shape
/// transform.
#[allow(clippy::result_large_err)] // SceneError::PluginAmbiguousLifecycle carries NamedSource.
pub fn lower_plugin(node: &PluginNode) -> Result<PluginDecl<'_>, SceneError> {
    let has_summon = node.summon.is_some();
    let has_on = !node.on.is_empty();

    if has_summon && has_on {
        return Err(SceneError::PluginAmbiguousLifecycle {
            name: node.name.clone(),
            // Placeholder source — the compile-pipeline driver
            // overwrites this via `enrich_ambiguous_lifecycle_spans`
            // before user-facing rendering. See module docs.
            src: NamedSource::new("", String::new()),
            summon_at: SourceSpan::new(0.into(), 0),
            on_at: SourceSpan::new(0.into(), 0),
        });
    }

    let lifecycle = match (has_summon, has_on) {
        (false, false) => Lifecycle::Always,
        (true, false) => Lifecycle::Summon,
        (false, true) => Lifecycle::EventMount,
        // Already returned above.
        (true, true) => unreachable!(),
    };

    Ok(PluginDecl {
        name: node.name.as_str(),
        source: node.source.as_ref().map(|s| s.uri.as_str()),
        mount: node.mount.as_ref().map(|m| m.target.as_str()),
        lifecycle,
        subscribes: node.subscribes.iter().map(|s| s.selector.as_str()).collect(),
        has_config: node.config.is_some(),
    })
}

/// Re-issue a [`SceneError::PluginAmbiguousLifecycle`] with precise
/// spans pulled from the raw KDL source.
///
/// Walks `src` looking for the named `plugin "<plugin_name>" { … }`
/// block, then captures the byte spans of its first `summon` child and
/// its first `on` child. Returns the original error unchanged when the
/// block cannot be located (defensive — `lower_plugin` having flagged a
/// conflict implies the block exists, but we never want span enrichment
/// to itself fail loudly).
///
/// `path` is used purely for the `NamedSource` filename rendered by
/// miette.
#[allow(clippy::result_large_err)]
pub fn enrich_ambiguous_lifecycle_spans(
    err: SceneError,
    src: &str,
    path: &std::path::Path,
) -> SceneError {
    let SceneError::PluginAmbiguousLifecycle { name, .. } = err else {
        return err;
    };

    // If we can't even reparse the source, hand back a placeholder
    // error keyed to the file so miette has *something* to render.
    let Ok(doc) = KdlDocument::parse(src) else {
        return SceneError::PluginAmbiguousLifecycle {
            name,
            src: NamedSource::new(path.display().to_string(), src.to_string()),
            summon_at: SourceSpan::new(0.into(), src.len().min(1)),
            on_at: SourceSpan::new(0.into(), src.len().min(1)),
        };
    };

    let plugin_node = find_plugin_block(&doc, &name);
    let (summon_at, on_at) = match plugin_node {
        Some(node) => {
            let summon = first_child_span(node, "summon")
                .unwrap_or_else(|| node.name().span());
            let on = first_child_span(node, "on")
                .unwrap_or_else(|| node.name().span());
            (summon, on)
        }
        None => {
            let fallback = SourceSpan::new(0.into(), src.len().min(1));
            (fallback, fallback)
        }
    };

    SceneError::PluginAmbiguousLifecycle {
        name,
        src: NamedSource::new(path.display().to_string(), src.to_string()),
        summon_at,
        on_at,
    }
}

/// Walk the raw KDL document for the first `plugin "<name>" { … }`
/// block matching `name`. Used by [`enrich_ambiguous_lifecycle_spans`].
///
/// The walker descends through every `scene { … }` body but does not
/// recurse into `plugin` bodies (no nested plugins per R2/R6). When the
/// document has no `scene` root (which `parse_scene` would have
/// rejected upstream) we still scan the top-level nodes as a fallback.
fn find_plugin_block<'a>(doc: &'a KdlDocument, name: &str) -> Option<&'a kdl::KdlNode> {
    fn match_plugin<'a>(node: &'a kdl::KdlNode, name: &str) -> Option<&'a kdl::KdlNode> {
        if node.name().value() != "plugin" {
            return None;
        }
        let arg = node.entries().iter().find(|e| e.name().is_none())?;
        let value = arg.value().as_string()?;
        if value == name {
            Some(node)
        } else {
            None
        }
    }

    for top in doc.nodes() {
        if let Some(found) = match_plugin(top, name) {
            return Some(found);
        }
        // Descend through `scene { … }` (single level — plugins live
        // at the scene root only per R2).
        if top.name().value() == "scene" {
            if let Some(body) = top.children() {
                for child in body.nodes() {
                    if let Some(found) = match_plugin(child, name) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

/// Return the span of the first child of `parent` whose name matches
/// `child_name`. Returns `None` when the parent has no body or the
/// child name never appears.
fn first_child_span(parent: &kdl::KdlNode, child_name: &str) -> Option<SourceSpan> {
    let body = parent.children()?;
    body.nodes()
        .iter()
        .find(|n| n.name().value() == child_name)
        .map(|n| n.name().span())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::parse::parse_scene;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.kdl")
    }

    fn first_plugin(src: &str) -> PluginNode {
        let doc = parse_scene(src, &p()).expect("parse fixture");
        doc.scene
            .plugins
            .into_iter()
            .next()
            .expect("fixture must declare at least one plugin")
    }

    #[test]
    fn lifecycle_always_when_neither_summon_nor_on() {
        let src = r#"
scene "s" {
    plugin "always" {
        source "shipped:foo"
        mount "status-bar"
    }
}
"#;
        let plugin = first_plugin(src);
        let decl = lower_plugin(&plugin).expect("lowering succeeds");
        assert_eq!(decl.name, "always");
        assert_eq!(decl.lifecycle, Lifecycle::Always);
        assert_eq!(decl.source, Some("shipped:foo"));
        assert_eq!(decl.mount, Some("status-bar"));
        assert!(decl.subscribes.is_empty());
        assert!(!decl.has_config);
    }

    #[test]
    fn lifecycle_summon_when_only_summon_present() {
        let src = r#"
scene "s" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
}
"#;
        let plugin = first_plugin(src);
        let decl = lower_plugin(&plugin).expect("lowering succeeds");
        assert_eq!(decl.lifecycle, Lifecycle::Summon);
    }

    #[test]
    fn lifecycle_event_mount_when_only_on_present() {
        let src = r#"
scene "s" {
    plugin "diff" {
        source "shipped:diff"
        mount "floating"
        on "UserEvent:agent.tool_request"
    }
}
"#;
        let plugin = first_plugin(src);
        let decl = lower_plugin(&plugin).expect("lowering succeeds");
        assert_eq!(decl.lifecycle, Lifecycle::EventMount);
    }

    #[test]
    fn ambiguous_lifecycle_when_summon_and_on_both_present() {
        let src = r#"
scene "s" {
    plugin "broken" {
        source "shipped:foo"
        mount "floating"
        summon "UserEvent:open"
        on "UserEvent:other"
    }
}
"#;
        let plugin = first_plugin(src);
        let err = lower_plugin(&plugin).expect_err("conflict must error");
        assert_eq!(err.code_enum(), ErrorCode::PluginAmbiguousLifecycle);
        match err {
            SceneError::PluginAmbiguousLifecycle { name, .. } => {
                assert_eq!(name, "broken");
            }
            other => panic!("expected PluginAmbiguousLifecycle, got {other:?}"),
        }
    }

    #[test]
    fn enrich_attaches_real_spans_to_conflict() {
        let src = r#"scene "s" {
    plugin "broken" {
        source "shipped:foo"
        mount "floating"
        summon "UserEvent:open"
        on "UserEvent:other"
    }
}
"#;
        let plugin = first_plugin(src);
        let err = lower_plugin(&plugin).expect_err("conflict must error");
        let enriched = enrich_ambiguous_lifecycle_spans(err, src, &p());
        match enriched {
            SceneError::PluginAmbiguousLifecycle {
                summon_at, on_at, ..
            } => {
                // Both spans must have non-zero length once enriched
                // (the placeholder pre-enrichment spans were `(0, 0)`).
                assert!(
                    summon_at.len() > 0,
                    "enriched summon span must be non-zero: {summon_at:?}"
                );
                assert!(
                    on_at.len() > 0,
                    "enriched on span must be non-zero: {on_at:?}"
                );
                // And they must point at distinct byte offsets.
                assert_ne!(
                    summon_at.offset(),
                    on_at.offset(),
                    "summon and on must occupy different source positions"
                );
            }
            other => panic!("expected PluginAmbiguousLifecycle, got {other:?}"),
        }
    }

    #[test]
    fn enrich_passes_non_conflict_errors_through_unchanged() {
        let err = SceneError::Grammar {
            message: "unrelated".to_string(),
            src: NamedSource::new("x", String::new()),
            at: SourceSpan::new(0.into(), 0),
        };
        let out = enrich_ambiguous_lifecycle_spans(err, "scene \"s\"", &p());
        // Round-trip preserves the unrelated error variant.
        assert_eq!(out.code_enum(), ErrorCode::Grammar);
    }

    #[test]
    fn lifecycle_as_str_uses_kebab_case() {
        assert_eq!(Lifecycle::Always.as_str(), "always");
        assert_eq!(Lifecycle::Summon.as_str(), "summon");
        assert_eq!(Lifecycle::EventMount.as_str(), "event-mount");
    }

    #[test]
    fn subscribes_are_collected_in_order() {
        let src = r#"
scene "s" {
    plugin "tap" {
        source "shipped:foo"
        mount "hidden"
        subscribes "UserEvent:a"
        subscribes "UserEvent:b"
    }
}
"#;
        let plugin = first_plugin(src);
        let decl = lower_plugin(&plugin).expect("lowering succeeds");
        assert_eq!(decl.subscribes, vec!["UserEvent:a", "UserEvent:b"]);
        assert_eq!(decl.lifecycle, Lifecycle::Always);
    }
}
