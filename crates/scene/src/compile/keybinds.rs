//! Keybind lowering — scene `bind "<chord>" { <ops> }` nodes → zellij
//! `keybinds { shared { bind … { MessagePlugin … } } }` KDL (T-065 / R5).
//!
//! # Translation matrix
//!
//! | Scene DSL                                | Zellij KDL                                                    |
//! |------------------------------------------|---------------------------------------------------------------|
//! | `bind "Ctrl s" { <ops> }`                | `bind "Ctrl s" { MessagePlugin "ark-bus" { name "ark-intent"; payload "<JSON>"; } }` |
//!
//! The rendered `bind` action is a single `MessagePlugin` call that
//! pipes a single-line JSON intent document into ark-bus (T-070's
//! `ark-intent` endpoint). Multi-op binds serialise their entire op
//! sequence into one payload — ark-bus forwards it to the supervisor
//! which dispatches each op in order against the `ark.core.*` intent
//! registry.
//!
//! # `clear-defaults=true` policy
//!
//! Not emitted. The keybinds block merges **additively** into zellij's
//! own keybinds so the user's pre-existing zellij binds survive.
//! Per-chord last-wins resolution is handled upstream by
//! [`crate::load_order::resolve_binds`] before this emitter runs — by
//! the time we get here, each chord appears at most once.
//!
//! # ark-bus dependency
//!
//! The emitted actions target the `ark-bus` plugin (R5 + T-073
//! auto-mount). Any scene that declares `bind` nodes triggers
//! ark-bus auto-injection via
//! [`crate::compile::auto_mount::inject_ark_bus_if_needed`]; T-065
//! assumes that injection ran (or will run) alongside keybind emission.
//!
//! # Payload shape
//!
//! Per ark-bus's `validate_intent_payload` (T-070): a single-line JSON
//! object carrying at least a `name` string. T-065 emits:
//!
//! ```json
//! {"name":"<canonical-op-name>","args":{<op-kdl-properties-as-strings>}}
//! ```
//!
//! Where `<canonical-op-name>` is the `ark.core.<verb>` identifier
//! registered in [`crate::ops::CORE_OP_NAMES`]. Multi-op binds pack
//! the full sequence under a top-level `"ops"` array.

#![allow(clippy::result_large_err)]

use kdl::{KdlDocument, KdlEntry, KdlEntryFormat, KdlNode, KdlValue};
use serde_json::{Map, Value};

use crate::ast::ops::OpNode;
use crate::ast::{BindNode, SceneBodyNode};
use crate::chord::parse_chord;
use crate::compile::auto_mount::ARK_BUS_PLUGIN_NAME;
use crate::error::SceneError;
use crate::parse::SceneIR;

/// The fixed `name=` on the emitted `MessagePlugin` action. ark-bus
/// routes pipe messages with this name to
/// [`ark_bus::dispatch_intent`](../../ark-bus/src/lib.rs) (T-070).
pub const ARK_INTENT_MESSAGE_NAME: &str = "ark-intent";

/// Collect every `BindNode` declared at scene root (R1.2 keeps `bind`
/// as a top-level-only node).
fn collect_binds(ir: &SceneIR) -> Vec<&BindNode> {
    ir.scene
        .body
        .iter()
        .filter_map(|n| match n {
            SceneBodyNode::Bind(b) => Some(b),
            _ => None,
        })
        .collect()
}

/// Whether `ir` has at least one `bind` node whose emission requires
/// a `keybinds { }` block in the rendered layout.
pub fn has_keybinds(ir: &SceneIR) -> bool {
    !collect_binds(ir).is_empty()
}

/// Compile all `bind "<chord>" { <ops> }` nodes in `ir` into a zellij
/// `keybinds { shared { … } }` block (T-065).
///
/// Returns `Ok(None)` when the scene has no `bind` nodes — callers
/// should skip injection in that case. Returns `Ok(Some(keybinds_node))`
/// otherwise; the node is suitable for pushing into a top-level
/// [`KdlDocument`] alongside `layout { }`.
///
/// Errors:
/// * [`SceneError::InvalidChord`] — raised from [`parse_chord`] if a
///   bind's chord string fails validation. Should be surfaced earlier
///   by the check pass, but guarded here for defence in depth.
pub fn compile_keybinds_node(ir: &SceneIR) -> Result<Option<KdlNode>, SceneError> {
    let binds = collect_binds(ir);
    if binds.is_empty() {
        return Ok(None);
    }

    // Emit `keybinds { shared { <bind …> } }`. No `clear-defaults=true` —
    // merges additively with user's own zellij config (R5).
    let mut keybinds_node = KdlNode::new("keybinds");
    let mut keybinds_body = KdlDocument::new();

    let mut shared_node = KdlNode::new("shared");
    let mut shared_body = KdlDocument::new();

    for bind in binds {
        let chord_node = build_bind_node(bind)?;
        shared_body.nodes_mut().push(chord_node);
    }

    shared_node.set_children(shared_body);
    keybinds_body.nodes_mut().push(shared_node);
    keybinds_node.set_children(keybinds_body);
    Ok(Some(keybinds_node))
}

/// Build a `bind "<chord>" { MessagePlugin "ark-bus" { name "ark-intent";
/// payload "<JSON>"; } }` node for a single scene-level `BindNode`.
fn build_bind_node(bind: &BindNode) -> Result<KdlNode, SceneError> {
    // Normalise the chord through the parser so zellij sees a canonical
    // `"Mod KEY"` form regardless of user input casing. Parse failures
    // ought to have been caught by validate/scope; guarded here for
    // defence in depth.
    let chord = parse_chord(&bind.chord)?;
    let chord_str = chord.as_zellij_string();

    let mut bind_node = KdlNode::new("bind");
    bind_node.push(forced_string_entry(&chord_str));

    let mut bind_body = KdlDocument::new();
    let mp_node = build_message_plugin_node(&bind.ops);
    bind_body.nodes_mut().push(mp_node);
    bind_node.set_children(bind_body);

    Ok(bind_node)
}

/// Build the inner `MessagePlugin "ark-bus" { name "ark-intent";
/// payload "<JSON>"; }` action node.
fn build_message_plugin_node(ops: &[OpNode]) -> KdlNode {
    let mut mp = KdlNode::new("MessagePlugin");
    mp.push(forced_string_entry(ARK_BUS_PLUGIN_NAME));

    let mut body = KdlDocument::new();

    let mut name_node = KdlNode::new("name");
    name_node.push(forced_string_entry(ARK_INTENT_MESSAGE_NAME));
    body.nodes_mut().push(name_node);

    let json = build_intent_json(ops);
    let mut payload_node = KdlNode::new("payload");
    payload_node.push(forced_string_entry(&json));
    body.nodes_mut().push(payload_node);

    mp.set_children(body);
    mp
}

/// Serialise an op sequence to a single-line JSON intent document.
///
/// Shape:
/// * Empty op list → `{"name":"ark.core.noop","ops":[]}` (noop is a
///   forward-compat placeholder; ark-bus accepts any JSON object with
///   a `name` string).
/// * Single op → `{"name":"<ark.core.verb>","args":{…}}` — args carry
///   every KDL property + positional argument the op declared in the
///   scene file as strings (typed coercion happens supervisor-side).
/// * Multi-op → `{"name":"ark.core.batch","ops":[{"name":…,"args":…},…]}`.
fn build_intent_json(ops: &[OpNode]) -> String {
    match ops {
        [] => {
            let mut obj = Map::new();
            obj.insert("name".to_string(), Value::String("ark.core.noop".into()));
            obj.insert("ops".to_string(), Value::Array(Vec::new()));
            Value::Object(obj).to_string()
        }
        [only] => op_to_json(only).to_string(),
        many => {
            let mut obj = Map::new();
            obj.insert("name".to_string(), Value::String("ark.core.batch".into()));
            let arr: Vec<Value> = many.iter().map(op_to_json).collect();
            obj.insert("ops".to_string(), Value::Array(arr));
            Value::Object(obj).to_string()
        }
    }
}

/// Serialise a single [`OpNode`] to a `{"name": …, "args": {…}}` JSON
/// value. The args object carries typed-to-string coerced values for
/// every declared property; op-verb names match
/// [`crate::ops::CORE_OP_NAMES`]. Unknown ops serialise with
/// `"name":"ark.core.unknown"` + a `"verb"` field carrying the raw
/// scene-file text so diagnostic tooling can surface the problem.
fn op_to_json(op: &OpNode) -> Value {
    let (name, args) = match op {
        OpNode::Focus(o) => ("ark.core.focus", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Close(o) => ("ark.core.close", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Rename(o) => ("ark.core.rename", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            m.insert("to".to_string(), Value::String(o.to.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Resize(o) => ("ark.core.resize", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            m.insert("direction".to_string(), Value::String(o.direction.clone()));
            m.insert("by".to_string(), Value::String(o.by.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Move(o) => ("ark.core.move", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            m.insert("to".to_string(), Value::String(o.to.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Pin(o) => ("ark.core.pin", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Unpin(o) => ("ark.core.unpin", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Spawn(o) => ("ark.core.spawn", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::NewTab(o) => ("ark.core.new_tab", {
            let mut m = Map::new();
            m.insert("handle".to_string(), Value::String(o.handle.clone()));
            insert_opt_string(&mut m, "name", &o.name);
            insert_opt_string(&mut m, "cwd", &o.cwd);
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::UseMode(o) => ("ark.core.use_mode", {
            let mut m = Map::new();
            m.insert("mode".to_string(), Value::String(o.mode.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Pipe(o) => ("ark.core.pipe", {
            let mut m = Map::new();
            m.insert("from".to_string(), Value::String(o.from.clone()));
            m.insert("to".to_string(), Value::String(o.to.clone()));
            m.insert("payload".to_string(), Value::String(o.payload.clone()));
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Emit(o) => ("ark.core.emit", {
            let mut m = Map::new();
            m.insert(
                "event_name".to_string(),
                Value::String(o.event_name.clone()),
            );
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::SetStatus(o) => ("ark.core.set_status", {
            let mut m = Map::new();
            m.insert("text".to_string(), Value::String(o.text.clone()));
            insert_opt_string(&mut m, "severity", &o.severity);
            if let Some(ttl) = o.ttl_ms {
                m.insert("ttl_ms".to_string(), Value::Number(ttl.into()));
            }
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Exec(o) => ("ark.core.exec", {
            let mut m = Map::new();
            m.insert("script".to_string(), Value::String(o.script.clone()));
            insert_opt_string(&mut m, "shell", &o.shell);
            if let Some(t) = o.timeout_ms {
                m.insert("timeout_ms".to_string(), Value::Number(t.into()));
            }
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::ReloadScene(o) => ("ark.core.reload_scene", {
            let mut m = Map::new();
            insert_when(&mut m, &o.when);
            m
        }),
        OpNode::Unknown { verb, .. } => ("ark.core.unknown", {
            let mut m = Map::new();
            m.insert("verb".to_string(), Value::String(verb.clone()));
            m
        }),
    };
    let mut obj = Map::new();
    obj.insert("name".to_string(), Value::String(name.to_string()));
    obj.insert("args".to_string(), Value::Object(args));
    Value::Object(obj)
}

fn insert_when(m: &mut Map<String, Value>, when: &Option<String>) {
    if let Some(w) = when {
        m.insert("when".to_string(), Value::String(w.clone()));
    }
}

fn insert_opt_string(m: &mut Map<String, Value>, key: &str, v: &Option<String>) {
    if let Some(s) = v {
        m.insert(key.to_string(), Value::String(s.clone()));
    }
}

/// Build a KDL entry with an explicitly-quoted string value, matching
/// the `str_prop` helper in `compile::layout`. Quoted-form survives
/// [`KdlDocument::autoformat`] via `autoformat_keep=true`.
fn forced_string_entry(value: &str) -> KdlEntry {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let mut entry = KdlEntry::new(KdlValue::String(value.to_string()));
    entry.set_format(KdlEntryFormat {
        value_repr: format!("\"{}\"", escaped),
        leading: " ".into(),
        autoformat_keep: true,
        ..Default::default()
    });
    entry
}

/// Prepend a compiled `keybinds { shared { … } }` block to `doc` if
/// `ir` declares any `bind` nodes and no pre-existing `keybinds` node
/// is present. Returns `true` when an injection happened, `false`
/// otherwise. Mirrors the contract of
/// [`crate::compile::auto_mount::inject_ark_bus_if_needed`].
pub fn inject_keybinds_if_needed(doc: &mut KdlDocument, ir: &SceneIR) -> Result<bool, SceneError> {
    if !has_keybinds(ir) {
        return Ok(false);
    }
    if doc.nodes().iter().any(|n| n.name().value() == "keybinds") {
        return Ok(false);
    }
    let Some(node) = compile_keybinds_node(ir)? else {
        return Ok(false);
    };
    // Insert before `layout { }` so zellij sees `keybinds { … } layout
    // { … }` in the conventional order used by user-authored zellij
    // configs.
    let insert_at = doc
        .nodes()
        .iter()
        .position(|n| n.name().value() == "layout")
        .unwrap_or(0);
    doc.nodes_mut().insert(insert_at, node);
    doc.autoformat();
    doc.ensure_v1();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn ir(src: &str) -> SceneIR {
        parse_scene(src, "test.kdl").expect("scene parses")
    }

    #[test]
    fn has_keybinds_true_for_bind_scene() {
        let src = r#"scene "s" { bind "Ctrl s" { focus "@main" } }"#;
        assert!(has_keybinds(&ir(src)));
    }

    #[test]
    fn has_keybinds_false_for_pure_layout_scene() {
        let src = r#"scene "s" { layout { tab "@m" { pane "@p" } } }"#;
        assert!(!has_keybinds(&ir(src)));
    }

    #[test]
    fn compile_keybinds_node_absent_when_no_binds() {
        let src = r#"scene "s" { layout { tab "@m" { pane "@p" } } }"#;
        let node = compile_keybinds_node(&ir(src)).expect("no binds ok");
        assert!(node.is_none());
    }

    #[test]
    fn compile_keybinds_node_single_bind_single_op() {
        let src = r#"scene "s" { bind "Ctrl s" { focus "@main" } }"#;
        let node = compile_keybinds_node(&ir(src))
            .expect("compiles")
            .expect("has node");
        let rendered = node.to_string();
        assert!(
            rendered.contains("keybinds"),
            "top-level keybinds: {rendered}"
        );
        assert!(rendered.contains("shared"), "shared block: {rendered}");
        assert!(
            rendered.contains("\"Ctrl s\""),
            "chord preserved: {rendered}"
        );
        assert!(
            rendered.contains("MessagePlugin"),
            "MessagePlugin action: {rendered}"
        );
        assert!(
            rendered.contains("\"ark-bus\""),
            "targets ark-bus: {rendered}"
        );
        assert!(
            rendered.contains("\"ark-intent\""),
            "message name ark-intent: {rendered}"
        );
        // Payload is a single-line JSON object.
        assert!(
            rendered.contains("ark.core.focus"),
            "op name in payload: {rendered}"
        );
        assert!(
            rendered.contains("@main"),
            "handle arg in payload: {rendered}"
        );
    }

    #[test]
    fn compile_keybinds_never_emits_clear_defaults() {
        // R5 + T-065 acceptance: additive merge with user binds — no
        // `clear-defaults=true` at the keybinds-node level.
        let src = r#"scene "s" { bind "Ctrl s" { focus "@m" } }"#;
        let node = compile_keybinds_node(&ir(src)).unwrap().unwrap();
        let rendered = node.to_string();
        assert!(
            !rendered.contains("clear-defaults"),
            "must NOT emit clear-defaults=true: {rendered}"
        );
    }

    #[test]
    fn compile_keybinds_node_multiple_binds() {
        let src = r#"
scene "s" {
    bind "Ctrl s" { focus "@main" }
    bind "Alt p" { close "@popup" }
}
"#;
        let node = compile_keybinds_node(&ir(src)).unwrap().unwrap();
        let rendered = node.to_string();
        assert!(rendered.contains("\"Ctrl s\""));
        assert!(rendered.contains("\"Alt p\""));
        assert!(rendered.contains("ark.core.focus"));
        assert!(rendered.contains("ark.core.close"));
    }

    #[test]
    fn compile_keybinds_node_multi_op_body_packs_batch() {
        let src = r#"
scene "s" {
    bind "Ctrl s" {
        focus "@main"
        close "@popup"
    }
}
"#;
        let node = compile_keybinds_node(&ir(src)).unwrap().unwrap();
        let rendered = node.to_string();
        assert!(
            rendered.contains("ark.core.batch"),
            "multi-op must batch: {rendered}"
        );
        assert!(
            rendered.contains("ark.core.focus"),
            "first op name: {rendered}"
        );
        assert!(
            rendered.contains("ark.core.close"),
            "second op name: {rendered}"
        );
    }

    #[test]
    fn compile_keybinds_json_payload_parses() {
        // Every emitted payload string MUST re-parse as JSON so
        // ark-bus's `validate_intent_payload` accepts it at runtime.
        // Walk the compiled KDL structurally to pull each payload
        // verbatim rather than grepping the rendered string (which is
        // subject to KDL v1/v2 escape quirks).
        let src = r#"
scene "s" {
    bind "Ctrl s" { focus "@main" }
    bind "Alt p" {
        close "@popup"
        set_status text="closed"
    }
}
"#;
        let node = compile_keybinds_node(&ir(src)).unwrap().unwrap();
        let mut found = 0usize;
        // keybinds { shared { bind "…" { MessagePlugin "ark-bus" { name "…"; payload "…" } } } }
        let Some(keybinds_body) = node.children() else {
            panic!("keybinds node has no children");
        };
        for shared in keybinds_body.nodes() {
            if shared.name().value() != "shared" {
                continue;
            }
            let Some(shared_body) = shared.children() else {
                continue;
            };
            for bind_n in shared_body.nodes() {
                if bind_n.name().value() != "bind" {
                    continue;
                }
                let Some(bind_body) = bind_n.children() else {
                    continue;
                };
                for mp in bind_body.nodes() {
                    if mp.name().value() != "MessagePlugin" {
                        continue;
                    }
                    let Some(mp_body) = mp.children() else {
                        continue;
                    };
                    for inner in mp_body.nodes() {
                        if inner.name().value() == "payload" {
                            let raw = inner
                                .entries()
                                .first()
                                .and_then(|e| e.value().as_string())
                                .expect("payload has string arg");
                            let v: serde_json::Value = serde_json::from_str(raw)
                                .unwrap_or_else(|e| panic!("payload not JSON: {e}; raw=`{raw}`"));
                            assert!(v.get("name").is_some(), "name required: {v}");
                            found += 1;
                        }
                    }
                }
            }
        }
        assert!(found >= 2, "expected at least 2 payloads, got {found}");
    }

    #[test]
    fn inject_keybinds_prepends_before_layout() {
        let src = r#"scene "s" { bind "Ctrl s" { focus "@m" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(KdlNode::new("layout"));

        let injected = inject_keybinds_if_needed(&mut doc, &ir).unwrap();
        assert!(injected);
        assert_eq!(doc.nodes()[0].name().value(), "keybinds");
        assert_eq!(doc.nodes()[1].name().value(), "layout");
    }

    #[test]
    fn inject_keybinds_noop_when_no_binds() {
        let src = r#"scene "s" { layout { tab "@m" { pane "@p" } } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(KdlNode::new("layout"));

        let injected = inject_keybinds_if_needed(&mut doc, &ir).unwrap();
        assert!(!injected);
        assert_eq!(doc.nodes().len(), 1);
    }

    #[test]
    fn inject_keybinds_noop_when_already_present() {
        let src = r#"scene "s" { bind "Ctrl s" { focus "@m" } }"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(KdlNode::new("keybinds"));
        doc.nodes_mut().push(KdlNode::new("layout"));

        let injected = inject_keybinds_if_needed(&mut doc, &ir).unwrap();
        assert!(!injected);
        // Still exactly one keybinds node (we respected the existing
        // one; no shadowing).
        let count = doc
            .nodes()
            .iter()
            .filter(|n| n.name().value() == "keybinds")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn chord_normalised_via_parse_chord() {
        // Lowercase `ctrl` should round-trip to canonical `Ctrl`.
        let src = r#"scene "s" { bind "ctrl s" { focus "@m" } }"#;
        let ir = ir(src);
        let node = compile_keybinds_node(&ir).unwrap().unwrap();
        let rendered = node.to_string();
        assert!(
            rendered.contains("\"Ctrl s\""),
            "chord should be canonicalised: {rendered}"
        );
    }

    #[test]
    fn snapshot_fixture_single_bind_focus() {
        // Snapshot fixture pinning the exact rendered KDL shape for a
        // single-bind single-op scene. Any structural drift in the
        // emitter — bind order, node ordering, JSON shape, quoting —
        // trips this assertion. Update intentionally if contract
        // changes; break glass if not.
        let src = r#"scene "s" { bind "Ctrl s" { focus "@main" } }"#;
        let node = compile_keybinds_node(&ir(src)).unwrap().unwrap();
        let mut doc = KdlDocument::new();
        doc.nodes_mut().push(node);
        doc.autoformat();
        doc.ensure_v1();
        let rendered = doc.to_string();

        // Structural invariants pinned as a string-contains contract
        // (vs a full-text equality snapshot — KDL v1/v2 whitespace
        // + identifier-quoting-rules drift between kdl-rs releases
        // without semantic impact). The pinned fragments are the
        // load-bearing signals ark-bus + zellij rely on.
        let expected_fragments: &[&str] = &[
            "keybinds",
            "shared",
            "bind \"Ctrl s\"",
            "MessagePlugin \"ark-bus\"",
            "name \"ark-intent\"",
            "payload ",
            "ark.core.focus",
            "@main",
        ];
        for frag in expected_fragments {
            assert!(
                rendered.contains(frag),
                "snapshot fragment `{frag}` missing from:\n{rendered}"
            );
        }
        // Anti-invariants — NEVER emit these.
        let forbidden: &[&str] = &[
            "clear-defaults", // additive merge (R5)
            "SwitchToMode",   // not in v1 action set
            "unbind",         // rebind is the ark-bus endpoint, not scene
        ];
        for frag in forbidden {
            assert!(
                !rendered.contains(frag),
                "forbidden fragment `{frag}` present in:\n{rendered}"
            );
        }
    }

    #[test]
    fn rendered_doc_reparses_after_injection() {
        let src = r#"
scene "s" {
    bind "Ctrl s" { focus "@main" }
    bind "Alt q" { close "@popup" }
}
"#;
        let ir = ir(src);
        let mut doc = KdlDocument::new();
        // Seed with a layout so the injection result mirrors the real
        // reconciler output shape.
        let mut layout_node = KdlNode::new("layout");
        layout_node.set_children(KdlDocument::new());
        doc.nodes_mut().push(layout_node);

        assert!(inject_keybinds_if_needed(&mut doc, &ir).unwrap());
        let text = doc.to_string();
        // Round-trip via KDL parser: `KdlDocument::parse` tries v2,
        // falls back to v1 — zellij's own parser is v1.
        KdlDocument::parse(&text).expect("keybinds+layout doc must re-parse");
    }
}
