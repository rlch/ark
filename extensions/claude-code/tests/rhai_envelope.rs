//! T-016: Rhai reaction envelope test.
//!
//! Simulates a synthetic `cc-hook` POST carrying a `SubagentStop`
//! payload, runs it through the R3 translator, and confirms that every
//! field an `on "claude-code.subagent.stop" { ... }` reaction needs is
//! reachable from a Rhai expression over `event.payload.*`.
//!
//! # Why raw `rhai::Engine` + not the scene runtime
//!
//! Wiring up a full `ark-scene` compile + dispatch loop in a unit test
//! pulls in KDL parsing, reaction binding, and the scene reconciler —
//! well beyond what this task needs to assert. The scene engine already
//! owns a scope-policed `rhai::Engine` wrapper (see
//! `crates/scene/src/rhai.rs`), but that wrapper disables the `=`
//! symbol for safety so it can't easily seed `let event = #{ ... }`
//! scope bindings from test code.
//!
//! The invariant T-016 exercises is payload-shape-only: given the JSON
//! the translator emits, can a Rhai expression like
//! `event.payload.agent_id` read the field? That question is answered
//! purely by Rhai's object-map semantics, which behave identically on a
//! vanilla `rhai::Engine`. If the scene engine later diverges (e.g.
//! custom object index semantics), this test fails obviously and points
//! at the gap.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_ext_claude_code::{
    HookEvent, HookPayload, NdjsonLine, flat_event_name, payload_to_ext_event,
};
use rhai::{Dynamic, Engine, Map, Scope};

/// Build the SubagentStop NDJSON envelope the R3 table says cc-hook
/// POSTs on subagent termination, populated with the four fields the
/// scene view will read back.
fn synth_subagent_stop_envelope() -> NdjsonLine {
    let mut extra = BTreeMap::new();
    extra.insert("agent_id".into(), serde_json::json!("agent-42"));
    extra.insert("agent_type".into(), serde_json::json!("code-writer"));
    extra.insert(
        "last_assistant_message".into(),
        serde_json::json!("wrote 3 files"),
    );
    extra.insert(
        "agent_transcript_path".into(),
        serde_json::json!("/tmp/agent-42.jsonl"),
    );

    let payload = HookPayload {
        session_id: "ark-sess".into(),
        cwd: PathBuf::from("/tmp"),
        hook_event_name: "SubagentStop".into(),
        tool_name: None,
        tool_input: None,
        extra,
    };

    NdjsonLine {
        kind: "SubagentStop".into(),
        session_id: "ark-sess".into(),
        payload,
        emitted_at: "2026-04-18T00:00:00Z".into(),
        bridge_version: None,
    }
}

/// Seed Rhai scope with `event = #{ name: "<ext>.<kind>", payload: #{
/// ... } }` so expressions in scene reactions see exactly what the
/// scene runtime will feed them once R3 dispatch is wired up.
///
/// Conversion path: serde_json::Value → JSON string → `Engine::parse_json`.
/// This is what the scene runtime will do too once it wires ExtEvents
/// into the reaction dispatcher — sharing the path keeps test + prod
/// in lockstep.
fn seed_event_into_scope(
    engine: &Engine,
    scope: &mut Scope,
    name: &str,
    payload: &serde_json::Value,
) {
    let payload_json = serde_json::to_string(payload).expect("ser payload");
    let payload_dyn: Dynamic = engine
        .parse_json(&payload_json, /*has_null=*/ true)
        .expect("parse_json payload")
        .into();

    let mut event_map = Map::new();
    event_map.insert("name".into(), name.into());
    event_map.insert("payload".into(), payload_dyn);
    scope.push("event", event_map);
}

fn eval_string(engine: &Engine, scope: &mut Scope, expr: &str) -> String {
    engine
        .eval_expression_with_scope::<String>(scope, expr)
        .unwrap_or_else(|e| panic!("eval `{expr}` failed: {e}"))
}

// ---------- Tests ----------

#[test]
fn subagent_stop_envelope_payload_fields_readable_from_rhai() {
    // Construct + translate.
    let envelope = synth_subagent_stop_envelope();
    let ext_event = payload_to_ext_event(&envelope.payload, HookEvent::SubagentStop);
    assert_eq!(ext_event.kind, "subagent.stop");
    assert_eq!(
        flat_event_name(HookEvent::SubagentStop),
        "claude-code.subagent.stop"
    );

    // Seed Rhai scope the way the scene runtime will.
    let engine = Engine::new();
    let mut scope = Scope::new();
    seed_event_into_scope(
        &engine,
        &mut scope,
        "claude-code.subagent.stop",
        &ext_event.payload,
    );

    // Every field the view + reactions need, accessed the same way an
    // `on "claude-code.subagent.stop" { ... }` Rhai block would.
    assert_eq!(
        eval_string(&engine, &mut scope, "event.payload.agent_id"),
        "agent-42"
    );
    assert_eq!(
        eval_string(&engine, &mut scope, "event.payload.agent_type"),
        "code-writer"
    );
    assert_eq!(
        eval_string(&engine, &mut scope, "event.payload.last_assistant_message"),
        "wrote 3 files"
    );
    assert_eq!(
        eval_string(&engine, &mut scope, "event.payload.agent_transcript_path"),
        "/tmp/agent-42.jsonl"
    );

    // And `event.name` matches the `<ext>.<kind>` R3 form Rhai
    // reactions bind against.
    assert_eq!(
        eval_string(&engine, &mut scope, "event.name"),
        "claude-code.subagent.stop"
    );
}

#[test]
fn event_payload_supports_method_chains_over_strings() {
    // Document (via test) that standard Rhai string methods work on
    // payload fields — scene authors relying on `event.payload.foo.contains("x")`
    // need to know that's safe. If a future change to the dispatcher
    // strips the standard package, this test fails early.
    let envelope = synth_subagent_stop_envelope();
    let ext_event = payload_to_ext_event(&envelope.payload, HookEvent::SubagentStop);

    let engine = Engine::new();
    let mut scope = Scope::new();
    seed_event_into_scope(
        &engine,
        &mut scope,
        "claude-code.subagent.stop",
        &ext_event.payload,
    );

    let b = engine
        .eval_expression_with_scope::<bool>(
            &mut scope,
            r#"event.payload.last_assistant_message.contains("3 files")"#,
        )
        .expect("eval");
    assert!(b);
}
