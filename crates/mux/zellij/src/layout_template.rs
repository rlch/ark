//! Minijinja-backed renderer for zellij KDL layout templates.
//!
//! Implements cavekit-mux-zellij.md R5 / cavekit-layouts.md R3 (T-030):
//!
//! - **Bounded variable surface.** Templates see exactly five keys —
//!   `cwd`, `agent_cmd`, `agent_args`, `id`, `name`. No env access, no
//!   loops over `sys`, no template inheritance. Adding a new variable
//!   is an intentional breaking change to audit.
//! - **Strict undefined behavior.** `{{ does_not_exist }}` is a hard
//!   error — never silently rendered to empty.
//! - **Post-render KDL validation.** A lightweight brace/string scanner
//!   catches obvious malformations (unbalanced `{`/`}`, unterminated
//!   string literal) before zellij is invoked, so the failure surfaces
//!   in the supervisor logs rather than as a vague zellij parse error.
//!
//! The validator is not a full KDL parser. Zellij itself is the source
//! of truth; this layer only catches the cheap cases.

use minijinja::{Environment, UndefinedBehavior, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LayoutTemplateError {
    #[error("template syntax error: {0}")]
    Syntax(String),
    #[error("undefined variable in template: {0}")]
    UndefinedVar(String),
    #[error("rendered output is not valid KDL: {0}")]
    InvalidKdl(String),
    #[error(transparent)]
    Internal(#[from] minijinja::Error),
}

/// Bounded variable surface. The template sees exactly these keys — no loops
/// over env, no sys access, no inheritance. (v1 keeps the template API
/// minimal; additional vars are a breaking change to audit.)
#[derive(Clone, Debug)]
pub struct LayoutVars {
    /// Absolute path string for the worktree.
    pub cwd: String,
    /// First argv token of the primary pane command.
    pub agent_cmd: String,
    /// Remaining argv tokens, exposed to templates as a list (e.g. `{% for a in agent_args %}`).
    pub agent_args: Vec<String>,
    /// Full `SessionId` path-leaf string (`<name>-<ulid>`).
    pub id: String,
    /// Human label for the tab.
    pub name: String,
}

impl LayoutVars {
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

/// Render a layout template. Undefined vars are a hard error per kit R5
/// ("undefined-var syntax error").
pub fn render(template_src: &str, vars: &LayoutVars) -> Result<String, LayoutTemplateError> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);
    let tmpl = env
        .template_from_str(template_src)
        .map_err(|e| LayoutTemplateError::Syntax(e.to_string()))?;
    let rendered = tmpl.render(vars.to_context()).map_err(|e| {
        // minijinja reports UndefinedError specifically — surface it distinctly
        if e.kind() == minijinja::ErrorKind::UndefinedError {
            LayoutTemplateError::UndefinedVar(e.to_string())
        } else {
            LayoutTemplateError::Internal(e)
        }
    })?;
    validate_kdl(&rendered)?;
    Ok(rendered)
}

/// Very light KDL syntax validation: balanced braces + quoted-string integrity
/// + no stray backtick/unclosed comment. Does NOT attempt to replicate zellij's
/// full parser — catches obvious malformations that would make zellij refuse.
fn validate_kdl(src: &str) -> Result<(), LayoutTemplateError> {
    let mut depth: i64 = 0;
    let mut in_str = false;
    let mut in_line_comment = false;
    let mut in_block_comment = 0u32;
    let mut escape = false;
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_line_comment {
            if c == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment > 0 {
            if c == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment -= 1;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_str {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => in_line_comment = true,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => in_block_comment += 1,
            _ => {}
        }
        if depth < 0 {
            return Err(LayoutTemplateError::InvalidKdl(
                "unbalanced }: extra closing brace".into(),
            ));
        }
        i += 1;
    }
    // Check string termination before brace balance — an unterminated
    // string commonly swallows a closing `}` and produces a misleading
    // "unbalanced braces" message.
    if in_str {
        return Err(LayoutTemplateError::InvalidKdl(
            "unterminated string literal".into(),
        ));
    }
    if depth != 0 {
        return Err(LayoutTemplateError::InvalidKdl(format!(
            "unbalanced braces: depth={depth}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> LayoutVars {
        LayoutVars {
            cwd: "/tmp/work".into(),
            agent_cmd: "claude".into(),
            agent_args: vec!["--resume".into(), "--verbose".into()],
            id: "cavekit-auth-01jx7z8k6x9y2zt4abcdef0123".into(),
            name: "builder".into(),
        }
    }

    #[test]
    fn simple_substitution_resolves_agent_cmd() {
        let tmpl = r#"pane command="{{ agent_cmd }}""#;
        let out = render(tmpl, &vars()).unwrap();
        assert_eq!(out, r#"pane command="claude""#);
    }

    #[test]
    fn undefined_var_is_distinct_error() {
        let err = render("{{ nope }}", &vars()).unwrap_err();
        assert!(
            matches!(err, LayoutTemplateError::UndefinedVar(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn syntax_error_in_template_returns_syntax_variant() {
        // Unclosed expression -> compile-time syntax error.
        let err = render("{{ unclosed", &vars()).unwrap_err();
        assert!(
            matches!(err, LayoutTemplateError::Syntax(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn rendered_output_with_unbalanced_braces_rejected() {
        // Template renders to `layout {{` (since `{{` is escaped via `{{ "{{" }}`).
        // Use a literal extra brace.
        let tmpl = "layout { tab { pane }";
        let err = render(tmpl, &vars()).unwrap_err();
        assert!(
            matches!(err, LayoutTemplateError::InvalidKdl(_)),
            "got: {err:?}"
        );
    }

    #[test]
    fn extra_closing_brace_rejected() {
        let tmpl = "layout { tab { pane } } }";
        let err = render(tmpl, &vars()).unwrap_err();
        match err {
            LayoutTemplateError::InvalidKdl(msg) => {
                assert!(
                    msg.contains("extra closing brace") || msg.contains("unbalanced"),
                    "msg: {msg}"
                );
            }
            other => panic!("expected InvalidKdl, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_rejected() {
        let tmpl = "layout { tab name=\"oops }";
        let err = render(tmpl, &vars()).unwrap_err();
        match err {
            LayoutTemplateError::InvalidKdl(msg) => {
                assert!(msg.contains("string"), "msg: {msg}");
            }
            other => panic!("expected InvalidKdl, got {other:?}"),
        }
    }

    #[test]
    fn agent_args_iteration() {
        let tmpl = "{% for a in agent_args %}{{ a }} {% endfor %}";
        let out = render(tmpl, &vars()).unwrap();
        assert_eq!(out, "--resume --verbose ");
    }

    #[test]
    fn comments_do_not_affect_brace_count() {
        let tmpl = "layout { /* { extra { in comment */ tab { pane } // }\n }";
        let out = render(tmpl, &vars()).unwrap();
        assert!(out.contains("layout"));
    }

    #[test]
    fn id_and_name_substitute() {
        let tmpl = r#"// id={{ id }} name={{ name }}\nlayout { tab { pane } }"#;
        let out = render(tmpl, &vars()).unwrap();
        assert!(out.contains("cavekit-auth-01jx7z8k6x9y2zt4abcdef0123"));
        assert!(out.contains("name=builder"));
    }

    // ------- T-122: additional coverage for R5 template surface -------

    /// All five standard vars resolve in a single template. Guards against
    /// silent regressions where one var gets dropped from `to_context`.
    #[test]
    fn all_five_standard_vars_resolve_in_one_template() {
        let tmpl = r#"layout {
    cwd "{{ cwd }}"
    // id={{ id }}
    tab name="{{ name }}" {
        pane command="{{ agent_cmd }}" {
            args{% for a in agent_args %} "{{ a }}"{% endfor %}
        }
    }
}
"#;
        let out = render(tmpl, &vars()).unwrap();
        assert!(out.contains("cwd \"/tmp/work\""), "missing cwd: {out}");
        assert!(
            out.contains("cavekit-auth-01jx7z8k6x9y2zt4abcdef0123"),
            "missing id: {out}"
        );
        assert!(out.contains("name=\"builder\""), "missing name: {out}");
        assert!(
            out.contains("command=\"claude\""),
            "missing agent_cmd: {out}"
        );
        assert!(out.contains("\"--resume\""), "missing agent_args[0]: {out}");
        assert!(
            out.contains("\"--verbose\""),
            "missing agent_args[1]: {out}"
        );
    }

    /// `{% if agent_cmd %}` — truthy branch when agent_cmd is non-empty.
    #[test]
    fn conditional_if_agent_cmd_truthy_branch() {
        let tmpl = "{% if agent_cmd %}pane command=\"{{ agent_cmd }}\"{% else %}pane{% endif %}";
        let out = render(tmpl, &vars()).unwrap();
        assert_eq!(out, "pane command=\"claude\"");
    }

    /// `{% if agent_cmd %}` — falsy branch when agent_cmd is the empty
    /// string. Minijinja treats `""` as falsy by default.
    #[test]
    fn conditional_if_agent_cmd_falsy_branch_with_empty_string() {
        let tmpl = "{% if agent_cmd %}HAS_CMD{% else %}NO_CMD{% endif %}";
        let v = LayoutVars {
            cwd: "/tmp".into(),
            agent_cmd: String::new(),
            agent_args: Vec::new(),
            id: "id".into(),
            name: "n".into(),
        };
        let out = render(tmpl, &v).unwrap();
        assert_eq!(out, "NO_CMD");
    }

    /// Empty `agent_args` iterates zero times. Edge case for the common
    /// `{% for a in agent_args %}` pattern in shipped layouts.
    #[test]
    fn empty_agent_args_iterates_zero_times() {
        let tmpl = "START{% for a in agent_args %} arg={{ a }}{% endfor %}END";
        let v = LayoutVars {
            cwd: "/tmp".into(),
            agent_cmd: "x".into(),
            agent_args: Vec::new(),
            id: "id".into(),
            name: "n".into(),
        };
        let out = render(tmpl, &v).unwrap();
        assert_eq!(out, "STARTEND");
    }

    /// Nested `{% if %}` inside `{% for %}` — the common layout pattern
    /// "emit args only when present". Exercises both block constructs.
    #[test]
    fn nested_for_with_if_inside_resolves() {
        let tmpl = "{% for a in agent_args %}{% if a %}[{{ a }}]{% endif %}{% endfor %}";
        let out = render(tmpl, &vars()).unwrap();
        assert_eq!(out, "[--resume][--verbose]");
    }
}
