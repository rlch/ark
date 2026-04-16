//! Auto-mount the `ark-bus` plugin (T-073).
//!
//! Per cavekit-scene.md R5 (ark-bus auto-mount) + R4 (zellij-side event
//! integration): any scene that uses a feature requiring the `ark-bus`
//! plugin must declare it. Rather than force every scene author to write
//! the boilerplate, the compile pipeline detects the trigger conditions
//! and splices the plugin declaration in automatically.
//!
//! ## When to inject
//!
//! Inject if **any** of:
//!
//! 1. The scene declares one or more `bind "<chord>" { … }` nodes.
//!    Keybinds compile to `MessagePlugin "ark-bus" name="ark-intent"
//!    payload=…` actions (T-065), so ark-bus must be loaded to receive
//!    the pipe message.
//! 2. Any `on` reaction's selector references a zellij-side event that
//!    ark-bus is responsible for forwarding (T-071) — the canonical
//!    UserEvent name is `UserEvent:ark.zellij.<kind>` or bare
//!    `ark.zellij.<kind>` for `kind ∈ {command_pane_opened,
//!    command_pane_exited, pane_closed, file_system_update}`.
//!
//! ## When NOT to inject
//!
//! - Pure-AgentEvent scenes (e.g. status-only setups that only react to
//!   ACP events) save one plugin load per session by skipping the
//!   injection.
//! - If the rendered layout doc already declares a top-level
//!   `plugin "ark-bus"` node (explicit user override), we never silently
//!   shadow it.
//!
//! ## Injected shape
//!
//! A peer top-level `plugin` node is prepended to the rendered KDL
//! document:
//!
//! ```kdl
//! plugin "ark-bus" {
//!     source "shipped:ark-bus"
//!     mount "hidden"
//! }
//! layout { … }
//! ```
//!
//! The plugin node is scene-layer metadata (zellij doesn't parse top-
//! level `plugin` nodes natively; the supervisor reads it). Using a peer
//! node rather than a nested pane keeps the zellij-consumed `layout { }`
//! subtree identical to what the layout compiler already produces, so
//! `zellij action override-layout` still sees only the layout it
//! expects.

#![allow(clippy::result_large_err)]

use kdl::{KdlDocument, KdlEntry, KdlNode};

use crate::ast::SceneBodyNode;
use crate::parse::SceneIR;

/// Plugin name expected by every other ark component when it talks to
/// the bus. Pinned as a constant so the injector and consumers
/// (T-070, T-072) all agree on a single identifier.
pub const ARK_BUS_PLUGIN_NAME: &str = "ark-bus";

/// `source` URI for the auto-injected ark-bus plugin. `shipped:` =
/// resolved out of the ark distribution rather than a user extension.
pub const ARK_BUS_SOURCE: &str = "shipped:ark-bus";

/// `mount` target — zellij's hidden / suppressed-pane API. ark-bus is
/// headless so this is the right semantic shape; `size 0` would also
/// hide it but is a geometry hack.
pub const ARK_BUS_MOUNT_TARGET: &str = "hidden";

/// Canonical prefix for zellij-side events forwarded by ark-bus (T-071).
/// Selectors containing this substring trigger injection so the
/// broadcaster is actually present at session-spawn. Both the
/// fully-qualified `UserEvent:ark.zellij.<kind>` form and the bare
/// `ark.zellij.<kind>` form are recognised — authors write one or the
/// other depending on whether their `on` selector includes the
/// `UserEvent:` prefix.
pub const ARK_BUS_EVENT_NEEDLE: &str = "ark.zellij.";

/// Whether `ir`'s scene declares at least one `bind` node.
///
/// `bind` declarations compile to `MessagePlugin "ark-bus"` actions
/// (T-065), so any scene with binds needs ark-bus at runtime to receive
/// the pipe.
pub fn has_binds(ir: &SceneIR) -> bool {
    ir.scene
        .body
        .iter()
        .any(|n| matches!(n, SceneBodyNode::Bind(_)))
}

/// Whether `ir`'s scene has at least one `on` reaction whose selector
/// references a zellij-side event (`ark.zellij.<kind>`).
///
/// The v3 `OnNode` does not yet surface the raw event-kind string on the
/// typed AST (see `OnNode::selector` which is `#[facet(skip)]`), so this
/// walker inspects the preserved raw `KdlDocument` in [`SceneIR::kdl_doc`]
/// instead. That mirrors [`crate::validate::pane_views`], which also
/// walks the raw doc when the typed AST can't represent the check.
pub fn has_zellij_reactions(ir: &SceneIR) -> bool {
    let Some(doc) = ir.kdl_doc.as_ref() else {
        return false;
    };
    doc_has_zellij_on(doc)
}

/// Recurse through the KDL document tree looking for any `on` node whose
/// first positional argument string contains the ark.zellij namespace.
fn doc_has_zellij_on(doc: &KdlDocument) -> bool {
    for node in doc.nodes() {
        if node.name().value() == "on" && node_has_zellij_selector(node) {
            return true;
        }
        if let Some(children) = node.children() {
            if doc_has_zellij_on(children) {
                return true;
            }
        }
    }
    false
}

/// A single `on "<selector>"` node triggers injection when its first
/// positional argument (the event kind / selector string) contains the
/// zellij-side namespace marker [`ARK_BUS_EVENT_NEEDLE`].
fn node_has_zellij_selector(on: &KdlNode) -> bool {
    for entry in on.entries() {
        // Properties (`when="…"`, `field="pat"`) carry a non-None name.
        // The event selector is the first *positional* entry.
        if entry.name().is_some() {
            continue;
        }
        if let Some(raw) = entry.value().as_string() {
            return raw.contains(ARK_BUS_EVENT_NEEDLE);
        }
        // First non-property entry wasn't a string — unusual, but treat
        // as "no match" rather than panic: downstream parse validation
        // will surface the shape error.
        return false;
    }
    false
}

/// Whether the scene requires the ark-bus plugin to be mounted for
/// correct runtime behaviour. Logical OR of the two trigger detectors.
pub fn needs_ark_bus(ir: &SceneIR) -> bool {
    has_binds(ir) || has_zellij_reactions(ir)
}

/// Whether the rendered layout document already carries a peer
/// `plugin "ark-bus"` declaration — in which case we never inject a
/// second one.
fn already_has_ark_bus(doc: &KdlDocument) -> bool {
    doc.nodes().iter().any(|n| {
        if n.name().value() != "plugin" {
            return false;
        }
        n.entries().iter().any(|e| {
            e.name().is_none()
                && e.value()
                    .as_string()
                    .is_some_and(|s| s == ARK_BUS_PLUGIN_NAME)
        })
    })
}

/// Inject a peer `plugin "ark-bus" { source "shipped:ark-bus"; mount
/// "hidden" }` node into `doc` when the scene requires it and no
/// explicit declaration is already present. Returns `true` when an
/// injection happened, `false` otherwise. Useful for tests + tracing
/// without a separate `did_inject_ark_bus(&KdlDocument)` query.
pub fn inject_ark_bus_if_needed(doc: &mut KdlDocument, ir: &SceneIR) -> bool {
    if !needs_ark_bus(ir) {
        return false;
    }
    if already_has_ark_bus(doc) {
        return false;
    }
    let node = build_ark_bus_plugin_node();
    // Prepend so the declaration precedes `layout { }` in the rendered
    // artifact — cosmetic but keeps human-readable output grouped.
    doc.nodes_mut().insert(0, node);
    doc.autoformat();
    true
}

/// Construct a fresh `plugin "ark-bus" { source "shipped:ark-bus"; mount
/// "hidden" }` KDL node.
fn build_ark_bus_plugin_node() -> KdlNode {
    let mut plugin = KdlNode::new("plugin");
    plugin.push(KdlEntry::new(ARK_BUS_PLUGIN_NAME));

    let mut body = KdlDocument::new();

    let mut source_node = KdlNode::new("source");
    source_node.push(KdlEntry::new(ARK_BUS_SOURCE));
    body.nodes_mut().push(source_node);

    let mut mount_node = KdlNode::new("mount");
    mount_node.push(KdlEntry::new(ARK_BUS_MOUNT_TARGET));
    body.nodes_mut().push(mount_node);

    plugin.set_children(body);
    plugin
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn ir(src: &str) -> SceneIR {
        parse_scene(src, "test.kdl").expect("scene parses")
    }

    #[test]
    fn needs_ark_bus_true_for_binds() {
        let src = r#"scene "s" { bind "Alt p" { close "@x" } }"#;
        assert!(has_binds(&ir(src)));
        assert!(needs_ark_bus(&ir(src)));
    }

    #[test]
    fn needs_ark_bus_false_for_pure_agent_events() {
        let src = r#"
scene "s" {
    on "AgentReady" { close "@x" }
    on "ProgressUpdate" { close "@x" }
}
"#;
        assert!(!has_binds(&ir(src)));
        assert!(!has_zellij_reactions(&ir(src)));
        assert!(!needs_ark_bus(&ir(src)));
    }

    #[test]
    fn needs_ark_bus_true_for_userevent_ark_zellij_selector() {
        let src = r#"
scene "s" {
    on "UserEvent:ark.zellij.command_pane_exited" { close "@x" }
}
"#;
        assert!(has_zellij_reactions(&ir(src)));
        assert!(needs_ark_bus(&ir(src)));
    }

    #[test]
    fn needs_ark_bus_true_for_bare_ark_zellij_selector() {
        let src = r#"
scene "s" {
    on "ark.zellij.pane_closed" { close "@x" }
}
"#;
        assert!(has_zellij_reactions(&ir(src)));
    }

    #[test]
    fn needs_ark_bus_false_for_non_zellij_user_event() {
        let src = r#"
scene "s" {
    on "UserEvent:my.custom.event" { close "@x" }
}
"#;
        assert!(!has_zellij_reactions(&ir(src)));
        assert!(!needs_ark_bus(&ir(src)));
    }

    #[test]
    fn inject_ark_bus_adds_plugin_node() {
        let src = r#"scene "s" { bind "Alt p" { close "@x" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        // Seed with a layout node so the output mirrors a real
        // `compile_layout_kdl` result.
        doc.nodes_mut().push(KdlNode::new("layout"));

        assert!(inject_ark_bus_if_needed(&mut doc, &ir));

        let text = doc.to_string();
        assert!(text.contains("plugin"), "expected plugin node: {text}");
        assert!(text.contains("ark-bus"), "expected ark-bus name: {text}");
        assert!(
            text.contains("shipped:ark-bus"),
            "expected source URI: {text}"
        );
        assert!(text.contains("hidden"), "expected mount target: {text}");
        // And the doc still re-parses as valid KDL.
        KdlDocument::parse_v2(&text).expect("rendered doc must re-parse");
    }

    #[test]
    fn inject_ark_bus_noop_when_not_needed() {
        let src = r#"scene "s" { on "AgentReady" { close "@x" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(KdlNode::new("layout"));

        assert!(!inject_ark_bus_if_needed(&mut doc, &ir));
        let text = doc.to_string();
        assert!(
            !text.contains("ark-bus"),
            "pure-agent-event scene must not inject ark-bus: {text}"
        );
    }

    #[test]
    fn inject_ark_bus_noop_when_already_present() {
        let src = r#"scene "s" { bind "Alt p" { close "@x" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        // Pre-seed the doc with an explicit ark-bus declaration.
        doc.nodes_mut().push(build_ark_bus_plugin_node());
        doc.nodes_mut().push(KdlNode::new("layout"));

        assert!(!inject_ark_bus_if_needed(&mut doc, &ir));
        // Still exactly one plugin node.
        let plugin_count = doc
            .nodes()
            .iter()
            .filter(|n| n.name().value() == "plugin")
            .count();
        assert_eq!(plugin_count, 1);
    }

    #[test]
    fn inject_ark_bus_prepends_plugin_before_layout() {
        let src = r#"scene "s" { bind "Alt p" { close "@x" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(KdlNode::new("layout"));

        assert!(inject_ark_bus_if_needed(&mut doc, &ir));
        assert_eq!(doc.nodes().len(), 2);
        assert_eq!(doc.nodes()[0].name().value(), "plugin");
        assert_eq!(doc.nodes()[1].name().value(), "layout");
    }

    #[test]
    fn constants_are_stable() {
        // Guard the injected wire shape — downstream consumers
        // (T-070 intent dispatch, T-072 rebind endpoint, supervisor
        // mount logic) key off these exact strings.
        assert_eq!(ARK_BUS_PLUGIN_NAME, "ark-bus");
        assert_eq!(ARK_BUS_SOURCE, "shipped:ark-bus");
        assert_eq!(ARK_BUS_MOUNT_TARGET, "hidden");
        assert_eq!(ARK_BUS_EVENT_NEEDLE, "ark.zellij.");
    }

    #[test]
    fn nested_on_inside_mode_block_detected() {
        // Reactions declared inside a `mode { }` (future extension
        // surface) or any other nested container should still trigger
        // ark-bus injection — the detector walks the full document
        // tree.
        let src = r#"
scene "s" {
    mode "alt" {
        tab "@main" { }
    }
    on "UserEvent:ark.zellij.file_system_update" { close "@x" }
}
"#;
        assert!(has_zellij_reactions(&ir(src)));
    }
}
