//! Auto-inject `plugin "ark-bus"` (T-6.7).
//!
//! Per `cavekit-scene.md` R6 + the T-6.7 entry: any scene that uses a
//! feature requiring the `ark-bus` plugin must declare it. Rather than
//! force every scene author to write the boilerplate, the compile
//! pipeline detects the trigger conditions and splices the plugin
//! declaration in automatically.
//!
//! ## When to inject
//!
//! Inject if **any** of:
//!
//! 1. The scene declares one or more `keybind` nodes. Keybinds compile
//!    to `MessagePlugin "ark-bus" name="ark-intent" payload=…` actions
//!    (T-6.5), so ark-bus must be loaded to receive the pipe message.
//! 2. Any scene-root `on { }` reaction's selector references a
//!    zellij-side event that ark-bus is responsible for forwarding
//!    (T-6.3) — the canonical UserEvent name is
//!    `UserEvent:ark.zellij.<kind>` for `kind ∈ {command_pane_opened,
//!    command_pane_exited, pane_closed, file_system_update}`.
//! 3. Any declared plugin's `subscribes` selector references one of the
//!    same zellij-side UserEvent names — same rationale.
//!
//! ## When NOT to inject
//!
//! - The scene already declares `plugin "ark-bus" { … }` somewhere
//!   (user override). We never silently shadow an explicit declaration.
//! - The scene has zero keybinds AND no zellij-side UserEvent
//!   subscribers. Pure-AgentEvent scenes (e.g. status-only setups) save
//!   one plugin load per session by skipping the injection.
//!
//! ## Mount shape
//!
//! The injected plugin uses `mount "hidden"` — zellij's first-class
//! suppressed-pane API. Precedent: zellij-autolock. ark-bus is
//! headless (no rendered surface) and mount-hidden is the
//! semantically-correct way to express that to zellij; it is NOT a
//! geometry hack like `size 0`.

use crate::ast::{
    DismissNode, MountNode, OpaqueBlock, PluginNode, SceneNode, SourceNode, SummonNode,
};

/// Plugin name expected by every other ark component when it talks to
/// the bus. Pinned as a constant so the injector and consumers (T-6.5,
/// T-6.4) all agree on a single identifier.
pub const ARK_BUS_PLUGIN_NAME: &str = "ark-bus";

/// `source` URI for the auto-injected ark-bus plugin. `shipped:` =
/// resolved out of the ark distribution rather than a user extension.
pub const ARK_BUS_SOURCE: &str = "shipped:ark-bus";

/// `mount` target — zellij's hidden / suppressed-pane API. ark-bus is
/// headless so this is the right semantic shape; `size 0` would also
/// hide it but is a geometry hack.
pub const ARK_BUS_MOUNT_TARGET: &str = "hidden";

/// Canonical UserEvent prefix for zellij-side events forwarded by
/// ark-bus (T-6.3). Selectors of the form
/// `UserEvent:ark.zellij.<kind>` trigger injection so the broadcaster
/// is actually present at session-spawn.
pub const ARK_BUS_EVENT_PREFIX: &str = "UserEvent:ark.zellij.";

/// Mutate `scene` in place, injecting an `ark-bus` plugin declaration
/// when one of the trigger conditions matches and the scene does not
/// already declare it.
///
/// Returns `true` when an injection happened, `false` otherwise.
/// Useful for tests + tracing without a separate
/// `did_inject_ark_bus(&SceneNode)` query.
pub fn maybe_inject_ark_bus(scene: &mut SceneNode) -> bool {
    if has_ark_bus_plugin(scene) {
        return false;
    }
    if !needs_ark_bus(scene) {
        return false;
    }
    scene.plugins.push(synthesise_ark_bus_plugin());
    true
}

/// Whether the scene already declares a plugin named `ark-bus`. The
/// match is exact (the plugin name is a stable identifier — no glob).
fn has_ark_bus_plugin(scene: &SceneNode) -> bool {
    scene
        .plugins
        .iter()
        .any(|p| p.name == ARK_BUS_PLUGIN_NAME)
}

/// Whether any of the three trigger conditions (T-6.7) holds.
fn needs_ark_bus(scene: &SceneNode) -> bool {
    if !scene.keybinds.is_empty() {
        return true;
    }
    if scene
        .ons
        .iter()
        .any(|on| selector_targets_zellij_side(&on.selector))
    {
        return true;
    }
    for plugin in &scene.plugins {
        if plugin
            .subscribes
            .iter()
            .any(|s| selector_targets_zellij_side(&s.selector))
        {
            return true;
        }
    }
    false
}

/// Whether a selector string targets a zellij-side UserEvent — i.e.
/// starts with `UserEvent:ark.zellij.`.
///
/// We trim leading whitespace before matching so authors can write the
/// selector with cosmetic indentation. Trailing content (concrete event
/// kind, optional `field=value` sugar) is ignored — the prefix match is
/// sufficient because every ark-bus-emitted event lives under that
/// prefix.
fn selector_targets_zellij_side(selector: &str) -> bool {
    selector.trim_start().starts_with(ARK_BUS_EVENT_PREFIX)
}

/// Build a fresh `PluginNode` representing the auto-injected ark-bus.
///
/// All optional fields default to their absent forms — only `name`,
/// `source`, and `mount` are populated because R6 requires them. Other
/// fields (`summon`, `on`, `subscribes`, `config`) stay empty so the
/// plugin is in `Lifecycle::Always` (per
/// `crate::plugin::Lifecycle::Always`).
fn synthesise_ark_bus_plugin() -> PluginNode {
    PluginNode {
        name: ARK_BUS_PLUGIN_NAME.to_string(),
        override_: None,
        source: Some(SourceNode {
            uri: ARK_BUS_SOURCE.to_string(),
        }),
        mount: Some(MountNode {
            target: ARK_BUS_MOUNT_TARGET.to_string(),
            into: None,
            split: None,
            size: None,
            x: None,
            y: None,
            width: None,
            height: None,
        }),
        summon: None as Option<SummonNode>,
        dismiss: None as Option<DismissNode>,
        on: Vec::new(),
        subscribes: Vec::new(),
        config: None as Option<OpaqueBlock>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SceneDoc;

    fn parse(input: &str) -> SceneDoc {
        facet_kdl::from_str(input).expect("scene parses")
    }

    /// A scene with one keybind triggers injection.
    #[test]
    fn keybind_triggers_injection() {
        let mut doc = parse(
            r#"scene "s" {
                keybind "Alt p" intent="picker.show"
            }"#,
        );
        assert_eq!(doc.scene.plugins.len(), 0);
        let injected = maybe_inject_ark_bus(&mut doc.scene);
        assert!(injected);
        assert_eq!(doc.scene.plugins.len(), 1);
        let p = &doc.scene.plugins[0];
        assert_eq!(p.name, ARK_BUS_PLUGIN_NAME);
        assert_eq!(
            p.source.as_ref().map(|s| s.uri.as_str()),
            Some(ARK_BUS_SOURCE)
        );
        assert_eq!(
            p.mount.as_ref().map(|m| m.target.as_str()),
            Some(ARK_BUS_MOUNT_TARGET)
        );
    }

    /// A scene with no keybinds and no zellij-side subscriptions does
    /// NOT trigger injection.
    #[test]
    fn pure_agent_event_scene_not_injected() {
        let mut doc = parse(
            r#"scene "s" {
                on "AgentReady" { }
                on "ProgressUpdate" { }
            }"#,
        );
        let injected = maybe_inject_ark_bus(&mut doc.scene);
        assert!(!injected);
        assert!(doc.scene.plugins.is_empty());
    }

    /// `on "UserEvent:ark.zellij.command_pane_exited"` triggers.
    #[test]
    fn zellij_side_user_event_in_on_triggers_injection() {
        let mut doc = parse(
            r#"scene "s" {
                on "UserEvent:ark.zellij.command_pane_exited" { }
            }"#,
        );
        assert!(maybe_inject_ark_bus(&mut doc.scene));
        assert!(
            doc.scene
                .plugins
                .iter()
                .any(|p| p.name == ARK_BUS_PLUGIN_NAME)
        );
    }

    /// `subscribes "UserEvent:ark.zellij.pane_closed"` inside another
    /// plugin triggers.
    #[test]
    fn zellij_side_subscribes_in_plugin_triggers_injection() {
        let mut doc = parse(
            r#"scene "s" {
                plugin "ark-status" {
                    source "shipped:ark-status"
                    mount "status-bar"
                    subscribes "UserEvent:ark.zellij.pane_closed"
                }
            }"#,
        );
        // Pre-injection: only ark-status declared.
        assert_eq!(doc.scene.plugins.len(), 1);
        assert!(maybe_inject_ark_bus(&mut doc.scene));
        assert_eq!(doc.scene.plugins.len(), 2);
        assert!(
            doc.scene
                .plugins
                .iter()
                .any(|p| p.name == ARK_BUS_PLUGIN_NAME)
        );
        assert!(
            doc.scene
                .plugins
                .iter()
                .any(|p| p.name == "ark-status")
        );
    }

    /// A scene that already declares `plugin "ark-bus"` is not
    /// duplicated.
    #[test]
    fn explicit_ark_bus_declaration_not_duplicated() {
        let mut doc = parse(
            r#"scene "s" {
                plugin "ark-bus" {
                    source "shipped:ark-bus"
                    mount "hidden"
                }
                keybind "Alt p" intent="picker.show"
            }"#,
        );
        assert_eq!(doc.scene.plugins.len(), 1);
        let injected = maybe_inject_ark_bus(&mut doc.scene);
        assert!(!injected, "should not duplicate explicit declaration");
        assert_eq!(doc.scene.plugins.len(), 1);
    }

    /// Selectors that don't match the zellij-side prefix don't trigger.
    #[test]
    fn non_zellij_user_events_do_not_trigger() {
        let mut doc = parse(
            r#"scene "s" {
                on "UserEvent:my.custom.event" { }
                on "AgentReady" { }
            }"#,
        );
        assert!(!maybe_inject_ark_bus(&mut doc.scene));
        assert!(doc.scene.plugins.is_empty());
    }

    /// Selector with leading whitespace is matched after trim.
    #[test]
    fn selector_leading_whitespace_handled() {
        assert!(selector_targets_zellij_side(
            "  UserEvent:ark.zellij.pane_closed"
        ));
    }

    /// Multiple triggers don't cause multiple injections — the
    /// detector is "any of", inject once.
    #[test]
    fn multiple_triggers_inject_once() {
        let mut doc = parse(
            r#"scene "s" {
                keybind "Alt p" intent="picker.show"
                on "UserEvent:ark.zellij.pane_closed" { }
            }"#,
        );
        let injected = maybe_inject_ark_bus(&mut doc.scene);
        assert!(injected);
        assert_eq!(
            doc.scene
                .plugins
                .iter()
                .filter(|p| p.name == ARK_BUS_PLUGIN_NAME)
                .count(),
            1
        );
    }

    /// The injected plugin's lifecycle inference (T-7.1) resolves to
    /// `Lifecycle::Always` — it has no `summon` or body-`on`.
    #[test]
    fn injected_plugin_has_always_lifecycle() {
        let mut doc = parse(
            r#"scene "s" {
                keybind "Alt p" intent="picker.show"
            }"#,
        );
        maybe_inject_ark_bus(&mut doc.scene);
        let p = doc
            .scene
            .plugins
            .iter()
            .find(|p| p.name == ARK_BUS_PLUGIN_NAME)
            .expect("injected");
        assert!(p.summon.is_none());
        assert!(p.on.is_empty());
    }
}
