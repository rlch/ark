//! Synthetic reactions generated from plugin lifecycle declarations.
//!
//! Implements the reaction-registry fan-out for T-7.3 (summon / dismiss),
//! T-7.4 (event-mount), and T-7.5 (subscribes forwarding). Each plugin
//! declaration produces one or more synthetic [`ReactionEntry`] values
//! that the shared reaction dispatcher (T-5.3) treats uniformly with
//! user-authored `on { }` reactions.
//!
//! # Tasks covered
//!
//! | Task   | Lifecycle       | Synthetic reactions                           |
//! |--------|-----------------|-----------------------------------------------|
//! | T-7.3  | `Summon`        | `summon` → `mount_plugin`, `dismiss` → `unmount_plugin` |
//! | T-7.4  | `EventMount`    | every `on { }` body → `mount_plugin`          |
//! | T-7.5  | Any lifecycle   | every `subscribes` selector → `pipe`          |
//!
//! # Why synthetic reactions
//!
//! Zellij's `launch-or-focus-plugin` primitive is already idempotent —
//! firing it against a live plugin focuses the pane; firing it against
//! a dormant plugin mounts and focuses. Representing lifecycle intent
//! as normal reactions therefore reduces the runtime to "on this event,
//! dispatch `mount_plugin`" with no extra state machine in the scene
//! layer. The supervisor's
//! [`crate::intent::ReactionOrigin::PluginLifecycle`] tag distinguishes
//! these reactions from user-authored ones for telemetry and scene
//! graph attribution.
//!
//! The [`crate::intent::ReactionOrigin`] enum grew a
//! [`crate::intent::ReactionOrigin::PluginLifecycle`] variant to
//! carry provenance; telemetry filters can target the synthetic path
//! without guessing from selector strings.
//!
//! # Dispatch depth
//!
//! Synthesised ops reuse the scene's `max-cascade-depth` because they
//! are indistinguishable from author-written ops at dispatch time. A
//! summon → mount_plugin → emit chain consumes one cascade step per
//! hop exactly as the user-scene path does.
//!
//! # TODOs
//!
//! * The `pipe` op's payload template currently reuses a static string —
//!   `"$EVENT_JSON"` interpolation via the T-2.5 template renderer
//!   lands when the template pass wires into op args generically. For
//!   v0.1 the synthetic `pipe` op captures the selector that matched
//!   plus the plugin name; the real JSON payload will arrive with the
//!   template-renderer integration.
//! * When the lowering pass extends `PluginDecl` with the `dismiss`
//!   selector / `on` selectors directly, this module stops needing to
//!   reach back into `PluginNode` for those fields. Today we accept
//!   the richer AST node directly so the lifecycle rewriting has full
//!   access.

use kdl::{KdlEntry, KdlNode, KdlValue};

use crate::ast::PluginNode;
use crate::intent::ReactionOrigin;
use crate::ops::dispatch::CompiledOp;
use crate::ops::Idempotency;
use crate::plugin::{Lifecycle, PluginDecl};
use crate::reactions::{EventKind, ReactionEntry, ReactionRegistry};

/// Provenance tag used by synthetic lifecycle reactions so telemetry
/// can distinguish them from user-authored scene reactions.
///
/// Set on every [`ReactionEntry`] produced by [`synthesise_plugin_reactions`].
pub const PLUGIN_LIFECYCLE_ORIGIN: ReactionOrigin = ReactionOrigin::PluginLifecycle;

/// Walk every `plugin { }` node in the scene and extend `registry` with
/// the synthetic reactions T-7.3 / T-7.4 / T-7.5 require.
///
/// The caller supplies the raw AST [`PluginNode`] list alongside the
/// lowered [`PluginDecl`] view so this pass can read the richer set of
/// fields (dismiss selector, PluginOn selectors) that `PluginDecl`
/// omits today — see the module-level TODO.
///
/// Ordering:
///
/// 1. Summon reactions per plugin with `Lifecycle::Summon`.
/// 2. Event-mount reactions for every `on { }` in a `Lifecycle::EventMount`
///    plugin.
/// 3. Dismiss reactions per plugin that declares a `dismiss` child.
/// 4. Subscribe / `pipe` forwarding reactions for every `subscribes`
///    child on every plugin regardless of lifecycle (R6: subscribes
///    forwards "regardless of mount state").
///
/// Returns the count of synthetic reactions appended so callers can
/// surface the wiring in debug logs.
pub fn synthesise_plugin_reactions(
    plugins: &[PluginNode],
    registry: &mut ReactionRegistry,
) -> Result<usize, Vec<crate::error::SceneError>> {
    let mut errors = Vec::new();
    let mut count = 0;

    for plugin in plugins {
        // Lower so we can gate on lifecycle; errors here are already
        // surfaced by the scene compile pipeline — we skip silently to
        // avoid double-reporting.
        let decl = match crate::plugin::lower_plugin(plugin) {
            Ok(d) => d,
            Err(_) => continue,
        };

        // T-7.3: Summon reaction — selector fires `mount_plugin name=<n>`.
        if decl.lifecycle == Lifecycle::Summon {
            if let Some(summon) = plugin.summon.as_ref() {
                match build_mount_reaction(&decl, &summon.selector) {
                    Ok(entry) => {
                        insert_for_selector(registry, &summon.selector, entry, &mut errors);
                        count += 1;
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // T-7.4: Event-mount reactions — every plugin-body `on` selector
        // fires `mount_plugin name=<n>`.
        if decl.lifecycle == Lifecycle::EventMount {
            for on in &plugin.on {
                match build_mount_reaction(&decl, &on.selector) {
                    Ok(entry) => {
                        insert_for_selector(registry, &on.selector, entry, &mut errors);
                        count += 1;
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // Dismiss reaction — fires `unmount_plugin name=<n>` when the
        // dismiss selector matches. Applies to both Summon and
        // EventMount lifecycles (Always plugins have no dismiss per R6).
        if decl.lifecycle != Lifecycle::Always {
            if let Some(dismiss) = plugin.dismiss.as_ref() {
                match build_unmount_reaction(&decl, &dismiss.selector) {
                    Ok(entry) => {
                        insert_for_selector(registry, &dismiss.selector, entry, &mut errors);
                        count += 1;
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // T-7.5: Subscribes — each selector fires `pipe plugin=<n> ...`
        // carrying the matched event's JSON. Lifecycle-agnostic.
        for sub in &plugin.subscribes {
            match build_pipe_reaction(&decl, &sub.selector) {
                Ok(entry) => {
                    insert_for_selector(registry, &sub.selector, entry, &mut errors);
                    count += 1;
                }
                Err(e) => errors.push(e),
            }
        }
    }

    if errors.is_empty() {
        Ok(count)
    } else {
        Err(errors)
    }
}

/// Build a synthetic `mount_plugin name="<plugin>"` reaction for
/// `selector`. Shared between summon and event-mount paths.
fn build_mount_reaction(
    decl: &PluginDecl<'_>,
    selector: &str,
) -> Result<ReactionEntry, crate::error::SceneError> {
    let ops = vec![CompiledOp::new(
        "ark.core.mount_plugin",
        Idempotency::LaunchOrFocus,
        build_mount_plugin_node(decl),
    )];
    Ok(ReactionEntry {
        selector: selector.to_string(),
        predicate: None,
        ops,
        origin: PLUGIN_LIFECYCLE_ORIGIN,
    })
}

/// Build a synthetic `unmount_plugin name="<plugin>"` reaction for
/// `selector`. Used by dismiss handling.
fn build_unmount_reaction(
    decl: &PluginDecl<'_>,
    selector: &str,
) -> Result<ReactionEntry, crate::error::SceneError> {
    let mut node = KdlNode::new("unmount_plugin");
    node.push(KdlEntry::new_prop(
        "name",
        KdlValue::String(decl.name.to_string()),
    ));
    let ops = vec![CompiledOp::new(
        "ark.core.unmount_plugin",
        Idempotency::NoopOnAbsent,
        node,
    )];
    Ok(ReactionEntry {
        selector: selector.to_string(),
        predicate: None,
        ops,
        origin: PLUGIN_LIFECYCLE_ORIGIN,
    })
}

/// Build a synthetic `pipe` reaction for the `subscribes` selector that
/// forwards the matching event's JSON serialisation to `<plugin>`.
///
/// The op shape matches the real [`crate::ops::messaging::PipeArgs`]
/// schema:
///
/// ```kdl
/// pipe plugin="<name>" name="ark-event" {
///     json "<rendered-event-json>"
/// }
/// ```
///
/// The `name="ark-event"` property routes the message to the plugin's
/// `pipe_handler` under the ark-event channel — the convention every
/// shipped plugin subscribes on.
///
/// The `json` child's body is a template string `"{{event_json}}"` which
/// the T-2.5 template renderer resolves at dispatch time to the matching
/// event's canonical `events.jsonl` JSON form. For dispatches where the
/// renderer has not yet wired in, the literal template string is forwarded
/// verbatim — the plugin treats malformed JSON as a no-op.
///
/// TODO(post-v0.1): thread the real `AgentEvent` through the context so
/// the `{{event_json}}` template resolves to the authoritative
/// `serde_json::to_string(&AgentEvent)` form. The template pass already
/// substitutes `{{…}}` tokens; this op's payload inherits the work when
/// the renderer lands in the reaction dispatcher (T-2.5 integration).
fn build_pipe_reaction(
    decl: &PluginDecl<'_>,
    selector: &str,
) -> Result<ReactionEntry, crate::error::SceneError> {
    // Root `pipe` node with property slots matching `PipeArgs`.
    let mut node = KdlNode::new("pipe");
    node.push(KdlEntry::new_prop(
        "plugin",
        KdlValue::String(decl.name.to_string()),
    ));
    node.push(KdlEntry::new_prop(
        "name",
        KdlValue::String("ark-event".to_string()),
    ));

    // `json "<template>"` child — the template renderer substitutes
    // `{{event_json}}` at dispatch time.
    let mut json_child = KdlNode::new("json");
    json_child.push(KdlEntry::new(KdlValue::String("{{event_json}}".to_string())));
    let mut body = ::kdl::KdlDocument::new();
    body.nodes_mut().push(json_child);
    node.set_children(body);

    let ops = vec![CompiledOp::new(
        "ark.core.pipe",
        Idempotency::AlwaysSideEffect,
        node,
    )];
    Ok(ReactionEntry {
        selector: selector.to_string(),
        predicate: None,
        ops,
        origin: PLUGIN_LIFECYCLE_ORIGIN,
    })
}

/// Build the `mount_plugin name="<n>" [at="<target>"]` KDL node that
/// backs every synthetic mount reaction.
fn build_mount_plugin_node(decl: &PluginDecl<'_>) -> KdlNode {
    let mut node = KdlNode::new("mount_plugin");
    node.push(KdlEntry::new_prop(
        "name",
        KdlValue::String(decl.name.to_string()),
    ));
    if let Some(target) = decl.mount {
        node.push(KdlEntry::new_prop(
            "at",
            KdlValue::String(target.to_string()),
        ));
    }
    node
}

/// Parse the selector's kind prefix and insert the entry into `registry`
/// under the correct `EventKind`. Unknown kinds are collected in
/// `errors` so the caller can surface them once; this mirrors the
/// error-accumulation pattern used by [`crate::reactions::populate_registry`].
fn insert_for_selector(
    registry: &mut ReactionRegistry,
    selector: &str,
    entry: ReactionEntry,
    errors: &mut Vec<crate::error::SceneError>,
) {
    let (kind, user_event_name) = parse_selector_kind(selector);
    let Some(kind) = kind else {
        errors.push(crate::error::SceneError::Grammar {
            message: format!("unknown event kind in plugin-lifecycle selector `{selector}`"),
            src: miette::NamedSource::new("<plugin-lifecycle>", selector.to_string()),
            at: (0, selector.len()).into(),
        });
        return;
    };
    registry.insert(kind, user_event_name, entry);
}

/// Copy of `reactions::parse_selector_kind` (private there) — lifted
/// here to avoid exposing the helper through `ReactionRegistry`'s
/// public surface for what is a one-caller dependency today.
fn parse_selector_kind(selector: &str) -> (Option<EventKind>, Option<String>) {
    let head = selector.split_whitespace().next().unwrap_or("");
    if let Some(rest) = head.strip_prefix("UserEvent:") {
        return (Some(EventKind::UserEvent), Some(rest.to_string()));
    }
    if let Some(rest) = head.strip_prefix("user_event:") {
        return (Some(EventKind::UserEvent), Some(rest.to_string()));
    }
    (EventKind::parse(head), None)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.kdl")
    }

    fn parse(src: &str) -> crate::ast::SceneDoc {
        parse_scene(src, &p()).expect("parse")
    }

    // ---- T-7.3: summon + dismiss ----------------------------------------

    #[test]
    fn summon_lifecycle_produces_mount_reaction_keyed_by_selector() {
        let doc = parse(
            r#"scene "s" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let count = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry)
            .expect("synthesis ok");
        assert_eq!(count, 1);

        // UserEvent:picker.show lands in both primary (UserEvent) and
        // secondary (by name) indices.
        let entries = registry.by_user_event_name("picker.show");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.selector, "UserEvent:picker.show");
        assert_eq!(entry.origin, PLUGIN_LIFECYCLE_ORIGIN);
        assert_eq!(entry.ops.len(), 1);
        assert_eq!(entry.ops[0].name, "ark.core.mount_plugin");
        assert_eq!(entry.ops[0].idempotency, Idempotency::LaunchOrFocus);
    }

    #[test]
    fn summon_with_dismiss_produces_matched_mount_and_unmount_reactions() {
        let doc = parse(
            r#"scene "s" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
        dismiss "UserEvent:picker.hide"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();

        let show = registry.by_user_event_name("picker.show");
        assert_eq!(show.len(), 1);
        assert_eq!(show[0].ops[0].name, "ark.core.mount_plugin");

        let hide = registry.by_user_event_name("picker.hide");
        assert_eq!(hide.len(), 1);
        assert_eq!(hide[0].ops[0].name, "ark.core.unmount_plugin");
        assert_eq!(hide[0].ops[0].idempotency, Idempotency::NoopOnAbsent);
    }

    // ---- T-7.4: event-mount ---------------------------------------------

    #[test]
    fn event_mount_lifecycle_produces_mount_per_on_selector() {
        let doc = parse(
            r#"scene "s" {
    plugin "diff" {
        source "shipped:diff"
        mount "floating"
        on "UserEvent:tool.file_edited"
        on "UserEvent:tool.file_created"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let count = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();
        assert_eq!(count, 2);

        assert_eq!(
            registry.by_user_event_name("tool.file_edited").len(),
            1
        );
        assert_eq!(
            registry.by_user_event_name("tool.file_created").len(),
            1
        );
    }

    #[test]
    fn event_mount_lifecycle_with_dismiss_pairs_mount_with_unmount() {
        let doc = parse(
            r#"scene "s" {
    plugin "diff" {
        source "shipped:diff"
        mount "floating"
        on "UserEvent:tool.started"
        dismiss "UserEvent:tool.done"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();

        assert_eq!(
            registry.by_user_event_name("tool.started")[0].ops[0].name,
            "ark.core.mount_plugin"
        );
        assert_eq!(
            registry.by_user_event_name("tool.done")[0].ops[0].name,
            "ark.core.unmount_plugin"
        );
    }

    // ---- T-7.5: subscribes pipe forwarding ------------------------------

    #[test]
    fn subscribes_produces_pipe_reaction_lifecycle_agnostic() {
        // Always-on plugin with subscribes.
        let doc = parse(
            r#"scene "s" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
        subscribes "UserEvent:agent.tick"
        subscribes "PhaseTransition"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let count = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();
        assert_eq!(count, 2);

        let tick = registry.by_user_event_name("agent.tick");
        assert_eq!(tick.len(), 1);
        let op = &tick[0].ops[0];
        assert_eq!(op.name, "ark.core.pipe");
        assert_eq!(op.idempotency, Idempotency::AlwaysSideEffect);
        // The synthetic node carries plugin=<name> + name="ark-event" +
        // a `json "{{event_json}}"` child for template rendering.
        let plugin_entry = op
            .node
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("plugin"))
            .expect("plugin prop");
        assert_eq!(plugin_entry.value().as_string(), Some("status"));
        let name_entry = op
            .node
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("name"))
            .expect("name prop");
        assert_eq!(name_entry.value().as_string(), Some("ark-event"));
        let body = op.node.children().expect("pipe has body");
        let json_child = body.nodes().iter().find(|n| n.name().value() == "json");
        assert!(json_child.is_some(), "pipe body has `json` child");

        // PhaseTransition selector → primary index.
        let phase_entries = registry.by_kind(&EventKind::PhaseTransition);
        assert_eq!(phase_entries.len(), 1);
        assert_eq!(phase_entries[0].ops[0].name, "ark.core.pipe");
    }

    #[test]
    fn subscribes_coexists_with_summon_and_dismiss() {
        let doc = parse(
            r#"scene "s" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
        dismiss "UserEvent:picker.hide"
        subscribes "UserEvent:picker.item_changed"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let count = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();
        assert_eq!(count, 3, "summon + dismiss + subscribes each produce one");
    }

    // ---- Always lifecycle: no summon/dismiss reactions but subscribes OK

    #[test]
    fn always_lifecycle_synthesises_only_subscribes_reactions() {
        let doc = parse(
            r#"scene "s" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
        subscribes "UserEvent:agent.tick"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let count = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();
        assert_eq!(count, 1, "only the subscribes reaction");
        assert!(registry.by_user_event_name("agent.tick").first().is_some());
    }

    // ---- Error path: unknown selector kind -----------------------------

    #[test]
    fn unknown_selector_kind_is_grammar_error() {
        let doc = parse(
            r#"scene "s" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
        summon "NotAKind"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        let errs = synthesise_plugin_reactions(&doc.scene.plugins, &mut registry)
            .expect_err("unknown kind must surface");
        assert!(errs.iter().any(|e| matches!(
            e,
            crate::error::SceneError::Grammar { .. }
        )));
    }

    // ---- T-7.5 integration: subscribes pipe op dispatches --------------

    /// The synthesised `subscribes -> pipe` reaction dispatches
    /// end-to-end through the intent registry without panicking. The
    /// pipe op is a stub at this tier (logs + returns Ok) so the
    /// assertion is simply "the dispatcher accepts the synthesised
    /// node shape" — which requires the `pipe plugin=<n> name="ark-event"
    /// { json "..." }` shape to match `PipeArgs`'s facet schema.
    #[tokio::test]
    async fn subscribes_synthesised_pipe_reaction_dispatches_end_to_end() {
        use crate::id::SceneId;
        use crate::intent::{IntentContext, IntentRegistry};
        use crate::ops::dispatch::dispatch_sequence;
        use crate::ops::register_core_ops;

        let doc = parse(
            r#"scene "s" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
        subscribes "UserEvent:agent.tick"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();

        let entries = registry.by_user_event_name("agent.tick");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        let intents = IntentRegistry::new();
        register_core_ops(&intents).await;
        let ctx = IntentContext::placeholder(SceneId::from_bytes(
            std::path::PathBuf::from("/tmp/s.kdl"),
            b"scene",
        ));

        dispatch_sequence(&entry.ops, &intents, &ctx)
            .await
            .expect("pipe op dispatches cleanly");
    }

    // ---- T-7.4 integration: dispatch via intent registry --------------

    /// End-to-end: a `Lifecycle::EventMount` plugin's synthesised
    /// reaction dispatches the `mount_plugin` op through the real
    /// intent registry without panicking — the op sequence compiles,
    /// the registry resolves `ark.core.mount_plugin`, and the stub
    /// returns `Ok(None)`. Subsequent matches hit the same op which is
    /// idempotent at the mux layer; the reaction dispatcher treats
    /// multi-fires as separate `launch-or-focus` attempts.
    #[tokio::test]
    async fn event_mount_synthesised_reaction_dispatches_mount_op_end_to_end() {
        use crate::id::SceneId;
        use crate::intent::{IntentContext, IntentRegistry};
        use crate::ops::dispatch::dispatch_sequence;
        use crate::ops::register_core_ops;

        let doc = parse(
            r#"scene "s" {
    plugin "diff" {
        source "shipped:diff"
        mount "floating"
        on "UserEvent:tool.file_edited"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();

        let entries = registry.by_user_event_name("tool.file_edited");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        let intents = IntentRegistry::new();
        register_core_ops(&intents).await;
        let ctx = IntentContext::placeholder(SceneId::from_bytes(
            std::path::PathBuf::from("/tmp/s.kdl"),
            b"scene",
        ));

        // First dispatch: stub returns Ok; no panic.
        dispatch_sequence(&entry.ops, &intents, &ctx)
            .await
            .expect("first mount succeeds");

        // Second dispatch: same selector, same ops — idempotent
        // focus-on-already-mounted via launch-or-focus-plugin.
        dispatch_sequence(&entry.ops, &intents, &ctx)
            .await
            .expect("second mount (focus) succeeds");
    }

    // ---- Provenance tag is set on every synthesised entry --------------

    #[test]
    fn synthesised_entries_carry_plugin_lifecycle_origin() {
        let doc = parse(
            r#"scene "s" {
    plugin "p" {
        source "shipped:p"
        mount "floating"
        summon "UserEvent:x"
        subscribes "UserEvent:y"
    }
}
"#,
        );
        let mut registry = ReactionRegistry::new();
        synthesise_plugin_reactions(&doc.scene.plugins, &mut registry).unwrap();
        for entries in registry.iter_primary().map(|(_, v)| v) {
            for entry in entries {
                assert_eq!(entry.origin, PLUGIN_LIFECYCLE_ORIGIN);
            }
        }
    }
}
