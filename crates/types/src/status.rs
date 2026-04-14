//! Agent status snapshot rolled up from the event stream.
//!
//! See cavekit-types-state-events.md R6. Written atomically to `status.json`
//! (see cavekit-types-state-events.md R5 / `state_dir`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::event::{Severity, TabHandle};
use crate::spec::AgentSpec;

/// Lifecycle phase of an agent.
///
/// Phases are emitted as `PhaseTransition` events as well; see
/// cavekit-types-state-events.md R6.
///
/// # Terminal phases
///
/// `Done`, `Failed`, `Crashed`, `Killed`, and `Timeout` are terminal.
/// F-088: `Killed` and `Timeout` must NOT be conflated with `Done` —
/// forced/timeout termination is not a successful outcome, and picker /
/// `ark list` surfaces need the distinction.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Running,
    Idle,
    Prompting,
    Reviewing,
    Done,
    Failed,
    Crashed,
    /// Forced termination via `ark kill` / SIGTERM grace expiry.
    Killed,
    /// Orchestrator-driven timeout (e.g. stall detector escalated).
    Timeout,
}

/// Rollup of review findings by severity. See
/// cavekit-types-state-events.md R6.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Findings {
    pub p0: u32,
    pub p1: u32,
    pub p2: u32,
    pub p3: u32,
}

impl Findings {
    /// Total number of findings across all severities.
    pub fn total(&self) -> u32 {
        self.p0 + self.p1 + self.p2 + self.p3
    }

    /// Increment the counter for the given severity.
    pub fn record(&mut self, severity: Severity) {
        match severity {
            Severity::P0 => self.p0 += 1,
            Severity::P1 => self.p1 += 1,
            Severity::P2 => self.p2 += 1,
            Severity::P3 => self.p3 += 1,
        }
    }
}

/// Snapshot of an agent's state at the time of the most recent event.
///
/// Persisted to `status.json` after every event; see
/// cavekit-types-state-events.md R6.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentStatus {
    pub spec: AgentSpec,
    pub phase: Phase,
    pub progress: Option<(u32, u32)>,
    pub last_event_at: DateTime<Utc>,
    pub last_event_summary: String,
    pub tab_handles: Vec<TabHandle>,
    pub supervisor_pid: u32,
    pub stalled_since: Option<DateTime<Utc>>,
    pub findings: Findings,
    /// Picker-hide flag toggled by the `Forget` control-socket command
    /// (cavekit-hook-ipc.md R5). `#[serde(default)]` keeps pre-existing
    /// status files deserialising cleanly.
    #[serde(default)]
    pub hide: bool,
}

impl AgentStatus {
    /// Initial status for a freshly-spawned agent. `last_event_at` is set to
    /// "now" — the supervisor overwrites it as events arrive.
    pub fn new(spec: AgentSpec, supervisor_pid: u32) -> Self {
        Self {
            spec,
            phase: Phase::Starting,
            progress: None,
            last_event_at: Utc::now(),
            last_event_summary: String::new(),
            tab_handles: Vec::new(),
            supervisor_pid,
            stalled_since: None,
            findings: Findings::default(),
            hide: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::AgentId;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn sample_spec() -> AgentSpec {
        let id = AgentId::new("cavekit", "auth");
        let mut spec = AgentSpec::new(
            id,
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        spec.env = BTreeMap::new();
        spec
    }

    #[test]
    fn phase_serializes_snake_case() {
        let cases = [
            (Phase::Starting, "\"starting\""),
            (Phase::Running, "\"running\""),
            (Phase::Idle, "\"idle\""),
            (Phase::Prompting, "\"prompting\""),
            (Phase::Reviewing, "\"reviewing\""),
            (Phase::Done, "\"done\""),
            (Phase::Failed, "\"failed\""),
            (Phase::Crashed, "\"crashed\""),
            (Phase::Killed, "\"killed\""),
            (Phase::Timeout, "\"timeout\""),
        ];
        for (phase, expected) in cases {
            let json = serde_json::to_string(&phase).expect("ser");
            assert_eq!(json, expected);
            let back: Phase = serde_json::from_str(&json).expect("de");
            assert_eq!(back, phase);
        }
    }

    /// F-088 regression: Killed and Timeout are distinct from Done.
    #[test]
    fn killed_and_timeout_are_distinct_from_done() {
        assert_ne!(Phase::Killed, Phase::Done);
        assert_ne!(Phase::Timeout, Phase::Done);
        assert_ne!(Phase::Killed, Phase::Timeout);
        assert_ne!(Phase::Killed, Phase::Failed);
        assert_ne!(Phase::Timeout, Phase::Failed);
    }

    #[test]
    fn findings_default_and_total() {
        let f = Findings::default();
        assert_eq!(f.total(), 0);
        assert_eq!(f.p0, 0);
        assert_eq!(f.p1, 0);
        assert_eq!(f.p2, 0);
        assert_eq!(f.p3, 0);
    }

    #[test]
    fn findings_record_increments_right_counter() {
        let mut f = Findings::default();
        f.record(Severity::P0);
        f.record(Severity::P1);
        f.record(Severity::P1);
        f.record(Severity::P2);
        f.record(Severity::P3);
        f.record(Severity::P3);
        f.record(Severity::P3);
        assert_eq!(f.p0, 1);
        assert_eq!(f.p1, 2);
        assert_eq!(f.p2, 1);
        assert_eq!(f.p3, 3);
        assert_eq!(f.total(), 7);
    }

    #[test]
    fn agent_status_new_defaults() {
        let spec = sample_spec();
        let s = AgentStatus::new(spec.clone(), 4242);
        assert_eq!(s.phase, Phase::Starting);
        assert_eq!(s.last_event_summary, "");
        assert!(s.tab_handles.is_empty());
        assert_eq!(s.supervisor_pid, 4242);
        assert!(s.progress.is_none());
        assert!(s.stalled_since.is_none());
        assert_eq!(s.findings, Findings::default());
        assert_eq!(s.spec, spec);
        assert!(!s.hide, "hide defaults to false");
    }

    #[test]
    fn phase_is_copy_and_eq() {
        let p = Phase::Running;
        let q = p; // Copy
        assert_eq!(p, q);
        assert_ne!(p, Phase::Done);
    }

    #[test]
    fn findings_accumulate_across_all_severities() {
        let mut f = Findings::default();
        for sev in [Severity::P0, Severity::P1, Severity::P2, Severity::P3] {
            f.record(sev);
        }
        assert_eq!(f.total(), 4);
        assert_eq!(f.p0, 1);
        assert_eq!(f.p1, 1);
        assert_eq!(f.p2, 1);
        assert_eq!(f.p3, 1);
    }

    #[test]
    fn agent_status_serde_roundtrip_full() {
        let spec = sample_spec();
        let mut findings = Findings::default();
        findings.record(Severity::P0);
        findings.record(Severity::P2);
        let status = AgentStatus {
            spec,
            phase: Phase::Reviewing,
            progress: Some((3, 10)),
            last_event_at: Utc::now(),
            last_event_summary: "reviewing pr".into(),
            tab_handles: vec![
                TabHandle::new("ark-cavekit-auth", 1, "builder"),
                TabHandle::new("ark-cavekit-auth", 2, "reviewer"),
            ],
            supervisor_pid: 12345,
            stalled_since: Some(Utc::now()),
            findings,
            hide: false,
        };
        let json = serde_json::to_string(&status).expect("ser");
        let back: AgentStatus = serde_json::from_str(&json).expect("de");
        assert_eq!(back, status);
    }

    #[test]
    fn hide_field_is_optional_on_deserialize() {
        // Existing status.json files written before T-066 do not carry a
        // `hide` field; `#[serde(default)]` must keep them readable.
        let spec = sample_spec();
        let legacy = serde_json::json!({
            "spec": spec,
            "phase": "starting",
            "progress": null,
            "last_event_at": Utc::now(),
            "last_event_summary": "",
            "tab_handles": [],
            "supervisor_pid": 42,
            "stalled_since": null,
            "findings": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }
        });
        let status: AgentStatus = serde_json::from_value(legacy).expect("legacy deserialize");
        assert!(!status.hide);
    }

    #[test]
    fn hide_true_roundtrips() {
        let spec = sample_spec();
        let status = AgentStatus {
            spec,
            phase: Phase::Idle,
            progress: None,
            last_event_at: Utc::now(),
            last_event_summary: String::new(),
            tab_handles: vec![],
            supervisor_pid: 1,
            stalled_since: None,
            findings: Findings::default(),
            hide: true,
        };
        let json = serde_json::to_string(&status).expect("ser");
        assert!(json.contains("\"hide\":true"));
        let back: AgentStatus = serde_json::from_str(&json).expect("de");
        assert!(back.hide);
    }
}
