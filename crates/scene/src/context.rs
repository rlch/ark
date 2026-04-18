//! Scope builders — bind runtime context into `rhai::Scope` (T-025 / R8).
//!
//! Two builders mirror the two Rhai evaluation scopes (R8):
//!
//! - [`build_spawn_scope`] — layout-time context. Bindings: `cwd`,
//!   `id`, `name`, `env` (env as a `rhai::Map` of `String -> String`).
//!   Called once per spawn / reconciler pass.
//! - [`build_event_scope`] — event-time context. Bindings: `event`
//!   (flat-mapped `CoreEvent` variant), `payload` (the flattened event
//!   payload), `session`, plus selector-captured locals.
//!   Called per reaction / bind op fire.
//!
//! # Snapshot types
//!
//! [`SessionSnapshot`] is the session-runtime view exposed to scene
//! predicates. Agent-era snapshot types are gone (Phase 1 kills the
//! agent concept); session state is the only runtime scope.
//!
//! # Event → rhai conversion
//!
//! Conversion walks the [`FlatEvent`] projection through
//! `serde_json::Value` and converts nodes bottom-up to `rhai::Dynamic`.
//! Round-tripping through JSON is not the cleanest path — a direct facet
//! SHAPE walk is on the roadmap — but it keeps scene independent of
//! facet-SHAPE work and is sufficient for smoke-level scene predicates.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ark_types::{CoreEvent, FlatEvent, SessionId};
use rhai::{Dynamic, Scope};

/// Session runtime snapshot exposed to event-scope predicates.
///
/// Replaces the agent-era `AgentSnapshot`. The binding name (`session`)
/// and fields (`id`, `name`, `cwd`, `started_at`, `extensions`) are the
/// stable surface scene predicates rely on.
///
/// See cavekit-soul-phase-1-types.md R9.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Session identifier — carries `name` + `ulid`.
    pub id: SessionId,
    /// Human-readable session label.
    pub name: String,
    /// Session working directory.
    pub cwd: PathBuf,
    /// When the session was created.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Per-extension state bucket. Each extension owns one entry keyed by
    /// its manifest name; the value is free-form JSON the extension
    /// maintains. Exposed to Rhai as `session.extensions` — a string-keyed
    /// map of dynamic values.
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            id: SessionId::new("default"),
            name: String::new(),
            cwd: PathBuf::new(),
            started_at: chrono::Utc::now(),
            extensions: BTreeMap::new(),
        }
    }
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
/// Bindings: `event` (flattened event — `name` + `payload`), `payload`
/// (the flat payload), `session` (map of snapshot fields), plus every
/// key in `locals` pushed as its own top-level binding (these are the
/// selector-captured locals from T-058).
pub fn build_event_scope(
    event: &CoreEvent,
    session: &SessionSnapshot,
    locals: &BTreeMap<String, Dynamic>,
) -> Scope<'static> {
    let mut scope = Scope::new();

    // Flatten the event into the stable (`name`, `payload`) projection
    // (see types R7 / FlatEvent).
    let flat = FlatEvent::from(event);

    // `event` — the full flattened object.
    let event_json = serde_json::to_value(&flat).unwrap_or(serde_json::Value::Null);
    scope.push("event", json_to_dynamic(&event_json));

    // `payload` — the flat payload, directly. For core variants this is
    // a JSON object of the variant fields; for extension events it's the
    // extension-owned payload verbatim.
    scope.push("payload", json_to_dynamic(&flat.payload));

    // `session` — flat map of snapshot fields.
    let mut session_map = rhai::Map::new();
    session_map.insert("id".into(), Dynamic::from(session.id.as_path_leaf()));
    session_map.insert("name".into(), Dynamic::from(session.name.clone()));
    session_map.insert(
        "cwd".into(),
        Dynamic::from(session.cwd.display().to_string()),
    );
    session_map.insert(
        "started_at".into(),
        Dynamic::from(session.started_at.to_rfc3339()),
    );
    // `session.extensions` — string-keyed map of per-ext JSON buckets.
    let ext_map: rhai::Map = session
        .extensions
        .iter()
        .map(|(k, v)| (k.clone().into(), json_to_dynamic(v)))
        .collect();
    session_map.insert("extensions".into(), Dynamic::from(ext_map));
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
    use ark_types::ExtEvent;

    #[test]
    fn spawn_scope_has_expected_keys() {
        let env: BTreeMap<String, String> =
            [("HOME".into(), "/home/me".into())].into_iter().collect();
        let scope = build_spawn_scope("/tmp", "abc", "demo", &env);
        assert!(scope.contains("cwd"));
        assert!(scope.contains("id"));
        assert!(scope.contains("name"));
        assert!(scope.contains("env"));
    }

    #[test]
    fn spawn_scope_eval_roundtrip() {
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
    fn event_scope_ext_event_exposes_payload() {
        let evt = CoreEvent::Ext(ExtEvent {
            ext: "myext".into(),
            kind: "something".into(),
            payload: serde_json::json!({ "n": 42, "tag": "alpha" }),
        });
        let session = SessionSnapshot::default();
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &session, &locals);

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

        let mut scope4 = scope;
        let name: String = engine
            .eval_expression_with_scope(&mut scope4, r#"event["name"]"#)
            .expect("event name should evaluate");
        assert_eq!(name, "myext.something");
    }

    #[test]
    fn event_scope_exposes_session_extensions() {
        let mut session = SessionSnapshot::default();
        session.name = "demo".into();
        session
            .extensions
            .insert("myext".into(), serde_json::json!({ "counter": 7 }));
        let evt = CoreEvent::Error {
            error: "boom".into(),
        };
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &session, &locals);
        let engine = rhai::Engine::new();
        let mut scope2 = scope;
        let counter: i64 = engine
            .eval_expression_with_scope(&mut scope2, r#"session["extensions"]["myext"]["counter"]"#)
            .expect("session.extensions.myext.counter should evaluate");
        assert_eq!(counter, 7);
    }

    #[test]
    fn event_scope_captures_locals() {
        let evt = CoreEvent::Error {
            error: "boom".into(),
        };
        let session = SessionSnapshot::default();
        let mut locals: BTreeMap<String, Dynamic> = BTreeMap::new();
        locals.insert("path".into(), Dynamic::from("src/README.md".to_string()));
        locals.insert("count".into(), Dynamic::from(7_i64));

        let scope = build_event_scope(&evt, &session, &locals);
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
    fn event_scope_core_session_started_event_name() {
        let spec = ark_types::SessionSpec {
            id: SessionId::new("demo"),
            name: "demo".to_string(),
            scene_path: None,
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            created_at: chrono::Utc::now(),
            ext_config: BTreeMap::new(),
        };
        let evt = CoreEvent::SessionStarted { spec };
        let session = SessionSnapshot::default();
        let locals = BTreeMap::new();
        let scope = build_event_scope(&evt, &session, &locals);
        let engine = rhai::Engine::new();
        let mut scope2 = scope;
        let name: String = engine
            .eval_expression_with_scope(&mut scope2, r#"event["name"]"#)
            .expect("event name should evaluate");
        assert_eq!(name, "ark.core.session_started");
    }

    #[test]
    fn json_to_dynamic_null_and_primitives() {
        assert!(json_to_dynamic(&serde_json::Value::Null).is_unit());
        assert!(
            json_to_dynamic(&serde_json::Value::Bool(true))
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            json_to_dynamic(&serde_json::json!(42)).as_int().unwrap(),
            42
        );
        assert_eq!(
            json_to_dynamic(&serde_json::json!("hi"))
                .into_string()
                .unwrap(),
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
