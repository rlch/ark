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
/// # use ark_scene::template::{compile_time_render, LayoutVars};
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
}
