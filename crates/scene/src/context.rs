//! Scope builders — bind runtime context into `rhai::Scope` (T-025 / R8).
//!
//! Two builders mirror the two Rhai evaluation scopes (R8):
//!
//! - [`build_spawn_scope`] — layout-time context. Bindings: `cwd`,
//!   `id`, `name`, `env` (env as a `rhai::Map` of `String -> String`).
//!   Called once per spawn / reconciler pass.
//! - [`build_event_scope`] — event-time context. Bindings: `event`
//!   (flat-mapped `AgentEvent` variant), `payload` (extracted for
//!   `UserEvent`), `agent`, `session`, plus selector-captured locals.
//!   Called per reaction / bind op fire.
//!
//! # Placeholder snapshot types
//!
//! [`AgentSnapshot`] and [`SessionSnapshot`] are minimal local structs
//! for T-025. The full supervisor-owned snapshot types will land in
//! later tiers (T-056+); this module will be rewired once they are
//! public.
//!
//! # AgentEvent → rhai conversion
//!
//! Conversion walks `AgentEvent` through `serde_json::Value` and
//! converts nodes bottom-up to `rhai::Dynamic`. Round-tripping through
//! JSON is not the cleanest path — a direct facet SHAPE walk is on the
//! roadmap — but it keeps T-025 independent of facet-SHAPE work and is
//! sufficient for smoke-level scene predicates.

use std::collections::BTreeMap;

use ark_types::AgentEvent;
use rhai::{Dynamic, Scope};

/// Placeholder agent runtime snapshot.
///
/// Replace with the supervisor's own `AgentSnapshot` type when
/// T-056+ exposes one; the binding name (`agent`) and fields
/// (`phase`, `name`) are the stable surface scene predicates rely on.
#[derive(Debug, Clone, Default)]
pub struct AgentSnapshot {
    /// Agent lifecycle phase (e.g. `"planning"`, `"executing"`,
    /// `"review"`). Mirrors `ark_types::Phase` as a string for scene
    /// predicate use.
    pub phase: String,
    /// Agent display name.
    pub name: String,
}

/// Placeholder session runtime snapshot.
///
/// Replace with the supervisor's own `SessionSnapshot` type when
/// T-056+ exposes one; the binding name (`session`) and fields
/// (`id`, `name`) are the stable surface scene predicates rely on.
#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    /// Session identifier (ULID, UUID, etc. — transparent to scene).
    pub id: String,
    /// Human-readable session label.
    pub name: String,
}

/// Build a fresh [`rhai::Scope`] for the spawn-time scope (R8).
///
/// Bindings: `cwd`, `id`, `name` (strings) + `env` (`rhai::Map` of
/// `String -> String`). Rhai predicates / interpolation holes attached
/// to layout nodes see exactly these names.
pub fn build_spawn_scope(
    cwd: &str,
    id: &str,
    name: &str,
    env: &BTreeMap<String, String>,
) -> Scope<'static> {
    let mut scope = Scope::new();
    scope.push("cwd", cwd.to_string());
    scope.push("id", id.to_string());
    scope.push("name", name.to_string());
    let env_map: rhai::Map = env
        .iter()
        .map(|(k, v)| (k.clone().into(), Dynamic::from(v.clone())))
        .collect();
    scope.push("env", env_map);
    scope
}

/// Build a fresh [`rhai::Scope`] for the event-time scope (R8).
///
/// Bindings: `event` (variant flat-map), `payload` (extracted for
/// `UserEvent`; `()` otherwise), `agent` (map of snapshot fields),
/// `session` (map of snapshot fields), plus every key in `locals`
/// pushed as its own top-level binding (these are the selector-captured
/// locals from T-058).
pub fn build_event_scope(
    event: &AgentEvent,
    agent: &AgentSnapshot,
    session: &SessionSnapshot,
    locals: &BTreeMap<String, Dynamic>,
) -> Scope<'static> {
    let mut scope = Scope::new();

    // `event` — serialize the entire variant to JSON, then convert.
    // `serde_json` preserves the `#[serde(tag = "kind", rename_all = "snake_case")]`
    // shape so scene predicates can match on `event.kind`.
    let event_json = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    scope.push("event", json_to_dynamic(&event_json));

    // `payload` — extract for UserEvent; elsewhere `()`.
    if let AgentEvent::UserEvent { payload, .. } = event {
        scope.push("payload", json_to_dynamic(payload));
    } else {
        scope.push("payload", Dynamic::UNIT);
    }

    // `agent`, `session` — small flat maps.
    let mut agent_map = rhai::Map::new();
    agent_map.insert("phase".into(), Dynamic::from(agent.phase.clone()));
    agent_map.insert("name".into(), Dynamic::from(agent.name.clone()));
    scope.push("agent", agent_map);

    let mut session_map = rhai::Map::new();
    session_map.insert("id".into(), Dynamic::from(session.id.clone()));
    session_map.insert("name".into(), Dynamic::from(session.name.clone()));
    scope.push("session", session_map);

    // Selector-captured locals flow in as top-level bindings (T-058).
    for (k, v) in locals {
        scope.push(k.clone(), v.clone());
    }

    scope
}

/// Convert a `serde_json::Value` into a `rhai::Dynamic`.
///
/// - `Null` → `Dynamic::UNIT`
/// - `Bool` → `Dynamic::from(bool)`
/// - `Number` → `i64` if integral, else `f64`
/// - `String` → owned `String`
/// - `Array` → `rhai::Array` with elements converted recursively
/// - `Object` → `rhai::Map` with elements converted recursively
fn json_to_dynamic(v: &serde_json::Value) -> Dynamic {
    match v {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::UNIT
            }
        }
        serde_json::Value::String(s) => Dynamic::from(s.clone()),
        serde_json::Value::Array(a) => {
            let arr: rhai::Array = a.iter().map(json_to_dynamic).collect();
            Dynamic::from(arr)
        }
        serde_json::Value::Object(o) => {
            let map: rhai::Map = o
                .iter()
                .map(|(k, v)| (k.clone().into(), json_to_dynamic(v)))
                .collect();
            Dynamic::from(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::AgentId;

    #[test]
    fn spawn_scope_has_expected_keys() {
        let env: BTreeMap<String, String> = [("HOME".into(), "/home/me".into())]
            .into_iter()
            .collect();
        let scope = build_spawn_scope("/tmp", "abc", "demo", &env);
        // contains() checks whether the binding is in scope.
        assert!(scope.contains("cwd"));
        assert!(scope.contains("id"));
        assert!(scope.contains("name"));
        assert!(scope.contains("env"));
    }

    #[test]
    fn spawn_scope_eval_roundtrip() {
        // Feed the scope to a real Rhai engine and evaluate a
        // predicate that reads every binding.
        let engine = rhai::Engine::new();
        let env: BTreeMap<String, String> =
            [("HOME".into(), "/home/me".into())].into_iter().collect();
        let mut scope = build_spawn_scope("/projects/app", "abc", "demo", &env);
        let v: bool = engine
            .eval_expression_with_scope(
                &mut scope,
                r#"cwd == "/projects/app" && id == "abc" && name == "demo" && env["HOME"] == "/home/me""#,
            )
            .expect("spawn scope predicate should evaluate");
        assert!(v);
    }

    #[test]
    fn event_scope_user_event_exposes_payload() {
        let id = AgentId::new("test", "agent");
        let evt = AgentEvent::UserEvent {
            name: "myext.something".into(),
            source: "ext:myext".into(),
            payload: serde_json::json!({ "n": 42, "tag": "alpha" }),
        };
        let agent = AgentSnapshot {
            phase: "planning".into(),
            name: "builder".into(),
        };
        let session = SessionSnapshot {
            id: "s1".into(),
            name: "session-1".into(),
        };
        let _ = id;
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &agent, &session, &locals);

        let engine = rhai::Engine::new();
        let mut scope2 = scope.clone();
        let n: i64 = engine
            .eval_expression_with_scope(&mut scope2, r#"payload["n"]"#)
            .expect("payload n should evaluate");
        assert_eq!(n, 42);

        let mut scope3 = scope.clone();
        let tag: String = engine
            .eval_expression_with_scope(&mut scope3, r#"payload["tag"]"#)
            .expect("payload tag should evaluate");
        assert_eq!(tag, "alpha");

        let mut scope4 = scope.clone();
        let phase: String = engine
            .eval_expression_with_scope(&mut scope4, r#"agent["phase"]"#)
            .expect("agent phase should evaluate");
        assert_eq!(phase, "planning");

        let mut scope5 = scope;
        let sid: String = engine
            .eval_expression_with_scope(&mut scope5, r#"session["id"]"#)
            .expect("session id should evaluate");
        assert_eq!(sid, "s1");
    }

    #[test]
    fn event_scope_non_user_event_payload_is_unit() {
        let id = AgentId::new("test", "agent");
        let evt = AgentEvent::Error {
            id: id.clone(),
            message: "boom".into(),
        };
        let agent = AgentSnapshot::default();
        let session = SessionSnapshot::default();
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &agent, &session, &locals);
        let engine = rhai::Engine::new();
        let mut scope2 = scope;
        // Payload is unit (aka ()) — type check via `type_of`.
        let t: String = engine
            .eval_expression_with_scope(&mut scope2, r#"type_of(payload)"#)
            .expect("type_of(payload) should evaluate");
        assert_eq!(t, "()");
    }

    #[test]
    fn event_scope_captures_locals() {
        let id = AgentId::new("test", "agent");
        let evt = AgentEvent::Error {
            id,
            message: "boom".into(),
        };
        let agent = AgentSnapshot::default();
        let session = SessionSnapshot::default();
        let mut locals: BTreeMap<String, Dynamic> = BTreeMap::new();
        locals.insert("path".into(), Dynamic::from("src/README.md".to_string()));
        locals.insert("count".into(), Dynamic::from(7_i64));

        let scope = build_event_scope(&evt, &agent, &session, &locals);
        let engine = rhai::Engine::new();
        let mut scope2 = scope.clone();
        let p: String = engine
            .eval_expression_with_scope(&mut scope2, r#"path"#)
            .expect("local path should evaluate");
        assert_eq!(p, "src/README.md");

        let mut scope3 = scope;
        let c: i64 = engine
            .eval_expression_with_scope(&mut scope3, r#"count"#)
            .expect("local count should evaluate");
        assert_eq!(c, 7);
    }

    #[test]
    fn event_scope_started_variant() {
        // Smoke test: Started variant flows through the JSON pipeline.
        let spec = ark_types::AgentSpec::new(
            AgentId::new("test", "agent"),
            "demo",
            "cavekit",
            "claude-code",
            std::path::PathBuf::from("/tmp"),
            vec!["echo".into(), "hello".into()],
        );
        let evt = AgentEvent::Started { spec };
        let agent = AgentSnapshot::default();
        let session = SessionSnapshot::default();
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &agent, &session, &locals);
        // `event.kind` should be `"started"` per serde `rename_all = "snake_case"`.
        let engine = rhai::Engine::new();
        let mut scope2 = scope;
        let kind: String = engine
            .eval_expression_with_scope(&mut scope2, r#"event["kind"]"#)
            .expect("event kind should evaluate");
        assert_eq!(kind, "started");
    }

    #[test]
    fn json_to_dynamic_null_and_primitives() {
        assert!(json_to_dynamic(&serde_json::Value::Null).is_unit());
        assert!(json_to_dynamic(&serde_json::Value::Bool(true))
            .as_bool()
            .unwrap());
        assert_eq!(
            json_to_dynamic(&serde_json::json!(42)).as_int().unwrap(),
            42
        );
        assert_eq!(
            json_to_dynamic(&serde_json::json!("hi")).into_string().unwrap(),
            "hi".to_string()
        );
    }

    #[test]
    fn json_to_dynamic_array_map() {
        let v = serde_json::json!([1, 2, 3]);
        let d = json_to_dynamic(&v);
        assert!(d.is_array());

        let v = serde_json::json!({"a": 1, "b": "x"});
        let d = json_to_dynamic(&v);
        assert!(d.is_map());
    }
}
