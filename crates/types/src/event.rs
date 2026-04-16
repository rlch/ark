use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::AgentId;
use crate::spec::AgentSpec;

/// Handle to a zellij tab hosting an agent pane.
///
/// Refined by T-007: adds a constructor and `Display` impl. Serialization shape
/// is stable (session/tab_index/name) so persisted events from earlier builds
/// stay compatible. See cavekit-types-state-events.md R3 and
/// cavekit-architecture.md R3–R4.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TabHandle {
    pub session: String,
    pub tab_index: u32,
    pub name: String,
}

impl TabHandle {
    /// Construct a new `TabHandle`.
    pub fn new(session: impl Into<String>, tab_index: u32, name: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            tab_index,
            name: name.into(),
        }
    }
}

impl std::fmt::Display for TabHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.session, self.tab_index)
    }
}

/// Every observable event during an agent run. Serde-serializable for
/// `events.jsonl` and zellij pipe payloads. See cavekit-types-state-events.md R3.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    Started {
        spec: AgentSpec,
    },
    TabOpened {
        id: AgentId,
        parent: Option<AgentId>,
        role: TabRole,
        tab_handle: TabHandle,
        label: String,
    },
    TabClosed {
        id: AgentId,
        tab_handle: TabHandle,
    },
    Progress {
        id: AgentId,
        done: u32,
        total: u32,
        label: Option<String>,
    },
    TaskDone {
        id: AgentId,
        task_id: String,
        label: Option<String>,
    },
    Iteration {
        id: AgentId,
        n: u32,
        max: Option<u32>,
    },
    PhaseTransition {
        id: AgentId,
        from: Option<String>,
        to: String,
    },
    ToolUse {
        id: AgentId,
        tool: String,
        input_summary: String,
    },
    Message {
        id: AgentId,
        role: MessageRole,
        summary: String,
    },
    FileEdited {
        id: AgentId,
        path: PathBuf,
        additions: u32,
        deletions: u32,
    },
    ReviewComment {
        id: AgentId,
        reviewer: AgentId,
        severity: Severity,
        path: PathBuf,
        line: Option<u32>,
        body: String,
    },
    PermissionAsked {
        id: AgentId,
        tool: String,
        summary: String,
    },
    PermissionResolved {
        id: AgentId,
        tool: String,
        decision: PermissionDecision,
    },
    Stall {
        id: AgentId,
        since: DateTime<Utc>,
    },
    Log {
        id: AgentId,
        level: LogLevel,
        line: String,
    },
    Error {
        id: AgentId,
        message: String,
    },
    Done {
        id: AgentId,
        outcome: Outcome,
    },
    /// Namespaced, free-form event emitted by scenes, extensions, hooks, the ACP
    /// agent, or ark-core. Carries an arbitrary JSON `payload` and a `source` tag
    /// so `ark scene explain` can attribute reactions to their origin.
    ///
    /// ## Reserved payload top-level keys
    ///
    /// The keys `name`, `source`, and `payload` are reserved at the top of the
    /// variant itself. Scene selectors with the UserEvent hybrid access route
    /// bare field names into `payload`; reserved keys bypass that routing and
    /// bind to the top-level variant fields directly. See cavekit-scene.md R4.
    ///
    /// ## Canonical `source` values
    ///
    /// - `"scene"` — emitted by a scene reaction (`emit` op).
    /// - `"ext:<name>"` — emitted by an ark-native extension with that manifest name.
    /// - `"hook:<name>"` — emitted by a legacy TOML `[[hooks]]` entry (compat).
    /// - `"core"` — emitted by ark-core (supervisor, scene runtime, etc.).
    /// - `"acp"` — emitted by the ACP agent (permission requests, session updates).
    ///
    /// Must be non-empty; the scene compiler rejects empty strings. See
    /// cavekit-scene.md R4 and R10.
    #[serde(rename = "user_event")]
    UserEvent {
        /// Dotted, namespaced event name (e.g. `ark.acp.permission_requested`,
        /// `ark.scene.reloaded`, `myext.my_event`). Matched verbatim by scene
        /// selectors of the form `"UserEvent:<name>"`.
        name: String,
        /// Origin attribution, used by `ark scene explain` to trace which
        /// reaction graph produced this event. See the variant-level doc
        /// comment for canonical values.
        source: String,
        /// Arbitrary JSON payload; bound to the `payload` map in scene Rhai
        /// predicates. Shape is owned by the emitter — consumers should treat
        /// unknown fields as tolerated.
        payload: serde_json::Value,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TabRole {
    Builder,
    Subagent,
    Reviewer,
    Log,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success { artifacts: Vec<PathBuf> },
    Failed { reason: String },
    Killed,
    Timeout,
    Crashed { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allowed,
    Denied,
    Deferred,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    fn sample_spec() -> AgentSpec {
        let id = sample_id();
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

    fn sample_tab_handle() -> TabHandle {
        TabHandle {
            session: "ark-cavekit-auth".into(),
            tab_index: 1,
            name: "builder".into(),
        }
    }

    fn roundtrip(event: &AgentEvent) {
        let json = serde_json::to_string(event).expect("ser");
        let back: AgentEvent = serde_json::from_str(&json).expect("de");
        assert_eq!(&back, event);
    }

    #[test]
    fn started_roundtrip_and_tag() {
        let ev = AgentEvent::Started {
            spec: sample_spec(),
        };
        let json = serde_json::to_string(&ev).expect("ser");
        assert!(
            json.contains("\"kind\":\"started\""),
            "tag/rename check: {json}"
        );
        roundtrip(&ev);
    }

    #[test]
    fn tab_opened_roundtrip() {
        let ev = AgentEvent::TabOpened {
            id: sample_id(),
            parent: Some(sample_id()),
            role: TabRole::Builder,
            tab_handle: sample_tab_handle(),
            label: "main".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn tab_closed_roundtrip() {
        let ev = AgentEvent::TabClosed {
            id: sample_id(),
            tab_handle: sample_tab_handle(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn progress_roundtrip() {
        let ev = AgentEvent::Progress {
            id: sample_id(),
            done: 3,
            total: 10,
            label: Some("step".into()),
        };
        roundtrip(&ev);
    }

    #[test]
    fn task_done_roundtrip() {
        let ev = AgentEvent::TaskDone {
            id: sample_id(),
            task_id: "T-005".into(),
            label: None,
        };
        roundtrip(&ev);
    }

    #[test]
    fn iteration_roundtrip() {
        let ev = AgentEvent::Iteration {
            id: sample_id(),
            n: 2,
            max: Some(5),
        };
        roundtrip(&ev);
    }

    #[test]
    fn phase_transition_roundtrip() {
        let ev = AgentEvent::PhaseTransition {
            id: sample_id(),
            from: Some("starting".into()),
            to: "running".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn tool_use_roundtrip() {
        let ev = AgentEvent::ToolUse {
            id: sample_id(),
            tool: "Read".into(),
            input_summary: "foo.rs".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn message_roundtrip() {
        let ev = AgentEvent::Message {
            id: sample_id(),
            role: MessageRole::Assistant,
            summary: "hi".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn file_edited_roundtrip() {
        let ev = AgentEvent::FileEdited {
            id: sample_id(),
            path: PathBuf::from("src/lib.rs"),
            additions: 10,
            deletions: 2,
        };
        roundtrip(&ev);
    }

    #[test]
    fn review_comment_roundtrip() {
        let ev = AgentEvent::ReviewComment {
            id: sample_id(),
            reviewer: AgentId::new("cavekit", "reviewer"),
            severity: Severity::P1,
            path: PathBuf::from("src/lib.rs"),
            line: Some(42),
            body: "fix this".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn permission_asked_roundtrip() {
        let ev = AgentEvent::PermissionAsked {
            id: sample_id(),
            tool: "Bash".into(),
            summary: "rm -rf /".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn permission_resolved_roundtrip() {
        let ev = AgentEvent::PermissionResolved {
            id: sample_id(),
            tool: "Bash".into(),
            decision: PermissionDecision::Denied,
        };
        roundtrip(&ev);
    }

    #[test]
    fn stall_roundtrip() {
        let ev = AgentEvent::Stall {
            id: sample_id(),
            since: Utc::now(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn log_roundtrip() {
        let ev = AgentEvent::Log {
            id: sample_id(),
            level: LogLevel::Info,
            line: "hello".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn error_roundtrip() {
        let ev = AgentEvent::Error {
            id: sample_id(),
            message: "boom".into(),
        };
        roundtrip(&ev);
    }

    #[test]
    fn done_roundtrip() {
        let ev = AgentEvent::Done {
            id: sample_id(),
            outcome: Outcome::Success {
                artifacts: vec![PathBuf::from("out.txt")],
            },
        };
        roundtrip(&ev);
    }

    #[test]
    fn user_event_roundtrip_and_tag() {
        let ev = AgentEvent::UserEvent {
            name: "ark.acp.permission_requested".into(),
            source: "acp".into(),
            payload: serde_json::json!({
                "request_id": "req-1",
                "tool": "Bash",
                "params": {"command": "ls"},
            }),
        };
        let json = serde_json::to_string(&ev).expect("ser");
        assert!(
            json.contains("\"kind\":\"user_event\""),
            "serde rename check — must tag as snake_case `user_event`: {json}"
        );
        assert!(
            json.contains("\"source\":\"acp\""),
            "source field must round-trip verbatim: {json}"
        );
        roundtrip(&ev);
    }

    #[test]
    fn user_event_source_attribution_values() {
        // Exercise each canonical `source` value documented on UserEvent (see
        // cavekit-scene.md R4 + build-site-scene.md T-008).
        for source in ["scene", "ext:my-extension", "hook:on-stop", "core", "acp"] {
            let ev = AgentEvent::UserEvent {
                name: "myns.some_event".into(),
                source: source.into(),
                payload: serde_json::Value::Null,
            };
            roundtrip(&ev);
        }
    }

    // --- sub-enum roundtrips ---

    #[test]
    fn tab_role_roundtrip() {
        for role in [
            TabRole::Builder,
            TabRole::Subagent,
            TabRole::Reviewer,
            TabRole::Log,
            TabRole::Custom("frobnicator".into()),
        ] {
            let json = serde_json::to_string(&role).expect("ser");
            let back: TabRole = serde_json::from_str(&json).expect("de");
            assert_eq!(back, role);
        }
    }

    #[test]
    fn outcome_roundtrip() {
        for outcome in [
            Outcome::Success {
                artifacts: vec![PathBuf::from("a"), PathBuf::from("b")],
            },
            Outcome::Failed {
                reason: "oops".into(),
            },
            Outcome::Killed,
            Outcome::Timeout,
            Outcome::Crashed {
                reason: "sigsegv".into(),
            },
        ] {
            let json = serde_json::to_string(&outcome).expect("ser");
            let back: Outcome = serde_json::from_str(&json).expect("de");
            assert_eq!(back, outcome);
        }
    }

    #[test]
    fn severity_roundtrip_and_ord() {
        for sev in [Severity::P0, Severity::P1, Severity::P2, Severity::P3] {
            let json = serde_json::to_string(&sev).expect("ser");
            let back: Severity = serde_json::from_str(&json).expect("de");
            assert_eq!(back, sev);
        }
        // Ord check — P0 < P1 < P2 < P3 in declaration order
        let mut v = vec![Severity::P3, Severity::P0, Severity::P2, Severity::P1];
        v.sort();
        assert_eq!(
            v,
            vec![Severity::P0, Severity::P1, Severity::P2, Severity::P3]
        );
    }

    #[test]
    fn message_role_roundtrip() {
        for role in [
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::System,
            MessageRole::Tool,
        ] {
            let json = serde_json::to_string(&role).expect("ser");
            let back: MessageRole = serde_json::from_str(&json).expect("de");
            assert_eq!(back, role);
        }
    }

    #[test]
    fn permission_decision_roundtrip() {
        for d in [
            PermissionDecision::Allowed,
            PermissionDecision::Denied,
            PermissionDecision::Deferred,
        ] {
            let json = serde_json::to_string(&d).expect("ser");
            let back: PermissionDecision = serde_json::from_str(&json).expect("de");
            assert_eq!(back, d);
        }
    }

    #[test]
    fn log_level_roundtrip() {
        for l in [
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            let json = serde_json::to_string(&l).expect("ser");
            let back: LogLevel = serde_json::from_str(&json).expect("de");
            assert_eq!(back, l);
        }
    }

    #[test]
    fn tab_handle_new_and_display() {
        let h = TabHandle::new("ark-cavekit-auth", 3, "builder");
        assert_eq!(h.session, "ark-cavekit-auth");
        assert_eq!(h.tab_index, 3);
        assert_eq!(h.name, "builder");
        assert_eq!(format!("{h}"), "ark-cavekit-auth:3");
    }

    #[test]
    fn tab_handle_hash_via_btreeset() {
        use std::collections::BTreeSet;
        let a = TabHandle::new("s", 1, "a");
        let b = TabHandle::new("s", 2, "b");
        let mut set: BTreeSet<String> = BTreeSet::new();
        set.insert(a.to_string());
        set.insert(b.to_string());
        set.insert(a.to_string());
        assert_eq!(set.len(), 2);

        // Also ensure the Hash derive is usable via HashSet
        use std::collections::HashSet;
        let mut hs: HashSet<TabHandle> = HashSet::new();
        hs.insert(a.clone());
        hs.insert(b.clone());
        hs.insert(a.clone());
        assert_eq!(hs.len(), 2);
    }

    /// Instantiate every variant once — proves field shapes compile.
    #[test]
    fn all_variants_compile() {
        let id = sample_id();
        let _events: Vec<AgentEvent> = vec![
            AgentEvent::Started {
                spec: sample_spec(),
            },
            AgentEvent::TabOpened {
                id: id.clone(),
                parent: None,
                role: TabRole::Builder,
                tab_handle: sample_tab_handle(),
                label: "x".into(),
            },
            AgentEvent::TabClosed {
                id: id.clone(),
                tab_handle: sample_tab_handle(),
            },
            AgentEvent::Progress {
                id: id.clone(),
                done: 0,
                total: 1,
                label: None,
            },
            AgentEvent::TaskDone {
                id: id.clone(),
                task_id: "t".into(),
                label: None,
            },
            AgentEvent::Iteration {
                id: id.clone(),
                n: 0,
                max: None,
            },
            AgentEvent::PhaseTransition {
                id: id.clone(),
                from: None,
                to: "running".into(),
            },
            AgentEvent::ToolUse {
                id: id.clone(),
                tool: "t".into(),
                input_summary: "s".into(),
            },
            AgentEvent::Message {
                id: id.clone(),
                role: MessageRole::User,
                summary: "s".into(),
            },
            AgentEvent::FileEdited {
                id: id.clone(),
                path: PathBuf::from("p"),
                additions: 0,
                deletions: 0,
            },
            AgentEvent::ReviewComment {
                id: id.clone(),
                reviewer: id.clone(),
                severity: Severity::P2,
                path: PathBuf::from("p"),
                line: None,
                body: "b".into(),
            },
            AgentEvent::PermissionAsked {
                id: id.clone(),
                tool: "t".into(),
                summary: "s".into(),
            },
            AgentEvent::PermissionResolved {
                id: id.clone(),
                tool: "t".into(),
                decision: PermissionDecision::Allowed,
            },
            AgentEvent::Stall {
                id: id.clone(),
                since: Utc::now(),
            },
            AgentEvent::Log {
                id: id.clone(),
                level: LogLevel::Debug,
                line: "l".into(),
            },
            AgentEvent::Error {
                id: id.clone(),
                message: "m".into(),
            },
            AgentEvent::Done {
                id,
                outcome: Outcome::Killed,
            },
            AgentEvent::UserEvent {
                name: "myns.event".into(),
                source: "scene".into(),
                payload: serde_json::json!({"k": "v"}),
            },
        ];
        assert_eq!(_events.len(), 18);
    }
}
