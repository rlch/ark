//! Session status snapshot rolled up from the event stream.
//!
//! See cavekit-soul-phase-1-types.md R4. Written atomically to `status.json`.
//! Phase 1 keeps `AgentStatus` as a thin placeholder; the Soul Phase 1
//! `SessionStatus` rework + deletion of agent-methodology fields lands in
//! a later tier. For now this module no longer defines `Phase`, `Outcome`,
//! `Findings`, or `Severity` — those are deleted outright from core
//! (cavekit-soul-phase-1-types.md R5).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::event::TabHandle;
use crate::spec::SessionSpec;

/// Snapshot of a session's state at the time of the most recent event.
///
/// Persisted to `status.json` after every event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStatus {
    pub spec: SessionSpec,
    pub last_event_at: DateTime<Utc>,
    pub last_event_summary: String,
    pub tab_handles: Vec<TabHandle>,
    pub supervisor_pid: u32,
    pub stalled_since: Option<DateTime<Utc>>,
    /// Picker-hide flag toggled by the `Forget` control-socket command.
    #[serde(default)]
    pub hide: bool,
}

impl AgentStatus {
    /// Initial status for a freshly-spawned session. `last_event_at` is set to
    /// "now" — the supervisor overwrites it as events arrive.
    pub fn new(spec: SessionSpec, supervisor_pid: u32) -> Self {
        Self {
            spec,
            last_event_at: Utc::now(),
            last_event_summary: String::new(),
            tab_handles: Vec::new(),
            supervisor_pid,
            stalled_since: None,
            hide: false,
        }
    }
}
