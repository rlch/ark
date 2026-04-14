//! Hook entries for `[[hooks]]` table — cavekit-config.md R4.
//!
//! A hook ties a shell command to one or more `AgentEvent` matches.  At fire
//! time the supervisor builds a [`HookContext`] from the event (event kind,
//! orchestrator, severity, plus a free-form `vars` map for `{{name}}`-style
//! template expansion) and asks every hook whether it should run via
//! [`HookEntry::matches`].
//!
//! `cmd` is a shell-style string in v1 (matches the kit's `cmd = "..."` form
//! once rendered); the kit's argv-array form is accepted via `cmd_argv` for
//! forward-compatibility but not yet executed by the supervisor.
//!
//! ```rust
//! use ark_config::hooks::{HookContext, HookEntry};
//! use std::collections::BTreeMap;
//!
//! let hook = HookEntry {
//!     cmd: "notify-send 'ark: {{name}} done'".into(),
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
    /// Shell command to run.  Supports `{{var}}` substitution from the
    /// event context — see [`HookEntry::render`].
    pub cmd: String,

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
    /// Unknown vars are left as the literal `{{name}}` form for v1 — the
    /// supervisor logs unknown keys but does not fail the hook (kit R4 calls
    /// for non-fatal failures).  Lookup keys may contain `[A-Za-z0-9_]` and
    /// must be at least one character.  Anything else is left as-is.
    pub fn render(&self, ctx: &HookContext) -> String {
        render_template(&self.cmd, &ctx.vars)
    }
}

/// Substitute `{{ident}}` placeholders inside `template` from `vars`.
///
/// - `ident` is `[A-Za-z0-9_]+` — anything else is left literal.
/// - Unknown keys leave the original `{{ident}}` form intact.
/// - Single `{` characters and unmatched `{{` are passed through verbatim.
fn render_template(template: &str, vars: &BTreeMap<String, String>) -> String {
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
                        out.push_str(val);
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
}
