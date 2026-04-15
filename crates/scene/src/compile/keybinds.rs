//! Lower scene `keybind` declarations into a zellij-compatible
//! `keybinds { … }` KDL block (R5).
//!
//! Each scene `keybind "<chord>" intent="<name>"` (or block-form
//! `keybind "<chord>" { <ops>+ }`) becomes a single zellij keybind
//! action that posts a JSON intent payload to the `ark-bus` plugin
//! via `MessagePlugin "ark-bus" name="ark-intent" payload=<json>`.
//! The bus then relays through `ark-hook intent` to the supervisor's
//! intent registry (T-6.2).
//!
//! ## Where the rendered block lives
//!
//! The output is a **top-level** `keybinds { … }` node, sibling to
//! `layout { }` — NOT nested inside it. Per cavekit-scene.md R5:
//!
//! > Zellij merges additively with user config (no `clear-defaults`).
//!
//! Placing `keybinds { … }` at the layout-file root means the user's
//! own keybind config (typically in `~/.config/zellij/config.kdl`)
//! merges with ours additively rather than being shadowed. This is the
//! upstream-zellij idiom; nesting under `layout { }` would be silently
//! ignored.
//!
//! ## Default mode
//!
//! v1 emits every binding under the `Normal` mode by default. The R5
//! grammar does not surface a `mode=` attribute on `keybind` yet —
//! when it does (later tier), this pass will route the binding to the
//! right mode block.
//!
//! ## intent= shorthand vs block-form
//!
//! Both shapes compile to the same KeybindPipe action:
//!
//! - `keybind "Alt p" intent="picker.show"` → payload
//!   `{"name":"picker.show","args":{}}`.
//! - `keybind "Alt p" { open_tab name="build" }` → payload synthesised
//!   from the first op in the body, with KDL props rendered as a JSON
//!   object. Multi-op bodies are deferred — the v1 dispatch path only
//!   surfaces a single intent per chord. Multi-op chords surface as a
//!   `Grammar` error so authors split into separate chords or emit a
//!   single composite op.
//!
//! ## Errors
//!
//! - Empty chord string → `Grammar` (caller should also have caught
//!   this in the chord validator T-6.6).
//! - Both `intent=` AND a non-empty body → `Grammar` (R5: "Mutually
//!   exclusive with a non-empty body").
//! - Block-form body with more than one op → `Grammar` (v1 limitation).

use kdl::{KdlDocument, KdlEntry, KdlNode};
use miette::NamedSource;
use serde_json::Value as JsonValue;

use crate::ast::{KeybindNode, OpNode};
use crate::error::SceneError;

/// Default zellij `InputMode` keybinds are emitted under. v1 ships
/// every scene keybind in `Normal`; future tiers may surface a
/// `mode=` attribute on `keybind` to override per-binding.
pub const DEFAULT_MODE: &str = "normal";

/// The plugin name targeted by every emitted MessagePlugin action.
/// Pinned to the `ark-bus` constant so the dispatch side and this
/// emitter share a single source of truth.
pub const TARGET_PLUGIN: &str = "ark-bus";

/// The pipe message name used by the dispatch path. Mirrors
/// `ark_bus::PIPE_INTENT` (kept as a string literal here to avoid a
/// cross-crate dep — `ark-bus` is a wasm plugin and pulling it as a
/// dep of the scene crate would forbid the workspace check from
/// running on host targets).
pub const PIPE_MESSAGE_NAME: &str = "ark-intent";

/// Compile a slice of [`KeybindNode`] into a zellij `keybinds { … }`
/// document.
///
/// Returns `Ok(None)` when there are no keybinds — the caller (R5 +
/// R15 path) emits the layout document on its own without a sibling
/// `keybinds` block. Returns `Ok(Some(KdlNode))` carrying the full
/// `keybinds { normal { "Alt p" { MessagePlugin … } … } }` tree
/// otherwise.
///
/// The pass does NOT validate the chord string against zellij's lexer
/// — that's T-6.6's job. We do reject obviously-broken cases
/// (empty, both `intent=` and a body, multi-op body) so downstream
/// renderers see a well-formed document.
pub fn compile_keybinds(
    keybinds: &[KeybindNode],
) -> Result<Option<KdlNode>, SceneError> {
    if keybinds.is_empty() {
        return Ok(None);
    }

    let mut keybinds_node = KdlNode::new("keybinds");
    let mut keybinds_children = KdlDocument::new();

    let mut mode_node = KdlNode::new(DEFAULT_MODE);
    let mut mode_children = KdlDocument::new();

    for kb in keybinds {
        let bind_node = compile_one_keybind(kb)?;
        mode_children.nodes_mut().push(bind_node);
    }

    mode_node.set_children(mode_children);
    keybinds_children.nodes_mut().push(mode_node);
    keybinds_node.set_children(keybinds_children);
    Ok(Some(keybinds_node))
}

/// Compile a single keybind into the `bind "<chord>" { MessagePlugin … }`
/// shape.
fn compile_one_keybind(kb: &KeybindNode) -> Result<KdlNode, SceneError> {
    if kb.chord.trim().is_empty() {
        return Err(SceneError::Grammar {
            message: "keybind chord string is empty".to_string(),
            src: NamedSource::new("<keybind>", String::new()),
            at: (0, 0).into(),
        });
    }

    // R5: intent= and body are mutually exclusive.
    let has_intent = kb.intent.is_some();
    let has_body = !kb.ops.is_empty();
    if has_intent && has_body {
        return Err(SceneError::Grammar {
            message: format!(
                "keybind `{}` declares both `intent=` and a non-empty body — pick one",
                kb.chord
            ),
            src: NamedSource::new("<keybind>", String::new()),
            at: (0, 0).into(),
        });
    }

    // v1 limitation: block-form bodies must have exactly one op so we
    // can synthesise a single intent payload. Multi-op bodies are
    // deferred — see module docs.
    if has_body && kb.ops.len() > 1 {
        return Err(SceneError::Grammar {
            message: format!(
                "keybind `{}` has {} ops in its body; v1 supports one op per chord (split into multiple chords)",
                kb.chord,
                kb.ops.len()
            ),
            src: NamedSource::new("<keybind>", String::new()),
            at: (0, 0).into(),
        });
    }

    // Resolve the intent name + a synthesised args object.
    //
    // Three cases:
    //   1. `intent="…"` shorthand — happy path, args is `{}`.
    //   2. Block-form body with one op — synthesise an intent payload
    //      from the op's positional args (placeholder until T-3.2
    //      typifies `OpNode`).
    //   3. Neither `intent=` nor a body — facet-kdl can produce this
    //      when the user wrote a block-form keybind whose body
    //      children didn't match the (still-opaque) `OpNode` slot.
    //      Treat it as the placeholder no-args dispatch under
    //      `ark.core.unknown` so the rendered keybind still routes
    //      somewhere predictable. T-3.2 + R5 stricter-validation will
    //      surface this as an error once the AST can tell the
    //      difference.
    let (intent_name, args_json) = if let Some(name) = &kb.intent {
        (name.clone(), JsonValue::Object(serde_json::Map::new()))
    } else if let Some(op) = kb.ops.first() {
        let intent = op_name_for(op);
        let args = synthesise_args_for_op(op);
        (intent, args)
    } else {
        // Empty body, no intent= — placeholder dispatch (see comment
        // above). Forward-looking: T-3.2 will treat this as a
        // `Grammar` error once `ops` faithfully reflects the parsed
        // body. The current behaviour intentionally keeps the
        // compiler total so authors can iterate scenes without the
        // pipeline aborting on body-not-yet-typed.
        (
            "ark.core.unknown".to_string(),
            JsonValue::Object(serde_json::Map::new()),
        )
    };

    // Build the JSON payload string. Compact form (single-line) so the
    // KDL property value is unambiguous.
    let payload_obj = serde_json::json!({
        "name": intent_name,
        "args": args_json,
    });
    let payload_str = payload_obj.to_string();

    // bind "<chord>" { MessagePlugin "<plugin>" { name "<pipe>"; payload "<json>"; }; }
    let mut bind_node = KdlNode::new("bind");
    bind_node
        .entries_mut()
        .push(KdlEntry::new(kb.chord.clone()));

    let mut bind_children = KdlDocument::new();

    let mut msg_node = KdlNode::new("MessagePlugin");
    msg_node
        .entries_mut()
        .push(KdlEntry::new(TARGET_PLUGIN.to_string()));

    let mut msg_children = KdlDocument::new();

    let mut name_node = KdlNode::new("name");
    name_node
        .entries_mut()
        .push(KdlEntry::new(PIPE_MESSAGE_NAME.to_string()));
    msg_children.nodes_mut().push(name_node);

    let mut payload_node = KdlNode::new("payload");
    payload_node.entries_mut().push(KdlEntry::new(payload_str));
    msg_children.nodes_mut().push(payload_node);

    msg_node.set_children(msg_children);
    bind_children.nodes_mut().push(msg_node);
    bind_node.set_children(bind_children);
    Ok(bind_node)
}

/// Resolve the intent name for an opaque [`OpNode`].
///
/// The current AST collapses every op body to its positional args (see
/// `OpNode` TODO T-3.2). The op's name (e.g. `open_tab`) lives in the
/// KDL parent — but because facet-kdl drops the discriminator at parse
/// time, we no longer have it on the ast type today.
///
/// As an interim, the keybind compiler treats every block-form body
/// as the namespaced `ark.core.<verb>` family with a placeholder verb
/// name `unknown`. Once T-3.2 typifies the op enum, this becomes a
/// match-arm per variant. Authors who want a specific intent today
/// should use the `intent="…"` shorthand instead of the block form.
fn op_name_for(_op: &OpNode) -> String {
    "ark.core.unknown".to_string()
}

/// Synthesise the JSON `args` object for a block-form op body.
///
/// See [`op_name_for`] for the typed-AST caveat. Until T-3.2 lands,
/// we emit `{"positional": [<args>...]}` so the supervisor's
/// `Intent` handler at least has the raw arguments to inspect.
fn synthesise_args_for_op(op: &OpNode) -> JsonValue {
    let positional: Vec<JsonValue> = op
        .args
        .iter()
        .map(|s| JsonValue::String(s.clone()))
        .collect();
    serde_json::json!({ "positional": positional })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::SceneDoc;

    fn parse(input: &str) -> SceneDoc {
        facet_kdl::from_str(input).expect("scene parses")
    }

    fn rendered_keybinds(input: &str) -> String {
        let doc = parse(input);
        let node = compile_keybinds(&doc.scene.keybinds)
            .expect("compile ok")
            .expect("at least one keybind");
        let mut wrapper = KdlDocument::new();
        wrapper.nodes_mut().push(node);
        wrapper.autoformat();
        wrapper.to_string()
    }

    #[test]
    fn empty_keybinds_returns_none() {
        let doc = parse(r#"scene "s" { }"#);
        let result = compile_keybinds(&doc.scene.keybinds).expect("ok");
        assert!(result.is_none(), "no keybinds = no block");
    }

    #[test]
    fn intent_shorthand_renders_message_plugin_action() {
        let rendered = rendered_keybinds(
            r#"scene "s" {
                keybind "Alt p" intent="picker.show"
            }"#,
        );
        // Re-parse to assert the structure rather than string-matching
        // the autoformat output.
        let parsed = KdlDocument::parse(&rendered).expect("re-parse");
        let kb_root = parsed
            .nodes()
            .iter()
            .find(|n| n.name().value() == "keybinds")
            .expect("keybinds node");
        let mode = kb_root
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == DEFAULT_MODE)
            .expect("normal mode");
        let bind = mode
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "bind")
            .expect("bind node");
        // Chord is the first positional entry.
        assert_eq!(bind.entries()[0].value().as_string(), Some("Alt p"));
        let msg = bind
            .children()
            .unwrap()
            .nodes()
            .iter()
            .find(|n| n.name().value() == "MessagePlugin")
            .expect("MessagePlugin node");
        assert_eq!(msg.entries()[0].value().as_string(), Some(TARGET_PLUGIN));
        // Inner `name` and `payload` children carry the intent envelope.
        let inner = msg.children().unwrap();
        let name_node = inner
            .nodes()
            .iter()
            .find(|n| n.name().value() == "name")
            .unwrap();
        assert_eq!(name_node.entries()[0].value().as_string(), Some("ark-intent"));
        let payload_node = inner
            .nodes()
            .iter()
            .find(|n| n.name().value() == "payload")
            .unwrap();
        let payload_str = payload_node.entries()[0]
            .value()
            .as_string()
            .expect("payload is a string");
        let payload_json: JsonValue =
            serde_json::from_str(payload_str).expect("payload parses as JSON");
        assert_eq!(payload_json["name"], "picker.show");
        assert!(payload_json["args"].is_object());
    }

    #[test]
    fn multiple_keybinds_render_in_source_order() {
        let rendered = rendered_keybinds(
            r#"scene "s" {
                keybind "Alt 1" intent="a.one"
                keybind "Alt 2" intent="a.two"
                keybind "Alt 3" intent="a.three"
            }"#,
        );
        let i_one = rendered.find("a.one").expect("first");
        let i_two = rendered.find("a.two").expect("second");
        let i_three = rendered.find("a.three").expect("third");
        assert!(i_one < i_two && i_two < i_three, "rendered: {rendered}");
    }

    /// Block-form keybinds are accepted by the parser but their bodies
    /// are opaque at the AST level today (`OpNode` is a placeholder per
    /// `crate::ast` TODO T-3.2 — facet-kdl matches children to fields
    /// by singular field name, and scene op verbs (`open_tab`, etc.)
    /// don't match the catch-all `op` slot). So `kb.ops` is empty for
    /// today's parser output, and the block form silently degrades to
    /// an empty-args dispatch under the placeholder intent name.
    /// Once T-3.2 typifies the op vocabulary, the block form will fire
    /// real intents; this test pins the v1 behaviour so the migration
    /// is testable end-to-end.
    #[test]
    fn keybind_block_form_is_accepted_but_args_empty_until_t32() {
        let doc = parse(
            r#"scene "s" {
                keybind "Alt o" {
                    open_tab "build"
                }
            }"#,
        );
        // Sanity: the parser kept the keybind even though its body is
        // semantically opaque.
        assert_eq!(doc.scene.keybinds.len(), 1);
        assert_eq!(doc.scene.keybinds[0].chord, "Alt o");
        // ops vec is empty per the AST T-3.2 caveat.
        assert!(
            doc.scene.keybinds[0].ops.is_empty(),
            "ops vec should be empty until T-3.2 typifies OpNode"
        );

        // Compiling with no body and no intent= falls into the
        // shorthand path with a synthesized empty intent. Today this
        // surfaces as an empty `intent` string upstream — the
        // compiler currently treats `intent: None && ops.is_empty()` as
        // an intent-less keybind, which is itself a no-op intent. We
        // therefore expect either a clean Grammar error OR a rendered
        // node — verify whichever the current implementation chose so
        // the regression surface is pinned.
        let result = compile_keybinds(&doc.scene.keybinds);
        // Today: succeeds with a placeholder intent payload (no body,
        // no intent=). This test pins the surface so a future change
        // (e.g. requiring `intent=` when body is empty) breaks it
        // visibly rather than silently.
        assert!(result.is_ok(), "today this currently succeeds: {result:?}");
    }

    /// `intent="…"` AND a non-empty body is rejected. Since the
    /// AST currently produces empty `ops` for unrecognised verb
    /// children, exercising the rejection path requires the body to
    /// actually populate `ops`. We construct a synthetic `KeybindNode`
    /// directly to bypass facet-kdl and pin the validator's reject
    /// branch.
    #[test]
    fn keybind_with_intent_and_body_is_rejected_synthetic() {
        let kb = KeybindNode {
            chord: "Alt p".to_string(),
            intent: Some("picker.show".to_string()),
            ops: vec![OpNode {
                args: vec!["x".to_string()],
            }],
        };
        let err = compile_keybinds(std::slice::from_ref(&kb)).expect_err("must reject");
        match err {
            SceneError::Grammar { message, .. } => {
                assert!(message.contains("intent="), "got: {message}");
            }
            other => panic!("expected Grammar, got {other:?}"),
        }
    }

    /// Multi-op block-form bodies are rejected. Same synthetic-AST
    /// trick — see the comment on the previous test.
    #[test]
    fn keybind_with_multi_op_body_is_rejected_synthetic() {
        let kb = KeybindNode {
            chord: "Alt o".to_string(),
            intent: None,
            ops: vec![
                OpNode {
                    args: vec!["a".to_string()],
                },
                OpNode {
                    args: vec!["b".to_string()],
                },
            ],
        };
        let err = compile_keybinds(std::slice::from_ref(&kb)).expect_err("must reject");
        match err {
            SceneError::Grammar { message, .. } => {
                assert!(message.contains("v1 supports one op"), "got: {message}");
            }
            other => panic!("expected Grammar, got {other:?}"),
        }
    }

    /// Empty chord is rejected with a Grammar error (chord-string
    /// validation proper lives in T-6.6).
    #[test]
    fn empty_chord_is_rejected_synthetic() {
        let kb = KeybindNode {
            chord: "".to_string(),
            intent: Some("foo".to_string()),
            ops: vec![],
        };
        let err = compile_keybinds(std::slice::from_ref(&kb)).expect_err("must reject");
        assert!(matches!(err, SceneError::Grammar { .. }));
    }

    #[test]
    fn keybinds_block_lives_at_root_not_inside_layout() {
        // Re-parse the rendered output and verify `keybinds` is a
        // top-level node (sibling of any future `layout`), not nested
        // inside a `layout { }` block.
        let rendered = rendered_keybinds(
            r#"scene "s" {
                keybind "Alt p" intent="picker.show"
            }"#,
        );
        let parsed = KdlDocument::parse(&rendered).expect("re-parse");
        let names: Vec<&str> = parsed
            .nodes()
            .iter()
            .map(|n| n.name().value())
            .collect();
        assert!(names.contains(&"keybinds"), "names: {names:?}");
        assert!(!names.contains(&"layout"), "expected no layout sibling here");
    }
}
