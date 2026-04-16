//! Minijinja-backed template rendering for scenes.
//!
//! Scenes emit two kinds of templates (see `cavekit-scene.md` R9):
//!
//! 1. **Compile-time** — inline `layout` snippets the scene compiler
//!    expands with a bounded `LayoutVars` surface (`cwd`, `agent_cmd`,
//!    `agent_args`, `id`, `name`). Undefined vars are a hard compile
//!    error because a layout that silently drops `{{ cwd }}` produces
//!    a broken zellij session with no useful diagnostic. This matches
//!    the `crates/mux/zellij/src/layout_template.rs` wrapper used by
//!    the runtime, so the two paths stay bug-compatible.
//!
//! 2. **Runtime** — `emit` / `ops` args rendered on every reaction
//!    firing. Here undefined access must be tolerated: payload
//!    shapes vary event-to-event, and a panic on a single missing
//!    field would wedge the whole reaction graph. The runtime
//!    renderer (T-2.5) uses `UndefinedBehavior::Chainable` and
//!    routes a trace of every undefined chain through `tracing::debug`
//!    so users can diagnose via `ark pane log`.
//!
//! Both paths share the same minijinja `Environment` builder — the
//! only knob that differs is `set_undefined_behavior`.

use minijinja::{Environment, UndefinedBehavior, Value};
use serde_json::Value as JsonValue;

use ark_types::event::AgentEvent;

use crate::context::{AgentSnapshot, SessionSnapshot};
use crate::error::SceneError;

/// Bounded variable surface for compile-time scene templating. Keyed
/// identically to the runtime `layout_template.rs` wrapper so a scene
/// template renders the same bytes whether ark or the zellij
/// supervisor owns the expansion.
///
/// Adding a new variable is an intentional breaking change — extend
/// this struct and update the rendering tests that snapshot the
/// surface.
#[derive(Clone, Debug)]
pub struct LayoutVars {
    /// Absolute worktree path (mirrors the fields in
    /// `crates/mux/zellij/src/layout_template.rs`).
    pub cwd: String,
    /// First argv token of the primary pane command.
    pub agent_cmd: String,
    /// Remaining argv tokens, exposed to templates as a list.
    pub agent_args: Vec<String>,
    /// Full `AgentId` string.
    pub id: String,
    /// Human-readable tab label.
    pub name: String,
}

impl LayoutVars {
    /// Convert into a minijinja context value.
    ///
    /// Kept separate so callers can reuse the same surface across
    /// multiple template renders without re-building it.
    pub fn to_context(&self) -> Value {
        minijinja::context! {
            cwd => self.cwd,
            agent_cmd => self.agent_cmd,
            agent_args => self.agent_args,
            id => self.id,
            name => self.name,
        }
    }
}

/// Build a minijinja environment configured for compile-time
/// rendering: `UndefinedBehavior::Strict` so `{{ missing }}` fails
/// loudly, and `keep_trailing_newline(true)` so rendered KDL
/// preserves the authored shape.
fn strict_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);
    env
}

/// Compile-time template render. Undefined variables are a hard
/// error ([`SceneError::TemplateRender`]); template-syntax errors
/// map to [`SceneError::TemplateCompile`].
///
/// Kept deliberately symmetric with
/// `crates/mux/zellij/src/layout_template.rs::render` so the scene
/// compiler and the runtime layout expander agree on the variable
/// surface.
///
/// # Example
/// ```
/// # use ark_scene_v2_archive::template::{compile_time_render, LayoutVars};
/// let vars = LayoutVars {
///     cwd: "/tmp/work".into(),
///     agent_cmd: "claude".into(),
///     agent_args: vec!["--resume".into()],
///     id: "cavekit-auth-01".into(),
///     name: "builder".into(),
/// };
/// let out = compile_time_render(r#"pane command="{{ agent_cmd }}""#, &vars).unwrap();
/// assert_eq!(out, r#"pane command="claude""#);
/// ```
pub fn compile_time_render(template: &str, vars: &LayoutVars) -> Result<String, SceneError> {
    let env = strict_env();
    let tmpl = env
        .template_from_str(template)
        .map_err(|e| SceneError::TemplateCompile {
            message: e.to_string(),
        })?;
    tmpl.render(vars.to_context())
        .map_err(|e| SceneError::TemplateRender {
            message: e.to_string(),
        })
}

/// Compile-only validation — parse the template but don't render it.
///
/// Used by the scene-check pass (T-2.6) to surface template syntax
/// errors even when the variable surface hasn't been fully resolved
/// yet. Returns [`SceneError::TemplateCompile`] on failure.
pub fn compile_only(template: &str) -> Result<(), SceneError> {
    let env = strict_env();
    env.template_from_str(template)
        .map(|_| ())
        .map_err(|e| SceneError::TemplateCompile {
            message: e.to_string(),
        })
}

/// Build a minijinja environment configured for **runtime** rendering:
/// `UndefinedBehavior::Chainable` so `{{ payload.a.b.c }}` on a
/// missing chain renders as the empty string instead of failing the
/// whole reaction.
///
/// The supervisor reads the `tracing::debug!(target = "scene::template")`
/// stream via `ark pane log` so users can inspect every undefined
/// access; this matches R9's requirement that runtime templates
/// "log every undefined-access trail".
fn chainable_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_keep_trailing_newline(true);
    env
}

/// Runtime template rendering. Undefined accesses are tolerated
/// (the rendered output is the empty string for the missing chain)
/// but every such access is emitted as a `tracing::debug!` event
/// under the `scene::template` target so users can diagnose via
/// `ark pane log` per R9.
///
/// Shape of the template context:
/// - `event` — flat-mapped `AgentEvent` shape (same as CEL R4/T-2.2).
/// - `payload` — explicit payload arg (or `event.payload` for
///   `UserEvent`, otherwise `null`).
/// - `agent` — [`AgentSnapshot`] serialized to JSON.
/// - `session` — [`SessionSnapshot`] serialized to JSON.
///
/// # Diagnostics
/// - Template syntax errors surface as [`SceneError::TemplateCompile`].
/// - Render errors other than undefined (e.g. bad filter invocation)
///   surface as [`SceneError::TemplateRender`].
/// - Undefined accesses themselves never produce an error — they're
///   rendered as empty string and logged at `debug`.
pub fn runtime_render(
    template: &str,
    event: &AgentEvent,
    payload: Option<&JsonValue>,
    agent: &AgentSnapshot,
    session: &SessionSnapshot,
) -> Result<String, SceneError> {
    let mjctx = build_minijinja_context(event, payload, agent, session)?;

    // Undefined-access tracer: first try a strict render solely to
    // collect every missing chain into the debug log. We swallow
    // the strict result entirely — the authoritative output comes
    // from the chainable pass below. This keeps the two passes
    // semantically distinct: strict is a probe, chainable is the
    // answer. Template compile and non-undefined render errors
    // still surface from the strict pass so we never emit a
    // chainable-rendered bogus string from a broken template.
    let strict = strict_env();
    let strict_tmpl = strict
        .template_from_str(template)
        .map_err(|e| SceneError::TemplateCompile {
            message: e.to_string(),
        })?;
    if let Err(err) = strict_tmpl.render(&mjctx) {
        if err.kind() == minijinja::ErrorKind::UndefinedError {
            // Emit a single `scene::template` debug event carrying
            // the full error chain. minijinja's Display impl for
            // `Error` includes the attribute path and the line the
            // access happened on, which is exactly what users need
            // to diagnose a stale selector.
            tracing::debug!(
                target = "scene::template",
                undefined = %err,
                "runtime template hit an undefined access (rendered as empty)"
            );
        } else {
            // Non-undefined error during strict render — escalate.
            return Err(SceneError::TemplateRender {
                message: err.to_string(),
            });
        }
    }

    let chainable = chainable_env();
    let tmpl = chainable
        .template_from_str(template)
        .map_err(|e| SceneError::TemplateCompile {
            message: e.to_string(),
        })?;
    tmpl.render(&mjctx).map_err(|e| SceneError::TemplateRender {
        message: e.to_string(),
    })
}

/// Build the minijinja context value for runtime rendering. Same
/// shape as the CEL context builder ([`crate::context::build_context`]):
/// `event` / `payload` / `agent` / `session` — so scene authors use
/// one mental model across both surfaces.
fn build_minijinja_context(
    event: &AgentEvent,
    payload: Option<&JsonValue>,
    agent: &AgentSnapshot,
    session: &SessionSnapshot,
) -> Result<Value, SceneError> {
    let event_json = serde_json::to_value(event).map_err(|e| SceneError::TemplateRender {
        message: format!("failed to serialize AgentEvent: {e}"),
    })?;
    let payload_value = match payload {
        Some(p) => p.clone(),
        None => event_json
            .get("payload")
            .cloned()
            .unwrap_or(JsonValue::Null),
    };
    let agent_json = serde_json::to_value(agent).unwrap_or(JsonValue::Null);
    let session_json = serde_json::to_value(session).unwrap_or(JsonValue::Null);
    Ok(minijinja::context! {
        event => Value::from_serialize(&event_json),
        payload => Value::from_serialize(&payload_value),
        agent => Value::from_serialize(&agent_json),
        session => Value::from_serialize(&session_json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn vars() -> LayoutVars {
        LayoutVars {
            cwd: "/tmp/work".into(),
            agent_cmd: "claude".into(),
            agent_args: vec!["--resume".into(), "--verbose".into()],
            id: "cavekit-auth-01".into(),
            name: "builder".into(),
        }
    }

    /// All five standard vars resolve in a single template.
    #[test]
    fn all_five_vars_substitute() {
        let tmpl = r#"layout {
    cwd "{{ cwd }}"
    tab name="{{ name }}" {
        pane command="{{ agent_cmd }}" id="{{ id }}" {
            args{% for a in agent_args %} "{{ a }}"{% endfor %}
        }
    }
}"#;
        let out = compile_time_render(tmpl, &vars()).unwrap();
        assert!(out.contains("cwd \"/tmp/work\""));
        assert!(out.contains("name=\"builder\""));
        assert!(out.contains("command=\"claude\""));
        assert!(out.contains("id=\"cavekit-auth-01\""));
        assert!(out.contains("\"--resume\""));
        assert!(out.contains("\"--verbose\""));
    }

    /// Undefined var → TemplateRender diagnostic with
    /// `scene/template-render` code.
    #[test]
    fn undefined_var_is_render_error() {
        let err = compile_time_render("{{ nope }}", &vars()).expect_err("strict should reject");
        assert_eq!(err.code_enum(), ErrorCode::TemplateRender);
    }

    /// Malformed template → TemplateCompile diagnostic with
    /// `scene/template-compile` code.
    #[test]
    fn syntax_error_is_compile_error() {
        let err = compile_time_render("{{ unclosed", &vars()).expect_err("syntax");
        assert_eq!(err.code_enum(), ErrorCode::TemplateCompile);
    }

    /// `compile_only` accepts a well-formed template even when the
    /// variable surface isn't bound yet.
    #[test]
    fn compile_only_accepts_valid_template() {
        compile_only("{{ cwd }} / {{ id }}").expect("valid");
    }

    /// `compile_only` rejects a malformed template with the compile
    /// error code.
    #[test]
    fn compile_only_rejects_syntax() {
        let err = compile_only("{% if %}").expect_err("bad if");
        assert_eq!(err.code_enum(), ErrorCode::TemplateCompile);
    }

    // -----------------------------------------------------------------
    // T-2.5: runtime_render tests
    // -----------------------------------------------------------------

    use ark_types::event::{AgentEvent, LogLevel};
    use ark_types::id::AgentId;

    fn sample_agent() -> AgentSnapshot {
        AgentSnapshot {
            id: "cavekit-auth-01".into(),
            name: "builder".into(),
            orchestrator: "cavekit".into(),
            engine: "claude-code".into(),
            cwd: "/tmp/work".into(),
            cmd: "claude".into(),
            args: vec!["--resume".into()],
        }
    }

    fn sample_session() -> SessionSnapshot {
        SessionSnapshot {
            name: "ark-cavekit-auth".into(),
        }
    }

    fn agent_id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    /// A successful runtime render resolves event, agent, session.
    #[test]
    fn runtime_render_happy_path() {
        let event = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "hello".into(),
        };
        let out = runtime_render(
            "{{ event.kind }} / {{ agent.name }} / {{ session.name }}",
            &event,
            None,
            &sample_agent(),
            &sample_session(),
        )
        .unwrap();
        assert_eq!(out, "log / builder / ark-cavekit-auth");
    }

    /// Chained access on a missing payload field renders empty
    /// (chainable undefined behavior) instead of erroring.
    #[test]
    fn runtime_render_chainable_undefined_renders_empty() {
        let event = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        let out = runtime_render(
            "before={{ payload.a.b.c }}after",
            &event,
            None,
            &sample_agent(),
            &sample_session(),
        )
        .unwrap();
        assert_eq!(out, "before=after");
    }

    /// Runtime render of a UserEvent exposes its payload via both
    /// event.payload.* and the top-level payload.*.
    #[test]
    fn runtime_render_user_event_payload_reachable() {
        let event = AgentEvent::UserEvent {
            name: "myns.hello".into(),
            payload: serde_json::json!({"greeting": "world"}),
            source: "scene".into(),
        };
        let out = runtime_render(
            "{{ event.name }}:{{ event.payload.greeting }}:{{ payload.greeting }}",
            &event,
            None,
            &sample_agent(),
            &sample_session(),
        )
        .unwrap();
        assert_eq!(out, "myns.hello:world:world");
    }

    /// Explicit payload wins over event.payload fallback.
    #[test]
    fn runtime_render_explicit_payload_wins() {
        let event = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        let payload = serde_json::json!({"k": "explicit"});
        let out = runtime_render(
            "{{ payload.k }}",
            &event,
            Some(&payload),
            &sample_agent(),
            &sample_session(),
        )
        .unwrap();
        assert_eq!(out, "explicit");
    }

    /// Syntax error maps to TemplateCompile.
    #[test]
    fn runtime_render_syntax_error_is_compile() {
        let event = AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        };
        let err = runtime_render(
            "{{ unclosed",
            &event,
            None,
            &sample_agent(),
            &sample_session(),
        )
        .expect_err("syntax error");
        assert_eq!(err.code_enum(), ErrorCode::TemplateCompile);
    }
}
