//! Hook entries for `[[hooks]]` table — cavekit-config.md R4.
//!
//! A hook ties a shell command to one or more `AgentEvent` matches.  At fire
//! time the supervisor builds a [`HookContext`] from the event (event kind,
//! orchestrator, severity, plus a free-form `vars` map for `{{name}}`-style
//! template expansion) and asks every hook whether it should run via
//! [`HookEntry::matches`].
//!
//! ## Two execution forms — pick the safe one
//!
//! A hook can specify either:
//!
//! - `cmd_argv = ["prog", "arg1", "{{var}}", ...]` — **preferred**. Executed
//!   via direct `exec` (no shell). `{{var}}` substitution happens per-argv,
//!   so shell metacharacters in `ctx.vars` are passed as plain-text arg
//!   bytes — injection is impossible by construction (F-058 fix).
//! - `cmd = "prog --flag {{var}}"` — legacy / shell-required form. Executed
//!   via `sh -c`. Every interpolated `{{var}}` value is shell-escaped via
//!   [`shlex::try_quote`] before substitution so a crafted filename like
//!   `$(rm -rf /tmp/evil)` cannot escape into a separate command (F-058
//!   hardening). The template literal syntax is preserved verbatim; only
//!   the untrusted interpolated values are quoted.
//!
//! If both fields are set, `cmd_argv` wins. The dispatcher in
//! `crates/core/src/consumers/hook_dispatcher.rs` reads the `render_form`
//! helper on [`HookEntry`] to decide which branch to take.
//!
//! ```rust
//! use ark_config::hooks::{HookContext, HookEntry};
//! use std::collections::BTreeMap;
//!
//! let hook = HookEntry {
//!     cmd: "notify-send 'ark: {{name}} done'".into(),
//!     cmd_argv: Vec::new(),
//!     on_event: vec!["done".into()],
//!     on_orchestrator: vec![],
//!     on_severity: vec![],
//! };
//!
//! let mut vars = BTreeMap::new();
//! vars.insert("name".into(), "scout".into());
//!
//! let ctx = HookContext {
//!     event_kind: "done".into(),
//!     orchestrator: "cavekit".into(),
//!     severity: None,
//!     vars,
//! };
//!
//! assert!(hook.matches(&ctx));
//! // "scout" contains no shell metacharacters, so shlex quotes it as-is.
//! assert_eq!(hook.render(&ctx), "notify-send 'ark: scout done'");
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One `[[hooks]]` entry.
///
/// Match semantics: empty filter list = match anything.  Non-empty filters are
/// OR'd within a list (`on_event = ["done", "fail"]` matches either) and AND'd
/// across lists (`on_event = ["done"]` AND `on_severity = ["P0"]`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookEntry {
    /// Legacy shell-string command form. Passed to `sh -c` by the
    /// dispatcher, with every interpolated `{{var}}` value shell-escaped
    /// via `shlex::try_quote` (F-058). Prefer [`Self::cmd_argv`] for
    /// new hooks — it bypasses the shell entirely and is injection-safe
    /// by construction.
    #[serde(default)]
    pub cmd: String,

    /// Preferred argv-array command form. Executed via direct
    /// `exec()` — no shell. `{{var}}` substitution happens per-argv, so
    /// shell metacharacters inside `ctx.vars` pass through as plain
    /// argument bytes; command injection is impossible by construction
    /// (F-058 safe path). When both `cmd_argv` and `cmd` are populated,
    /// `cmd_argv` wins.
    #[serde(default)]
    pub cmd_argv: Vec<String>,

    /// Event-kind filter, e.g. `["done", "finding"]`.  Empty = match any.
    #[serde(default)]
    pub on_event: Vec<String>,

    /// Orchestrator-slug filter, e.g. `["cavekit"]`.  Empty = match any.
    #[serde(default)]
    pub on_orchestrator: Vec<String>,

    /// Severity filter, e.g. `["P0", "P1"]`.  Empty = match any (including
    /// events that have no severity).
    #[serde(default)]
    pub on_severity: Vec<String>,
}

/// Which execution form the dispatcher should use for a [`HookEntry`].
///
/// Picked by [`HookEntry::render_form`] — `cmd_argv` wins when set,
/// otherwise the legacy string form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderedCommand {
    /// Direct-exec argv with `{{var}}` substituted per-element. No shell
    /// involved: safe for any `ctx.vars` value, including ones containing
    /// shell metacharacters.
    Argv(Vec<String>),
    /// Legacy shell form: pass to `sh -c`. Every interpolated value is
    /// already shell-escaped via `shlex::try_quote` in the returned
    /// string.
    Shell(String),
}

/// Context passed to [`HookEntry::matches`] / [`HookEntry::render`].
///
/// Constructed by the supervisor from the in-flight `AgentEvent` plus any
/// supervisor-supplied vars (typically `id`, `name`, `outcome`, `tool`, ...).
#[derive(Clone, Debug, Default)]
pub struct HookContext {
    /// Event kind slug, e.g. `"done"`, `"stall"`, `"finding"`.
    pub event_kind: String,
    /// Originating orchestrator slug, e.g. `"cavekit"`.
    pub orchestrator: String,
    /// Severity tag, when the event carries one (e.g. findings).
    pub severity: Option<String>,
    /// Free-form variables substituted into the rendered command.
    pub vars: BTreeMap<String, String>,
}

impl HookEntry {
    /// True when this hook should fire for the given event context.
    ///
    /// - Empty filter list ⇒ match anything.
    /// - `on_event` / `on_orchestrator`: contains check against the matching
    ///   field (case-sensitive).
    /// - `on_severity`: contains check against `ctx.severity`. If the filter
    ///   is non-empty and `ctx.severity` is `None`, the hook does **not**
    ///   match — severity-scoped hooks only fire on severity-bearing events.
    pub fn matches(&self, ctx: &HookContext) -> bool {
        if !self.on_event.is_empty() && !self.on_event.iter().any(|e| e == &ctx.event_kind) {
            return false;
        }
        if !self.on_orchestrator.is_empty()
            && !self.on_orchestrator.iter().any(|o| o == &ctx.orchestrator)
        {
            return false;
        }
        if !self.on_severity.is_empty() {
            match ctx.severity.as_deref() {
                None => return false,
                Some(sev) => {
                    if !self.on_severity.iter().any(|s| s == sev) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Render `cmd` by substituting `{{var}}` placeholders from `ctx.vars`.
    ///
    /// **F-058 hardening**: every interpolated value is shell-escaped via
    /// [`shlex::try_quote`] before substitution. A `{{path}}` whose value
    /// is `foo; rm -rf /tmp/evil` renders to the literal quoted token
    /// `'foo; rm -rf /tmp/evil'` — the shell treats it as one argument,
    /// not a command separator. Literal shell syntax in `cmd` (pipes,
    /// redirects, subshells written by the author) is preserved.
    ///
    /// Unknown vars are left as the literal `{{name}}` form — the
    /// supervisor logs unknown keys but does not fail the hook (kit R4 calls
    /// for non-fatal failures).  Lookup keys may contain `[A-Za-z0-9_]` and
    /// must be at least one character.  Anything else is left as-is.
    pub fn render(&self, ctx: &HookContext) -> String {
        render_template_shell_escaped(&self.cmd, &ctx.vars)
    }

    /// Pick the execution form the dispatcher should use.
    ///
    /// - Non-empty [`Self::cmd_argv`] → [`RenderedCommand::Argv`] with
    ///   per-element `{{var}}` substitution (no escaping needed — exec()
    ///   does not interpret argv bytes).
    /// - Otherwise → [`RenderedCommand::Shell`] with shell-escaped
    ///   interpolation into the legacy [`Self::cmd`] string.
    pub fn render_form(&self, ctx: &HookContext) -> RenderedCommand {
        if !self.cmd_argv.is_empty() {
            RenderedCommand::Argv(
                self.cmd_argv
                    .iter()
                    .map(|a| render_template_raw(a, &ctx.vars))
                    .collect(),
            )
        } else {
            RenderedCommand::Shell(render_template_shell_escaped(&self.cmd, &ctx.vars))
        }
    }

    /// True iff this entry has any `{{ident}}`-style placeholder in its
    /// [`Self::cmd`] string. Used by the dispatcher to decide when to
    /// emit the "shell form with interpolated values, prefer cmd_argv"
    /// warning (F-058).
    pub fn shell_cmd_has_template(&self) -> bool {
        template_has_placeholder(&self.cmd)
    }
}

/// Substitute `{{ident}}` placeholders inside `template` from `vars`,
/// applying `transform` to every substituted value before it is pushed
/// to the output.
///
/// - `ident` is `[A-Za-z0-9_]+` — anything else is left literal.
/// - Unknown keys leave the original `{{ident}}` form intact.
/// - Single `{` characters and unmatched `{{` are passed through verbatim.
fn render_template_with<F>(
    template: &str,
    vars: &BTreeMap<String, String>,
    mut transform: F,
) -> String
where
    F: FnMut(&str) -> String,
{
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < bytes.len() {
        // Look for `{{`
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Scan until matching `}}`
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'}' && bytes[end + 1] == b'}') {
                end += 1;
            }
            if end + 1 < bytes.len() && bytes[end] == b'}' && bytes[end + 1] == b'}' {
                let ident = &template[start..end];
                if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    if let Some(val) = vars.get(ident) {
                        out.push_str(&transform(val));
                    } else {
                        // Unknown var — leave literal.
                        out.push_str("{{");
                        out.push_str(ident);
                        out.push_str("}}");
                    }
                    i = end + 2;
                    continue;
                }
                // Non-ident inside braces — pass through verbatim.
                out.push_str(&template[i..end + 2]);
                i = end + 2;
                continue;
            }
            // No closing `}}` — pass through rest.
            out.push_str(&template[i..]);
            return out;
        }
        // ordinary byte
        let ch_end = next_char_boundary(template, i);
        out.push_str(&template[i..ch_end]);
        i = ch_end;
    }
    out
}

/// Raw (un-escaped) template rendering. Used by [`HookEntry::render_form`]
/// for the `Argv` branch — direct exec doesn't interpret arg bytes, so
/// no escaping is needed.
fn render_template_raw(template: &str, vars: &BTreeMap<String, String>) -> String {
    render_template_with(template, vars, |v| v.to_string())
}

/// Shell-escaped rendering (F-058). Every interpolated value is wrapped
/// via [`shlex::try_quote`] so shell metacharacters in `ctx.vars` become
/// literal argument bytes when the resulting string is passed to `sh -c`.
///
/// `shlex::try_quote` can fail if the value contains a NUL byte; in that
/// case we substitute the single-quoted empty string `''` and log a
/// tracing warning. `AgentEvent` paths and tool names don't contain NUL
/// in practice, but the fail-safe keeps the fallback contained.
fn render_template_shell_escaped(template: &str, vars: &BTreeMap<String, String>) -> String {
    render_template_with(template, vars, |v| match shlex::try_quote(v) {
        Ok(quoted) => quoted.into_owned(),
        Err(_) => {
            // NUL byte in the value — shlex cannot express it in shell
            // syntax. Substitute an empty quoted token so the rendered
            // command still parses as a valid shell literal.
            "''".to_string()
        }
    })
}

/// True iff `template` contains at least one `{{ident}}` placeholder
/// (where `ident` matches `[A-Za-z0-9_]+`).
fn template_has_placeholder(template: &str) -> bool {
    let bytes = template.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'}' && bytes[end + 1] == b'}') {
                end += 1;
            }
            if end + 1 < bytes.len() && bytes[end] == b'}' && bytes[end + 1] == b'}' {
                let ident = &template[start..end];
                if !ident.is_empty() && ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return true;
                }
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    false
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(event_kind: &str, orch: &str, sev: Option<&str>) -> HookContext {
        HookContext {
            event_kind: event_kind.into(),
            orchestrator: orch.into(),
            severity: sev.map(String::from),
            vars: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_filters_match_anything() {
        let h = HookEntry {
            cmd: "true".into(),
            ..Default::default()
        };
        assert!(h.matches(&ctx("done", "cavekit", None)));
        assert!(h.matches(&ctx("anything", "claude-code", Some("P3"))));
    }

    #[test]
    fn event_filter_or_semantics() {
        let h = HookEntry {
            cmd: "true".into(),
            on_event: vec!["done".into(), "fail".into()],
            ..Default::default()
        };
        assert!(h.matches(&ctx("done", "cavekit", None)));
        assert!(h.matches(&ctx("fail", "cavekit", None)));
        assert!(!h.matches(&ctx("stall", "cavekit", None)));
    }

    #[test]
    fn severity_filter_rejects_mismatch() {
        let h = HookEntry {
            cmd: "true".into(),
            on_severity: vec!["P0".into()],
            ..Default::default()
        };
        assert!(h.matches(&ctx("finding", "cavekit", Some("P0"))));
        assert!(!h.matches(&ctx("finding", "cavekit", Some("P1"))));
    }

    #[test]
    fn severity_filter_rejects_none_when_required() {
        let h = HookEntry {
            cmd: "true".into(),
            on_severity: vec!["P0".into()],
            ..Default::default()
        };
        assert!(!h.matches(&ctx("done", "cavekit", None)));
    }

    #[test]
    fn orchestrator_and_event_anded() {
        let h = HookEntry {
            cmd: "true".into(),
            on_event: vec!["done".into()],
            on_orchestrator: vec!["cavekit".into()],
            ..Default::default()
        };
        assert!(h.matches(&ctx("done", "cavekit", None)));
        assert!(!h.matches(&ctx("done", "claude-code", None)));
        assert!(!h.matches(&ctx("fail", "cavekit", None)));
    }

    #[test]
    fn render_substitutes_known_vars() {
        let mut vars = BTreeMap::new();
        vars.insert("id".into(), "agt-1".into());
        vars.insert("name".into(), "scout".into());
        let h = HookEntry {
            cmd: "echo {{id}} {{name}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo agt-1 scout");
    }

    #[test]
    fn render_leaves_unknown_vars_literal() {
        let h = HookEntry {
            cmd: "echo {{unknown}} done".into(),
            ..Default::default()
        };
        let c = ctx("done", "cavekit", None);
        assert_eq!(h.render(&c), "echo {{unknown}} done");
    }

    #[test]
    fn render_passes_through_single_braces() {
        let h = HookEntry {
            cmd: "echo { not-a-var }".into(),
            ..Default::default()
        };
        let c = ctx("done", "cavekit", None);
        assert_eq!(h.render(&c), "echo { not-a-var }");
    }

    #[test]
    fn render_handles_unmatched_double_brace() {
        let h = HookEntry {
            cmd: "echo {{noclose".into(),
            ..Default::default()
        };
        let c = ctx("done", "cavekit", None);
        // Unmatched -> passed through verbatim.
        assert_eq!(h.render(&c), "echo {{noclose");
    }

    #[test]
    fn deny_unknown_fields_on_hook_entry() {
        use figment::{
            Figment,
            providers::{Format, Toml},
        };
        let toml = r#"
            cmd = "true"
            bogus = "xx"
        "#;
        let res: Result<HookEntry, _> = Figment::new().merge(Toml::string(toml)).extract();
        assert!(res.is_err(), "unknown key should be rejected");
    }

    #[test]
    fn parses_minimal_hook_entry() {
        use figment::{
            Figment,
            providers::{Format, Toml},
        };
        let toml = r#"
            cmd = "notify-send hi"
        "#;
        let h: HookEntry = Figment::new().merge(Toml::string(toml)).extract().unwrap();
        assert_eq!(h.cmd, "notify-send hi");
        assert!(h.on_event.is_empty());
        assert!(h.on_orchestrator.is_empty());
        assert!(h.on_severity.is_empty());
    }

    // -----------------------------------------------------------------
    // F-058: shell-injection hardening.
    // - Interpolated values are shell-escaped via shlex::try_quote.
    // - `cmd_argv` form bypasses the shell entirely.
    // -----------------------------------------------------------------

    #[test]
    fn f058_render_escapes_semicolon_rm_injection() {
        let mut vars = BTreeMap::new();
        vars.insert("path".into(), "a; rm -rf /tmp/evil".into());
        let h = HookEntry {
            cmd: "touch {{path}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        // shlex single-quotes the whole value when it contains shell
        // metacharacters. The `; rm` fragment is inside the quotes — so
        // sh -c treats the entire thing as one argument to `touch`.
        assert_eq!(h.render(&c), "touch 'a; rm -rf /tmp/evil'");
    }

    #[test]
    fn f058_render_escapes_command_substitution() {
        let mut vars = BTreeMap::new();
        vars.insert("tool".into(), "$(whoami)".into());
        let h = HookEntry {
            cmd: "echo {{tool}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        // `$(...)` inside single quotes is NOT expanded by sh.
        assert_eq!(h.render(&c), "echo '$(whoami)'");
    }

    #[test]
    fn f058_render_escapes_backticks() {
        let mut vars = BTreeMap::new();
        vars.insert("x".into(), "`id`".into());
        let h = HookEntry {
            cmd: "echo {{x}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo '`id`'");
    }

    #[test]
    fn f058_render_escapes_and_chain() {
        let mut vars = BTreeMap::new();
        vars.insert("x".into(), "a && evil".into());
        let h = HookEntry {
            cmd: "echo {{x}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo 'a && evil'");
    }

    #[test]
    fn f058_render_leaves_safe_alphanumeric_unquoted() {
        let mut vars = BTreeMap::new();
        vars.insert("tool".into(), "Read".into());
        let h = HookEntry {
            cmd: "echo {{tool}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo Read");
    }

    #[test]
    fn f058_render_literal_shell_syntax_preserved() {
        // Author's legitimate shell syntax (redirects, pipes) is NOT
        // escaped — only interpolated `{{var}}` values are.
        let mut vars = BTreeMap::new();
        vars.insert("name".into(), "scout".into());
        let h = HookEntry {
            cmd: "echo {{name}} | tee -a /tmp/log > /dev/null".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo scout | tee -a /tmp/log > /dev/null");
    }

    #[test]
    fn f058_render_form_argv_substitutes_raw_values() {
        let mut vars = BTreeMap::new();
        vars.insert("path".into(), "a; rm b".into());
        let h = HookEntry {
            cmd: String::new(),
            cmd_argv: vec!["/bin/touch".into(), "{{path}}".into()],
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        match h.render_form(&c) {
            RenderedCommand::Argv(argv) => {
                assert_eq!(argv, vec!["/bin/touch".to_string(), "a; rm b".to_string()]);
            }
            RenderedCommand::Shell(s) => panic!("expected Argv, got Shell({s:?})"),
        }
    }

    #[test]
    fn f058_render_form_prefers_argv_when_both_set() {
        let h = HookEntry {
            cmd: "echo fallback".into(),
            cmd_argv: vec!["/bin/echo".into(), "chosen".into()],
            ..Default::default()
        };
        let c = ctx("done", "cavekit", None);
        match h.render_form(&c) {
            RenderedCommand::Argv(argv) => {
                assert_eq!(argv, vec!["/bin/echo".to_string(), "chosen".to_string()]);
            }
            RenderedCommand::Shell(_) => panic!("cmd_argv should win when both are set"),
        }
    }

    #[test]
    fn f058_render_form_shell_when_only_cmd_set() {
        let h = HookEntry {
            cmd: "echo hi".into(),
            ..Default::default()
        };
        let c = ctx("done", "cavekit", None);
        match h.render_form(&c) {
            RenderedCommand::Shell(s) => assert_eq!(s, "echo hi"),
            RenderedCommand::Argv(a) => panic!("expected Shell, got Argv({a:?})"),
        }
    }

    #[test]
    fn f058_shell_cmd_has_template_detects_placeholders() {
        let h = HookEntry {
            cmd: "echo {{name}}".into(),
            ..Default::default()
        };
        assert!(h.shell_cmd_has_template());
        let h2 = HookEntry {
            cmd: "echo hi".into(),
            ..Default::default()
        };
        assert!(!h2.shell_cmd_has_template());
        let h3 = HookEntry {
            cmd: "echo { not-a-var }".into(),
            ..Default::default()
        };
        assert!(!h3.shell_cmd_has_template());
    }

    #[test]
    fn f058_parses_cmd_argv_from_toml() {
        use figment::{
            Figment,
            providers::{Format, Toml},
        };
        let toml = r#"
            cmd_argv = ["notify-send", "ark: {{name}} done"]
        "#;
        let h: HookEntry = Figment::new().merge(Toml::string(toml)).extract().unwrap();
        assert!(h.cmd.is_empty());
        assert_eq!(
            h.cmd_argv,
            vec!["notify-send".to_string(), "ark: {{name}} done".to_string()]
        );
    }

    #[test]
    fn f058_render_escapes_nul_byte_fallback() {
        // NUL can't be represented in shell syntax. Our helper falls
        // back to `''` (empty quoted token) rather than panicking.
        let mut vars = BTreeMap::new();
        vars.insert("x".into(), "a\0b".into());
        let h = HookEntry {
            cmd: "echo {{x}}".into(),
            ..Default::default()
        };
        let c = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(h.render(&c), "echo ''");
    }
}
