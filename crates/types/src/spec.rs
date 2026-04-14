use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::AgentId;

/// Immutable input to a spawn. Serialized once to `spec.json` at spawn time and
/// never mutated afterwards. See cavekit-types-state-events.md R2.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSpec {
    /// Deterministic, human-friendly id. See R1.
    pub id: AgentId,
    /// Human label, visible in picker.
    pub name: String,
    /// Orchestrator slug (e.g. `cavekit`).
    pub orchestrator: String,
    /// Engine slug (e.g. `claude-code`).
    pub engine: String,
    /// Worktree path the agent runs inside.
    pub cwd: PathBuf,
    /// Primary agent pane command.
    pub cmd: Vec<String>,
    /// Environment overrides. `BTreeMap` so iteration order is deterministic,
    /// which matters for stable `spec.json` serialisation.
    pub env: BTreeMap<String, String>,
    /// Optional zellij layout KDL stem. `None` means the orchestrator decides.
    pub layout: Option<String>,
    /// Zellij session name, derived from id but persisted for self-contained reads.
    pub session: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Orchestrator-specific free-form config, validated by the orchestrator.
    pub runner_config: serde_json::Value,
}

impl AgentSpec {
    /// Fill defaults: empty env, no layout, session derived from id, created_at = now,
    /// runner_config = Null. Callers can mutate the returned struct to override any
    /// default before it is frozen to disk.
    pub fn new(
        id: AgentId,
        name: impl Into<String>,
        orchestrator: impl Into<String>,
        engine: impl Into<String>,
        cwd: PathBuf,
        cmd: Vec<String>,
    ) -> Self {
        let session = id.session_name();
        Self {
            id,
            name: name.into(),
            orchestrator: orchestrator.into(),
            engine: engine.into(),
            cwd,
            cmd,
            env: BTreeMap::new(),
            layout: None,
            session,
            created_at: Utc::now(),
            runner_config: serde_json::Value::Null,
        }
    }
}

/// Orchestrator's view of a spec. For v1 it is identical to `AgentSpec`; the
/// alias keeps downstream code readable when it explicitly wants the
/// orchestrator-facing perspective. See R2.
pub type OrchestratorSpec = AgentSpec;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> AgentSpec {
        let id = AgentId::new("cavekit", "auth");
        let mut spec = AgentSpec::new(
            id,
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".to_string(), "--foo".to_string()],
        );
        spec.env.insert("B_VAR".to_string(), "b".to_string());
        spec.env.insert("A_VAR".to_string(), "a".to_string());
        spec.env.insert("C_VAR".to_string(), "c".to_string());
        spec.layout = Some("default".to_string());
        spec.runner_config = serde_json::json!({ "iterations": 3 });
        spec
    }

    #[test]
    fn serde_json_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serialize");
        let back: AgentSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    #[test]
    fn serde_pretty_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string_pretty(&spec).expect("serialize pretty");
        let back: AgentSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    #[test]
    fn env_iteration_is_deterministic() {
        let spec = sample_spec();
        let keys: Vec<&str> = spec.env.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["A_VAR", "B_VAR", "C_VAR"]);
    }

    #[test]
    fn env_serialization_is_sorted() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serialize");
        let a = json.find("A_VAR").expect("A_VAR present");
        let b = json.find("B_VAR").expect("B_VAR present");
        let c = json.find("C_VAR").expect("C_VAR present");
        assert!(a < b && b < c, "env keys must serialize in sorted order");
    }

    #[test]
    fn new_derives_session_from_id() {
        let id = AgentId::new("cavekit", "auth");
        let expected_session = id.session_name();
        let spec = AgentSpec::new(
            id,
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/w"),
            vec!["bin".into()],
        );
        assert_eq!(spec.session, expected_session);
    }

    #[test]
    fn new_defaults_are_null_and_empty() {
        let spec = AgentSpec::new(
            AgentId::new("cavekit", "x"),
            "x",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/w"),
            vec!["bin".into()],
        );
        assert!(spec.env.is_empty());
        assert!(spec.layout.is_none());
        assert_eq!(spec.runner_config, serde_json::Value::Null);
    }

    #[test]
    fn orchestrator_spec_is_agent_spec_alias() {
        // Compile-time alias check: if the types diverge this assignment fails.
        let spec = sample_spec();
        let alias: OrchestratorSpec = spec.clone();
        assert_eq!(alias, spec);
    }
}
