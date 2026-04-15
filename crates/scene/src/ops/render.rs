//! Runtime template rendering for op string args (T-4.4).
//!
//! Op string args (`exec script="..."`, `pipe { text "..." }`,
//! `set_status text="..."`, …) all pass through minijinja's
//! [`UndefinedBehavior::Chainable`] runtime renderer at dispatch time
//! per R9 + T-2.5. The event / payload / agent / session bindings come
//! from the reaction's firing event, so every op rendering sees the
//! same context as the surrounding CEL guards.
//!
//! # Why per-op, not generic-over-SHAPE
//!
//! The task brief allows either a generic `render_args<T>` walker built
//! on facet SHAPE reflection, or per-op string-arg lists, with the
//! latter as a documented fallback. We took the fallback:
//!
//! * facet's SHAPE surface is read-only — it exposes types, field
//!   attributes, and doc-comments, but not in-place mutation. The
//!   `Peek` API (facet 0.42+) can read heap values, but `Poke` (for
//!   writing) requires an owned partial-initialized pointer, which
//!   doesn't compose with "I already have a parsed `&mut Args`".
//! * Even with `Poke`, walking an arbitrary struct to find every
//!   `String` field recursively is a lot of reflection machinery for
//!   thirteen ops with at most a handful of templatable strings each.
//! * The per-op approach keeps the template coverage explicit — a
//!   reviewer can grep `render::render_*_args` to see exactly which
//!   fields template-expand, which is more legible than "trust the
//!   walker to find them all."
//!
//! TODO(facet-reflection): when facet-poke matures (0.43+?), collapse
//! these into a single `render_args<T: Facet>(&mut T, ctx) -> ()`
//! walker. Until then, adding a new templatable field = one-line edit
//! in the per-op helper.
//!
//! # Dispatch integration
//!
//! [`dispatch::dispatch_sequence`][crate::ops::dispatch::dispatch_sequence]
//! calls these helpers after parsing args and before invoking
//! `Intent::dispatch`. An op that doesn't template any field (e.g.
//! `focus_tab`) has no entry here — the dispatcher skips the render
//! step for it.

use ark_types::event::AgentEvent;
use serde_json::Value as JsonValue;

use crate::context::{AgentSnapshot, SessionSnapshot};
use crate::error::SceneError;
use crate::template::runtime_render;

use super::control::ExecArgs;
use super::messaging::{EmitArgs, PipeArgs, SetStatusArgs};
use super::panes::SplitPaneArgs;
use super::plugins::{MountPluginArgs, UnmountPluginArgs};
use super::tabs::{CloseTabArgs, FocusTabArgs, OpenTabArgs, RenameTabArgs};

/// Runtime context fed into every template render.
///
/// Bundles the bindings the T-2.5 renderer expects (`event`, `payload`,
/// `agent`, `session`). Cheap to clone — the heavy field is
/// `payload: Option<JsonValue>` and a reaction typically shares one
/// render context across every op in the body.
#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    /// Firing event — bound to `event.*` (and `event.payload.*` for
    /// `UserEvent`).
    pub event: &'a AgentEvent,
    /// Explicit payload override (takes priority over `event.payload`).
    pub payload: Option<&'a JsonValue>,
    /// Agent snapshot — bound to `agent.*`.
    pub agent: &'a AgentSnapshot,
    /// Session snapshot — bound to `session.*`.
    pub session: &'a SessionSnapshot,
}

impl<'a> RenderContext<'a> {
    /// Render a single template string against the context. Thin
    /// wrapper around [`runtime_render`] so call sites don't have to
    /// repeat the four-argument destructuring.
    pub fn render(&self, template: &str) -> Result<String, SceneError> {
        runtime_render(template, self.event, self.payload, self.agent, self.session)
    }

    /// Render `template` in place, replacing the caller's string with
    /// the rendered output. On error, the input is left unchanged and
    /// the caller handles the diagnostic per the dispatcher's
    /// fail-fast contract (T-4.5).
    pub fn render_in_place(&self, target: &mut String) -> Result<(), SceneError> {
        let out = self.render(target)?;
        *target = out;
        Ok(())
    }

    /// Render an optional string in place. `None` inputs are left as
    /// `None`; `Some(s)` gets `s` rendered through [`Self::render`].
    pub fn render_opt(&self, target: &mut Option<String>) -> Result<(), SceneError> {
        if let Some(s) = target.as_mut() {
            self.render_in_place(s)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Per-op render helpers
// ---------------------------------------------------------------------------

/// Render the string args on [`OpenTabArgs`] — `name` + `layout`.
pub fn render_open_tab(args: &mut OpenTabArgs, ctx: &RenderContext<'_>) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.name)?;
    ctx.render_opt(&mut args.layout)?;
    Ok(())
}

/// Render the string args on [`CloseTabArgs`] — only `name`.
pub fn render_close_tab(
    args: &mut CloseTabArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_opt(&mut args.name)?;
    Ok(())
}

/// Render the string args on [`RenameTabArgs`] — `name` + `to`.
pub fn render_rename_tab(
    args: &mut RenameTabArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_opt(&mut args.name)?;
    ctx.render_in_place(&mut args.to)?;
    Ok(())
}

/// Render the string args on [`FocusTabArgs`] — only `name`.
pub fn render_focus_tab(
    args: &mut FocusTabArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_opt(&mut args.name)?;
    Ok(())
}

/// Render the string args on [`SplitPaneArgs`] — `into`, `side`, `size`,
/// `command { value }`, `cwd { value }`. `side` is templated even
/// though the final value must land in the static set of four — the
/// template simply resolves to one of those strings at render time.
pub fn render_split_pane(
    args: &mut SplitPaneArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.into)?;
    ctx.render_in_place(&mut args.side)?;
    ctx.render_opt(&mut args.size)?;
    if let Some(cmd) = args.command.as_mut() {
        ctx.render_in_place(&mut cmd.value)?;
    }
    if let Some(cwd) = args.cwd.as_mut() {
        ctx.render_in_place(&mut cwd.value)?;
    }
    Ok(())
}

/// Render the string args on [`MountPluginArgs`] — `name`, `at`, `into`.
pub fn render_mount_plugin(
    args: &mut MountPluginArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.name)?;
    ctx.render_opt(&mut args.at)?;
    ctx.render_opt(&mut args.into)?;
    Ok(())
}

/// Render the string args on [`UnmountPluginArgs`] — only `name`.
pub fn render_unmount_plugin(
    args: &mut UnmountPluginArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.name)?;
    Ok(())
}

/// Render the string args on [`PipeArgs`] — `plugin`, `severity`,
/// `name`, plus the `text`/`json` body.
pub fn render_pipe(args: &mut PipeArgs, ctx: &RenderContext<'_>) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.plugin)?;
    ctx.render_opt(&mut args.severity)?;
    ctx.render_opt(&mut args.name)?;
    if let Some(n) = args.text.as_mut() {
        ctx.render_in_place(&mut n.value)?;
    }
    if let Some(n) = args.json.as_mut() {
        ctx.render_in_place(&mut n.value)?;
    }
    Ok(())
}

/// Render the string args on [`EmitArgs`] — `name` + optional `json` body.
pub fn render_emit(args: &mut EmitArgs, ctx: &RenderContext<'_>) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.name)?;
    if let Some(n) = args.json.as_mut() {
        ctx.render_in_place(&mut n.value)?;
    }
    Ok(())
}

/// Render the string args on [`SetStatusArgs`] — `text` + `severity`.
pub fn render_set_status(
    args: &mut SetStatusArgs,
    ctx: &RenderContext<'_>,
) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.text)?;
    ctx.render_opt(&mut args.severity)?;
    Ok(())
}

/// Render the string args on [`ExecArgs`] — `script`, `shell`, `cwd`,
/// and every `var value="..."` entry in the `env { }` block. (`var
/// name="..."` is NOT templated — env var names are identifiers, not
/// user-facing strings; if a scene needs a dynamic env name, it should
/// template a higher-level wrapper.)
pub fn render_exec(args: &mut ExecArgs, ctx: &RenderContext<'_>) -> Result<(), SceneError> {
    ctx.render_in_place(&mut args.script)?;
    ctx.render_opt(&mut args.shell)?;
    ctx.render_opt(&mut args.cwd)?;
    if let Some(env) = args.env.as_mut() {
        for v in env.vars.iter_mut() {
            ctx.render_in_place(&mut v.value)?;
        }
    }
    Ok(())
}

/// `reload_scene` has no templatable string args today — included for
/// symmetry so callers can fold over the same "every op has a render
/// hook" shape when T-4.5's dispatcher threads this in.
pub fn render_reload_scene(_ctx: &RenderContext<'_>) -> Result<(), SceneError> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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

    fn log_event() -> AgentEvent {
        AgentEvent::Log {
            id: agent_id(),
            level: LogLevel::Info,
            line: "x".into(),
        }
    }

    fn user_event_with_payload(payload: serde_json::Value) -> AgentEvent {
        AgentEvent::UserEvent {
            name: "user.test".into(),
            payload,
            source: "scene".into(),
        }
    }

    /// T-4.4 acceptance criterion: `exec script="cargo test {{ payload.filter }}"`
    /// renders at fire time with a real payload substitution.
    #[test]
    fn exec_script_renders_payload_filter() {
        let ev = user_event_with_payload(serde_json::json!({"filter": "foo_test"}));
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = ExecArgs {
            script: "cargo test {{ payload.filter }}".into(),
            shell: None,
            timeout_ms: None,
            cwd: None,
            env: None,
        };
        render_exec(&mut args, &ctx).expect("render");
        assert_eq!(args.script, "cargo test foo_test");
    }

    /// T-4.4 acceptance criterion: undefined chain → empty string
    /// (chainable undefined behavior).
    #[test]
    fn exec_script_undefined_chain_renders_empty() {
        let ev = log_event();
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = ExecArgs {
            script: "echo [{{ payload.missing.deep }}]".into(),
            shell: None,
            timeout_ms: None,
            cwd: None,
            env: None,
        };
        render_exec(&mut args, &ctx).expect("render");
        assert_eq!(args.script, "echo []");
    }

    /// Multi-field render: every string field on `SplitPaneArgs` gets
    /// rendered.
    #[test]
    fn split_pane_renders_every_string_field() {
        let ev = user_event_with_payload(serde_json::json!({
            "tab": "work",
            "side": "right"
        }));
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = SplitPaneArgs {
            into: "{{ payload.tab }}".into(),
            side: "{{ payload.side }}".into(),
            size: Some("50%".into()),
            command: None,
            cwd: None,
        };
        render_split_pane(&mut args, &ctx).expect("render");
        assert_eq!(args.into, "work");
        assert_eq!(args.side, "right");
        assert_eq!(args.size.as_deref(), Some("50%"));
    }

    /// `set_status text="build={{ agent.name }}"` resolves against the
    /// agent snapshot.
    #[test]
    fn set_status_text_renders_agent_field() {
        let ev = log_event();
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = SetStatusArgs {
            text: "build={{ agent.name }}".into(),
            severity: Some("info".into()),
            ttl_ms: None,
        };
        render_set_status(&mut args, &ctx).expect("render");
        assert_eq!(args.text, "build=builder");
    }

    /// `emit name="..."` name is templated (rare but supported).
    #[test]
    fn emit_name_renders_template() {
        let ev = user_event_with_payload(serde_json::json!({"suffix": "ready"}));
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = EmitArgs {
            name: "user.{{ payload.suffix }}".into(),
            json: None,
        };
        render_emit(&mut args, &ctx).expect("render");
        assert_eq!(args.name, "user.ready");
    }

    /// `pipe { text "..." }` body templates resolve.
    #[test]
    fn pipe_text_body_renders() {
        let ev = user_event_with_payload(serde_json::json!({"kind": "build"}));
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = PipeArgs {
            plugin: "status".into(),
            severity: None,
            name: None,
            text: Some(super::super::messaging::PipeTextNode {
                value: "{{ payload.kind }} started".into(),
            }),
            json: None,
        };
        render_pipe(&mut args, &ctx).expect("render");
        assert_eq!(args.text.as_ref().unwrap().value, "build started");
    }

    /// `exec { env { var name="K" value="{{ ... }}" } }` renders `value`
    /// but leaves `name` as a literal identifier.
    #[test]
    fn exec_env_var_value_renders_but_name_stays_literal() {
        let ev = user_event_with_payload(serde_json::json!({"host": "localhost"}));
        let agent = sample_agent();
        let session = sample_session();
        let ctx = RenderContext {
            event: &ev,
            payload: None,
            agent: &agent,
            session: &session,
        };
        let mut args = ExecArgs {
            script: "env | grep TARGET".into(),
            shell: None,
            timeout_ms: None,
            cwd: None,
            env: Some(super::super::control::ExecEnvBlock {
                vars: vec![super::super::control::ExecEnvVarNode {
                    name: "TARGET".into(),
                    value: "{{ payload.host }}:8080".into(),
                }],
            }),
        };
        render_exec(&mut args, &ctx).expect("render");
        let v = &args.env.as_ref().unwrap().vars[0];
        assert_eq!(v.name, "TARGET");
        assert_eq!(v.value, "localhost:8080");
    }
}
