//! CEL context construction for scene reactions and keybind guards.
//!
//! Bridges the typed `AgentEvent` / agent / session inputs into
//! the `cel_interpreter::Context` that backs `when=` and `if=`
//! predicate evaluation (see `cavekit-scene.md` R4 + R8).
//!
//! # K8s-style event shape
//!
//! The admission-policy flavour of CEL that Kubernetes ships uses a
//! single `request.*` namespace whose fields change shape depending
//! on the request kind, but every request always exposes
//! `request.kind`. Scenes adopt the same muscle memory:
//!
//! - `event.kind` is always present and carries the `AgentEvent`
//!   variant discriminator in `snake_case`
//!   (`"phase_transition"`, `"tool_use"`, `"user_event"`, etc.).
//! - Every field specific to a variant is flat-mapped onto `event.*`.
//!   `AgentEvent::PhaseTransition { from, to }` exposes `event.from`
//!   and `event.to`; `AgentEvent::Progress { done, total, label }`
//!   exposes `event.done`, `event.total`, `event.label`; and so on.
//! - Accessing a field that isn't present on the current variant is
//!   a CEL error (`no such key`). Predicates guard against this with
//!   short-circuits — `event.kind == "phase_transition" && event.to == "review"`.
//!
//! The `payload` binding is a convenience alias: for
//! `AgentEvent::UserEvent`, `event.payload` and the top-level
//! `payload` variable point at the same JSON blob, so user-event
//! predicates can read `payload.foo.bar` directly. For all other
//! variants, `payload` is the explicit argument to
//! [`build_context`], or `null` when omitted.
//!
//! # Placeholder snapshots
//!
//! [`AgentSnapshot`] and [`SessionSnapshot`] are **intentionally
//! minimal** and live here instead of in `crates/types/` for v1.
//! TODO(scene/tier-3): migrate these to `crates/types/` once the
//! wider state layer (orchestrator cache, supervisor status feed)
//! needs them. Until then, the supervisor can construct them
//! ad-hoc at reaction-dispatch time from `AgentSpec`.

use cel_interpreter::Context;
use serde::Serialize;
use serde_json::Value as JsonValue;

use ark_types::event::AgentEvent;

use crate::error::SceneError;

/// Minimal agent snapshot bound to `agent.*` in CEL.
///
/// Fields mirror what scenes typically need to guard against: agent
/// identity, the engine/orchestrator stack, and the spawn command
/// line.
///
/// TODO(scene/tier-3): migrate to `crates/types/` once the
/// supervisor's agent-cache lands (Tier 5 in
/// `context/plans/build-site-scene.md`).
#[derive(Clone, Debug, Serialize)]
pub struct AgentSnapshot {
    /// Full `AgentId` string (e.g. `"cavekit-auth-01jx7z8k6x9y2zt4abcdef0123"`).
    pub id: String,
    /// Human-readable label (e.g. `"builder"`).
    pub name: String,
    /// Orchestrator name (`"cavekit"`, `"claude-code"`, …).
    pub orchestrator: String,
    /// Engine name (`"claude-code"`, `"codex"`, …).
    pub engine: String,
    /// Worktree path the agent is running in.
    pub cwd: String,
    /// First argv token.
    pub cmd: String,
    /// Remaining argv tokens.
    pub args: Vec<String>,
}

/// Minimal session snapshot bound to `session.*` in CEL.
///
/// v1 carries only the human session name — enough for predicates
/// like `session.name.startsWith("ark-")`. The wider session state
/// (tab list, active pane, mux details) arrives in later tiers.
///
/// TODO(scene/tier-3): migrate to `crates/types/`.
#[derive(Clone, Debug, Serialize)]
pub struct SessionSnapshot {
    /// Session name (e.g. `"ark-cavekit-auth"`).
    pub name: String,
}

/// Build a CEL context wired with `event`, `payload`, `agent`,
/// `session` bindings for the given inputs.
///
/// The returned context has the scene custom functions
/// (`matches`, `glob`, `starts_with`, `ends_with`, `contains`,
/// `size`) pre-registered via
/// [`crate::cel::register_custom_functions`].
///
/// `payload` is optional. When present, it's bound as a top-level
/// `payload` variable in CEL. For `AgentEvent::UserEvent`, the
/// user event's intrinsic payload is also exposed as
/// `event.payload`.
///
/// Returns [`SceneError::CelEvaluate`] if the event (or the
/// snapshot types) fail to serialize — in practice that cannot
/// happen for `AgentEvent` + `AgentSnapshot` as authored here, but
/// we preserve a Result surface to keep the caller's error
/// handling uniform.
pub fn build_context<'a>(
    event: &AgentEvent,
    payload: Option<&JsonValue>,
    agent: &AgentSnapshot,
    session: &SessionSnapshot,
) -> Result<Context<'a>, SceneError> {
    // Start from a default context so the CEL stdlib is available
    // (`matches`, `contains`, `size`, etc.), then layer the scene
    // custom functions on top.
    let mut ctx = Context::default();
    crate::cel::register_custom_functions(&mut ctx);

    // Serialize the event to its serde JSON representation. The
    // serde(tag = "kind", rename_all = "snake_case") shape on
    // AgentEvent means the output is already a flat map with
    // `kind` set to the snake_case discriminator. Perfect match
    // for the k8s admission-style flat-map this module promises.
    let event_json = serde_json::to_value(event).map_err(|e| SceneError::CelEvaluate {
        message: format!("failed to serialize AgentEvent: {e}"),
    })?;

    ctx.add_variable("event", event_json.clone())
        .map_err(|e| SceneError::CelEvaluate {
            message: format!("failed to bind `event`: {e}"),
        })?;

    // Resolve the top-level `payload` binding. Priority:
    //  1. explicit payload argument (any variant),
    //  2. for UserEvent, fall back to the event's intrinsic payload,
    //  3. otherwise null.
    let payload_value = match payload {
        Some(p) => p.clone(),
        None => event_json
            .get("payload")
            .cloned()
            .unwrap_or(JsonValue::Null),
    };
    ctx.add_variable("payload", payload_value)
        .map_err(|e| SceneError::CelEvaluate {
            message: format!("failed to bind `payload`: {e}"),
        })?;

    ctx.add_variable("agent", serde_json::to_value(agent).unwrap_or(JsonValue::Null))
        .map_err(|e| SceneError::CelEvaluate {
            message: format!("failed to bind `agent`: {e}"),
        })?;

    ctx.add_variable(
        "session",
        serde_json::to_value(session).unwrap_or(JsonValue::Null),
    )
    .map_err(|e| SceneError::CelEvaluate {
        message: format!("failed to bind `session`: {e}"),
    })?;

    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cel::{compile, eval, eval_bool};
    use ark_types::event::{
        AgentEvent, LogLevel, MessageRole, Outcome, PermissionDecision, Severity, TabHandle,
        TabRole,
    };
    use ark_types::id::AgentId;
    use ark_types::spec::AgentSpec;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn agent_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    fn spec() -> AgentSpec {
        let mut spec = AgentSpec::new(
            agent_id(),
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into(), "--resume".into()],
        );
        spec.env = BTreeMap::new();
        spec
    }

    fn tab_handle() -> TabHandle {
        TabHandle::new("ark-cavekit-auth", 1, "builder")
    }

    fn agent_snap() -> AgentSnapshot {
        AgentSnapshot {
            id: agent_id().to_string(),
            name: "builder".into(),
            orchestrator: "cavekit".into(),
            engine: "claude-code".into(),
            cwd: "/tmp/worktree".into(),
            cmd: "claude".into(),
            args: vec!["--resume".into()],
        }
    }

    fn session_snap() -> SessionSnapshot {
        SessionSnapshot {
            name: "ark-cavekit-auth".into(),
        }
    }

    /// Helper: build a context and evaluate `expr` as a bool, asserting
    /// the expected outcome.
    fn assert_eval_bool(event: &AgentEvent, expr: &str, expected: bool) {
        let agent = agent_snap();
        let session = session_snap();
        let ctx = build_context(event, None, &agent, &session).expect("build context");
        let prog = compile(expr, "test", 0).expect("compile");
        let got = eval_bool(&prog, &ctx).unwrap_or_else(|e| {
            panic!("eval `{expr}` failed: {e}");
        });
        assert_eq!(got, expected, "expr `{expr}` on event {event:?}");
    }

    // -----------------------------------------------------------------
    // One test per AgentEvent variant: assert event.kind snake_case +
    // one variant-specific field access.
    // -----------------------------------------------------------------

    #[test]
    fn started_variant_kind_and_field() {
        let ev = AgentEvent::Started { spec: spec() };
        assert_eval_bool(&ev, r#"event.kind == "started""#, true);
        // `spec` is flat-mapped onto `event.spec`.
        assert_eval_bool(&ev, r#"event.spec.name == "auth""#, true);
    }

    #[test]
    fn tab_opened_variant_kind_and_field() {
        let ev = AgentEvent::TabOpened {
            id: agent_id(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab_handle(),
            label: "main".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "tab_opened""#, true);
        assert_eval_bool(&ev, r#"event.label == "main""#, true);
    }

    #[test]
    fn tab_closed_variant_kind_and_field() {
        let ev = AgentEvent::TabClosed {
            id: agent_id(),
            tab_handle: tab_handle(),
        };
        assert_eval_bool(&ev, r#"event.kind == "tab_closed""#, true);
        assert_eval_bool(&ev, r#"event.tab_handle.tab_index == 1"#, true);
    }

    #[test]
    fn progress_variant_kind_and_field() {
        let ev = AgentEvent::Progress {
            id: agent_id(),
            done: 3,
            total: 10,
            label: Some("step".into()),
        };
        assert_eval_bool(&ev, r#"event.kind == "progress""#, true);
        assert_eval_bool(&ev, r#"event.done == 3 && event.total == 10"#, true);
    }

    #[test]
    fn task_done_variant_kind_and_field() {
        let ev = AgentEvent::TaskDone {
            id: agent_id(),
            task_id: "T-005".into(),
            label: None,
        };
        assert_eval_bool(&ev, r#"event.kind == "task_done""#, true);
        assert_eval_bool(&ev, r#"event.task_id == "T-005""#, true);
    }

    #[test]
    fn iteration_variant_kind_and_field() {
        let ev = AgentEvent::Iteration {
            id: agent_id(),
            n: 2,
            max: Some(5),
        };
        assert_eval_bool(&ev, r#"event.kind == "iteration""#, true);
        assert_eval_bool(&ev, r#"event.n == 2"#, true);
    }

    #[test]
    fn phase_transition_variant_kind_and_field() {
        let ev = AgentEvent::PhaseTransition {
            id: agent_id(),
            from: Some("starting".into()),
            to: "running".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "phase_transition""#, true);
        assert_eval_bool(&ev, r#"event.to == "running""#, true);
    }

    #[test]
    fn tool_use_variant_kind_and_field() {
        let ev = AgentEvent::ToolUse {
            id: agent_id(),
            tool: "Read".into(),
            input_summary: "foo.rs".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "tool_use""#, true);
        assert_eval_bool(&ev, r#"event.tool == "Read""#, true);
    }

    #[test]
    fn message_variant_kind_and_field() {
        let ev = AgentEvent::Message {
            id: agent_id(),
            role: MessageRole::Assistant,
            summary: "hi".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "message""#, true);
        assert_eval_bool(&ev, r#"event.role == "assistant""#, true);
    }

    #[test]
    fn file_edited_variant_kind_and_field() {
        let ev = AgentEvent::FileEdited {
            id: agent_id(),
            path: PathBuf::from("src/lib.rs"),
            additions: 10,
            deletions: 2,
        };
        assert_eval_bool(&ev, r#"event.kind == "file_edited""#, true);
        assert_eval_bool(&ev, r#"event.additions == 10"#, true);
    }

    #[test]
    fn review_comment_variant_kind_and_field() {
        let ev = AgentEvent::ReviewComment {
            id: agent_id(),
            reviewer: AgentId::new("cavekit", "reviewer"),
            severity: Severity::P1,
            path: PathBuf::from("src/lib.rs"),
            line: Some(42),
            body: "fix".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "review_comment""#, true);
        assert_eval_bool(&ev, r#"event.severity == "p1""#, true);
    }

    #[test]
    fn permission_asked_variant_kind_and_field() {
        let ev = AgentEvent::PermissionAsked {
            id: agent_id(),
            tool: "Bash".into(),
            summary: "rm -rf /".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "permission_asked""#, true);
        assert_eval_bool(&ev, r#"event.tool == "Bash""#, true);
    }

    #[test]
    fn permission_resolved_variant_kind_and_field() {
        let ev = AgentEvent::PermissionResolved {
            id: agent_id(),
            tool: "Bash".into(),
            decision: PermissionDecision::Denied,
        };
        assert_eval_bool(&ev, r#"event.kind == "permission_resolved""#, true);
        assert_eval_bool(&ev, r#"event.decision == "denied""#, true);
    }

    #[test]
    fn stall_variant_kind_and_field() {
        let ev = AgentEvent::Stall {
            id: agent_id(),
            since: Utc::now(),
        };
        assert_eval_bool(&ev, r#"event.kind == "stall""#, true);
        // `since` serializes to an RFC3339 string — just test type access.
        let agent = agent_snap();
        let session = session_snap();
        let ctx = build_context(&ev, None, &agent, &session).unwrap();
        let prog = compile("size(event.since) > 0", "t", 0).unwrap();
        assert!(eval_bool(&prog, &ctx).unwrap());
    }

    #[test]
    fn log_variant_kind_and_field() {
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "hello".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "log""#, true);
        assert_eval_bool(&ev, r#"event.level == "info""#, true);
    }

    #[test]
    fn error_variant_kind_and_field() {
        let ev = AgentEvent::Error {
            id: agent_id(),
            message: "boom".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "error""#, true);
        assert_eval_bool(&ev, r#"event.message == "boom""#, true);
    }

    #[test]
    fn done_variant_kind_and_field() {
        let ev = AgentEvent::Done {
            id: agent_id(),
            outcome: Outcome::Killed,
        };
        assert_eval_bool(&ev, r#"event.kind == "done""#, true);
        // Outcome is untagged on unit variants -> bare `"killed"` string.
        assert_eval_bool(&ev, r#"event.outcome == "killed""#, true);
    }

    #[test]
    fn user_event_variant_kind_and_field() {
        let ev = AgentEvent::UserEvent {
            name: "ark.acp.permission_requested".into(),
            payload: serde_json::json!({"tool": "Bash", "id": "req-1"}),
            source: "core".into(),
        };
        assert_eval_bool(&ev, r#"event.kind == "user_event""#, true);
        assert_eval_bool(
            &ev,
            r#"event.name == "ark.acp.permission_requested""#,
            true,
        );
        // UserEvent's payload is visible under both event.payload.* and
        // the top-level payload.* binding.
        assert_eval_bool(&ev, r#"event.payload.tool == "Bash""#, true);
        assert_eval_bool(&ev, r#"payload.tool == "Bash""#, true);
    }

    // -----------------------------------------------------------------
    // Cross-cutting checks.
    // -----------------------------------------------------------------

    /// The k8s-style short-circuit pattern from `cavekit-scene.md` R4:
    /// guard a variant-specific field with `event.kind == "..."`.
    #[test]
    fn short_circuit_guard_pattern() {
        let ev = AgentEvent::Progress {
            id: agent_id(),
            done: 5,
            total: 10,
            label: None,
        };
        // Progress has `done` but not `to` — guarded access is safe.
        assert_eval_bool(
            &ev,
            r#"event.kind == "progress" && event.done == 5"#,
            true,
        );
        // Guarded access to a field from a different variant — the
        // `&&` short-circuit means we never touch `event.to`.
        assert_eval_bool(
            &ev,
            r#"event.kind == "phase_transition" && event.to == "review""#,
            false,
        );
    }

    /// Unguarded access to a non-existent field on the current
    /// variant is a CEL `no such key` error (cel/evaluate diag).
    #[test]
    fn unguarded_missing_field_errors() {
        use crate::error::ErrorCode;
        let ev = AgentEvent::Progress {
            id: agent_id(),
            done: 5,
            total: 10,
            label: None,
        };
        let agent = agent_snap();
        let session = session_snap();
        let ctx = build_context(&ev, None, &agent, &session).unwrap();
        let prog = compile(r#"event.to == "foo""#, "t", 0).unwrap();
        let err = eval_bool(&prog, &ctx).expect_err("should fail");
        assert_eq!(err.code_enum(), ErrorCode::CelEvaluate);
    }

    /// The `agent.*` and `session.*` bindings are readable alongside
    /// event fields.
    #[test]
    fn agent_and_session_bindings_resolve() {
        let ev = AgentEvent::Progress {
            id: agent_id(),
            done: 1,
            total: 2,
            label: None,
        };
        assert_eval_bool(&ev, r#"agent.name == "builder""#, true);
        assert_eval_bool(&ev, r#"starts_with(session.name, "ark-")"#, true);
    }

    /// Explicit `payload` argument wins over `event.payload` (for
    /// non-UserEvent variants, `event.payload` doesn't exist anyway).
    #[test]
    fn explicit_payload_wins_over_event_payload() {
        let ev = AgentEvent::Progress {
            id: agent_id(),
            done: 1,
            total: 2,
            label: None,
        };
        let agent = agent_snap();
        let session = session_snap();
        let payload = serde_json::json!({"custom": 42});
        let ctx = build_context(&ev, Some(&payload), &agent, &session).unwrap();
        let prog = compile(r#"payload.custom == 42"#, "t", 0).unwrap();
        assert!(eval_bool(&prog, &ctx).unwrap());
    }

    /// For non-UserEvent variants with no explicit payload argument,
    /// `payload` binds to `null`.
    #[test]
    fn missing_payload_defaults_to_null() {
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        let agent = agent_snap();
        let session = session_snap();
        let ctx = build_context(&ev, None, &agent, &session).unwrap();
        let prog = compile("payload == null", "t", 0).unwrap();
        assert!(eval_bool(&prog, &ctx).unwrap());
    }

    /// The returned context carries the scene custom functions.
    /// Sanity-check `glob` resolves (not a default Context function).
    #[test]
    fn custom_functions_available_on_built_context() {
        let ev = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        let agent = agent_snap();
        let session = session_snap();
        let ctx = build_context(&ev, None, &agent, &session).unwrap();
        let prog = compile(r#"glob("foo/bar.rs", "**/*.rs")"#, "t", 0).unwrap();
        let v = eval(&prog, &ctx).unwrap();
        assert_eq!(v, cel_interpreter::Value::Bool(true));
    }
}
