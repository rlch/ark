//! Session status snapshot.
//!
//! See cavekit-soul-phase-1-types.md R4. Written atomically to `status.json`.
//! Per-extension status rollup data lives in `ext_state` under the extension
//! name. Core writes nothing into those buckets — extensions own their
//! entries. `Phase`, `Outcome`, `Findings`, and `Severity` are gone entirely
//! (cavekit-soul-phase-1-types.md R5); methodology concepts re-home inside
//! extensions in Phase 4.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::SessionId;

/// Snapshot of a session's state. Persisted to `status.json`.
///
/// Fields are deliberately minimal: core tracks only lifecycle timestamps and
/// the session id. Everything methodology-flavoured (phase, findings, tab
/// handles, supervisor pid) lives in `ext_state` under the owning extension's
/// manifest name.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionStatus {
    /// Session identity.
    pub id: SessionId,
    /// Wall-clock time the session was launched.
    pub started_at: DateTime<Utc>,
    /// Wall-clock time the session terminated, if it has.
    pub terminated_at: Option<DateTime<Utc>>,
    /// Per-extension status rollup. `BTreeMap` for deterministic order on
    /// disk. Core never writes into this map; each extension owns its entry
    /// under its manifest name.
    pub ext_state: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_serde_roundtrip() {
        let mut ext_state = BTreeMap::new();
        ext_state.insert(
            "claude-code".to_string(),
            serde_json::json!({ "phase": "running" }),
        );
        ext_state.insert(
            "acp-client".to_string(),
            serde_json::json!({ "connected": true }),
        );
        let status = SessionStatus {
            id: SessionId::new("foo"),
            started_at: Utc::now(),
            terminated_at: None,
            ext_state,
        };
        let json = serde_json::to_string(&status).expect("ser");
        let back: SessionStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back.id, status.id);
        assert_eq!(back.started_at, status.started_at);
        assert_eq!(back.terminated_at, status.terminated_at);
        assert_eq!(back.ext_state, status.ext_state);
    }
}
