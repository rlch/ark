use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::SessionId;

/// Immutable input to a session spawn. Serialized once to `spec.json` at
/// spawn time and never mutated afterwards.
///
/// See cavekit-soul-phase-1-types.md R1.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSpec {
    /// Unique session id (name + ulid).
    pub id: SessionId,
    /// Human label, visible in picker / `ark list`.
    pub name: String,
    /// Optional path to the scene KDL this session was launched against.
    pub scene_path: Option<PathBuf>,
    /// Working directory the session runs inside.
    pub cwd: PathBuf,
    /// Environment overrides. `BTreeMap` so iteration order is deterministic,
    /// which matters for stable `spec.json` serialisation.
    pub env: BTreeMap<String, String>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Per-extension free-form config. Each extension writes into its own
    /// bucket under its manifest name. Core never reads these.
    pub ext_config: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> SessionSpec {
        let mut env = BTreeMap::new();
        env.insert("B_VAR".to_string(), "b".to_string());
        env.insert("A_VAR".to_string(), "a".to_string());
        let mut ext_config = BTreeMap::new();
        ext_config.insert(
            "claude-code".to_string(),
            serde_json::json!({ "permission_policy": "default" }),
        );
        SessionSpec {
            id: SessionId::new("foo"),
            name: "foo".to_string(),
            scene_path: Some(PathBuf::from("/tmp/scenes/demo.kdl")),
            cwd: PathBuf::from("/tmp/worktree"),
            env,
            created_at: Utc::now(),
            ext_config,
        }
    }

    #[test]
    fn spec_serde_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serialize");
        let back: SessionSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    #[test]
    fn env_is_btreemap_deterministic() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).expect("serialize");
        let a = json.find("A_VAR").expect("A_VAR present");
        let b = json.find("B_VAR").expect("B_VAR present");
        assert!(a < b, "env keys must serialize in sorted order: {json}");
    }
}
