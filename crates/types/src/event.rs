//! Core event stream.
//!
//! See cavekit-soul-phase-1-types.md R6. `CoreEvent` is the narrow set of
//! lifecycle + log events core cares about. Everything else rides in
//! `CoreEvent::Ext(ExtEvent)` — the extension-owned free-form bucket where
//! each extension tags its payloads with its manifest name.
//!
//! The previous `AgentEvent` enum (with `TabOpened`, `ToolUse`, `TaskDone`,
//! `PermissionAsked`, `PermissionResolved`, `Iteration`, `PhaseTransition`,
//! `FileEdited`, `ReviewComment`, `Stall`, `Message`, `UserEvent`, `Done`,
//! etc.) is deleted; supplementary enums (`TabRole`, `TabHandle`,
//! `MessageRole`, `PermissionDecision`) go with it. Those concepts re-home
//! inside extensions (Phase 4+).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::spec::SessionSpec;

/// Core-level log level. Survives from the old event surface since
/// `CoreEvent::Log` still carries a level.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Extension-emitted event ride-along. Every non-core event rides inside a
/// `CoreEvent::Ext(ExtEvent)` envelope so the core bus stays flat.
///
/// `ext` is the manifest name of the emitting extension (e.g. `"acp-client"`,
/// `"claude-code"`), `kind` is the extension's own event discriminator
/// (e.g. `"permission.asked"`, `"tool.use"`), and `payload` is free-form
/// extension-owned JSON.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtEvent {
    pub ext: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// The core event stream. Narrow by design — five variants only. Anything
/// methodology- or extension-flavoured rides in `Ext(ExtEvent)`.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    /// Free-form log line emitted by core or (via `Ext`) an extension that
    /// prefers the core log channel.
    Log {
        level: String,
        message: String,
        target: Option<String>,
    },
    /// Core-level error.
    Error { error: String },
    /// Session lifecycle: spawn.
    SessionStarted { spec: SessionSpec },
    /// Session lifecycle: termination.
    SessionEnded { terminated_at: DateTime<Utc> },
    /// Extension-emitted event. See [`ExtEvent`].
    Ext(ExtEvent),
}

/// Flattened event projection consumed by scene predicates, the event log,
/// and anything else that wants a `(name, payload)` pair without having to
/// pattern-match on [`CoreEvent`].
///
/// See cavekit-soul-phase-1-types.md R7. Core variants flatten to
/// `ark.core.<variant_snake>`; extension events flatten to `<ext>.<kind>`
/// with the payload passed through unchanged.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FlatEvent {
    /// Dotted event name — `ark.core.<variant>` for core, `<ext>.<kind>` for
    /// extension-emitted events.
    pub name: String,
    /// JSON payload — variant fields serialised as a JSON object for core
    /// events, the passed-through payload for extension events.
    pub payload: serde_json::Value,
}

impl From<&CoreEvent> for FlatEvent {
    fn from(ev: &CoreEvent) -> Self {
        match ev {
            CoreEvent::Log {
                level,
                message,
                target,
            } => FlatEvent {
                name: "ark.core.log".to_string(),
                payload: serde_json::json!({
                    "level": level,
                    "message": message,
                    "target": target,
                }),
            },
            CoreEvent::Error { error } => FlatEvent {
                name: "ark.core.error".to_string(),
                payload: serde_json::json!({ "error": error }),
            },
            CoreEvent::SessionStarted { spec } => FlatEvent {
                name: "ark.core.session_started".to_string(),
                payload: serde_json::json!({ "spec": spec }),
            },
            CoreEvent::SessionEnded { terminated_at } => FlatEvent {
                name: "ark.core.session_ended".to_string(),
                payload: serde_json::json!({ "terminated_at": terminated_at }),
            },
            CoreEvent::Ext(ext) => FlatEvent::from(ext),
        }
    }
}

impl From<&ExtEvent> for FlatEvent {
    fn from(ev: &ExtEvent) -> Self {
        FlatEvent {
            name: format!("{}.{}", ev.ext, ev.kind),
            payload: ev.payload.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> SessionSpec {
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        SessionSpec {
            id: crate::id::SessionId::new("foo"),
            name: "foo".to_string(),
            scene_path: Some(PathBuf::from("/tmp/demo.kdl")),
            cwd: PathBuf::from("/tmp/worktree"),
            env: BTreeMap::new(),
            created_at: Utc::now(),
            ext_config: BTreeMap::new(),
        }
    }

    #[test]
    fn core_event_serde_roundtrip() {
        // Exercise every variant.
        let variants = vec![
            CoreEvent::Log {
                level: "info".to_string(),
                message: "hello".to_string(),
                target: Some("core".to_string()),
            },
            CoreEvent::Error {
                error: "boom".to_string(),
            },
            CoreEvent::SessionStarted {
                spec: sample_spec(),
            },
            CoreEvent::SessionEnded {
                terminated_at: Utc::now(),
            },
            CoreEvent::Ext(ExtEvent {
                ext: "acp-client".to_string(),
                kind: "permission.asked".to_string(),
                payload: serde_json::json!({ "tool": "Bash" }),
            }),
        ];
        for ev in &variants {
            let json = serde_json::to_string(ev).expect("ser");
            let back: CoreEvent = serde_json::from_str(&json).expect("de");
            // Re-serialize and compare bytes — ExtEvent has no PartialEq.
            let back_json = serde_json::to_string(&back).expect("re-ser");
            assert_eq!(back_json, json, "roundtrip not stable: {json}");
        }
    }

    #[test]
    fn flat_event_from_core_session_started() {
        let ev = CoreEvent::SessionStarted {
            spec: sample_spec(),
        };
        let flat = FlatEvent::from(&ev);
        assert_eq!(flat.name, "ark.core.session_started");
        assert!(flat.payload.get("spec").is_some());
    }

    #[test]
    fn flat_event_from_core_ext() {
        let ev = CoreEvent::Ext(ExtEvent {
            ext: "claude-code".to_string(),
            kind: "tool.use".to_string(),
            payload: serde_json::json!({ "tool": "Read" }),
        });
        let flat = FlatEvent::from(&ev);
        assert_eq!(flat.name, "claude-code.tool.use");
        assert_eq!(flat.payload, serde_json::json!({ "tool": "Read" }));
    }

    #[test]
    fn flat_event_from_core_log_and_error() {
        let log = CoreEvent::Log {
            level: "info".to_string(),
            message: "hi".to_string(),
            target: None,
        };
        assert_eq!(FlatEvent::from(&log).name, "ark.core.log");
        let err = CoreEvent::Error {
            error: "boom".to_string(),
        };
        assert_eq!(FlatEvent::from(&err).name, "ark.core.error");
    }

    #[test]
    fn ext_event_serde_roundtrip() {
        let ev = ExtEvent {
            ext: "claude-code".to_string(),
            kind: "tool.use".to_string(),
            payload: serde_json::json!({ "name": "Read", "input": "foo.rs" }),
        };
        let json = serde_json::to_string(&ev).expect("ser");
        let back: ExtEvent = serde_json::from_str(&json).expect("de");
        assert_eq!(back.ext, ev.ext);
        assert_eq!(back.kind, ev.kind);
        assert_eq!(back.payload, ev.payload);
    }
}
