//! Legacy `[[hooks]]` TOML → synthetic scene fragment compat layer (T-5.7).
//!
//! Pre-T-5 ark shipped hook execution as a dedicated `hook_dispatcher`
//! consumer task that subscribed to the supervisor broadcast bus and
//! ran every matching `[[hooks]]` entry's command in a child process.
//! T-5 introduces scene reactions as the unified runtime for "do
//! something when an event fires". To keep zero-migration for existing
//! users, this module compiles each `[[hooks]]` TOML entry into the
//! equivalent scene reaction so the new dispatcher can run it through
//! the same pipeline as user-authored reactions.
//!
//! ## Mapping table
//!
//! | TOML field | Scene fragment translation |
//! |---|---|
//! | `cmd = "notify-send hi"` | `exec script="notify-send hi"` |
//! | `cmd_argv = ["echo", "hi"]` | `exec script="echo hi"` (shell-quoted via [`shlex::try_quote`]) |
//! | `on_event = ["started"]` | one `on "Started" { exec ... }` block |
//! | `on_event = ["done", "fail"]` | two `on { }` blocks (one per slug) |
//! | `on_event` empty | one `on "*"` block per other event slug ark fires (concretely: one block per filter combination, with no event-kind filter — emitted as bare `on "*"` which selector grammar rejects today, so we conservatively emit one block per known [`crate::reactions::EventKind::ALL`] slug) |
//! | `on_orchestrator = ["cavekit"]` | `if="agent.orchestrator == 'cavekit'"` (OR'd across multiple) |
//! | `on_severity = ["P0"]` | `if="event.severity == 'P0'"` (OR'd; AND'd with orchestrator filter) |
//!
//! The `cmd` (shell form) and `cmd_argv` (direct exec form) distinction
//! collapses here: scene's `exec` op always runs `sh -c <script>`, so
//! the argv form is shell-quoted via [`shlex::try_quote`] before being
//! joined into a single `script=` string. F-058 hardening (variable
//! shell-escape) does NOT carry over to this compat path because the
//! synthesised reactions don't perform `{{var}}` substitution — at
//! T-5.7's wiring point the hook-compat reactions emit their literal
//! command verbatim. A follow-up (post-T-5.7) can wire scene's runtime
//! template renderer (R9) into the same args dict the hook dispatcher
//! built so `{{name}}`, `{{outcome}}`, etc. work end-to-end through the
//! reaction path.
//!
//! ## Origin attribution
//!
//! Every reaction synthesised here is tagged
//! [`ReactionOrigin::HookConfig`] so the T-5.6 telemetry surface
//! (`scene::reactions` target) renders `reaction_origin="HookConfig"`,
//! letting operators distinguish hook-derived fires from user-scene
//! reactions in a single grep.
//!
//! ## Why this doesn't depend on `ark-config`
//!
//! `ark-scene` is a leaf in the workspace dep graph (mux, core,
//! supervisor depend on it — not the other way round). Pulling
//! `ark-config` in just to read `HookEntry` would either (a) add a
//! hop the graph doesn't currently need, or (b) introduce a cycle if
//! `ark-config` ever grows a scene-shaped field. We mirror the minimal
//! shape we care about as [`HookEntry`] inside this module; the
//! supervisor (which already depends on both crates) maps from
//! `ark_config::HookEntry` to this mirror at boot time.

use kdl::KdlNode;

use crate::intent::ReactionOrigin;
use crate::ops::Idempotency;
use crate::ops::dispatch::CompiledOp;
use crate::reactions::{EventKind, ReactionEntry, ReactionRegistry};

// ---------------------------------------------------------------------------
// HookEntry mirror
// ---------------------------------------------------------------------------

/// Mirror of `ark_config::HookEntry` — the minimum shape this module
/// needs to render a synthetic scene fragment. Lives here (rather
/// than reaching into `ark-config`) to keep `ark-scene`'s dep graph
/// leaf-shaped; see the module docs for the rationale.
///
/// Field semantics match `ark_config::hooks::HookEntry` exactly. The
/// supervisor maps from the canonical config struct to this mirror at
/// boot time via [`HookEntry::from_argv`] / [`HookEntry::from_cmd`] /
/// [`HookEntry::new`].
#[derive(Debug, Clone, Default)]
pub struct HookEntry {
    /// Legacy shell-string command form (passed to `sh -c`).
    pub cmd: String,
    /// Preferred argv-array form (joined + shell-quoted into `script=`).
    pub cmd_argv: Vec<String>,
    /// Event-kind slug filter, e.g. `["done", "fail"]`. Empty = match
    /// every kind ark might emit.
    pub on_event: Vec<String>,
    /// Orchestrator-slug filter. Empty = match any.
    pub on_orchestrator: Vec<String>,
    /// Severity slug filter, e.g. `["P0"]`. Empty = match any.
    pub on_severity: Vec<String>,
}

impl HookEntry {
    /// Build an entry from explicit fields (mostly for tests).
    pub fn new(
        cmd: impl Into<String>,
        cmd_argv: Vec<String>,
        on_event: Vec<String>,
        on_orchestrator: Vec<String>,
        on_severity: Vec<String>,
    ) -> Self {
        Self {
            cmd: cmd.into(),
            cmd_argv,
            on_event,
            on_orchestrator,
            on_severity,
        }
    }
}

// ---------------------------------------------------------------------------
// Fragment rendering
// ---------------------------------------------------------------------------

/// Compile a slice of legacy `[[hooks]]` entries into a synthetic
/// scene KDL fragment that, when parsed, registers one `on { }`
/// reaction per (hook, event-kind) pair.
///
/// The returned string is a valid KDL **document body** — it can be
/// concatenated inside a `scene "<name>" { … }` wrapper and re-parsed
/// via [`crate::parse::parse_scene`]. Empty input returns an empty
/// string (no trailing newline) so callers can unconditionally
/// concatenate without producing stray whitespace.
///
/// See the module docs for the full mapping table.
pub fn hooks_to_scene_fragment(hooks: &[HookEntry]) -> String {
    if hooks.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (idx, hook) in hooks.iter().enumerate() {
        let script = render_script(hook);
        let predicate = render_predicate(hook);
        let kinds: Vec<EventKind> = effective_event_kinds(hook);
        for kind in kinds {
            // `EventKind::as_str` returns the snake_case slug; we render
            // the PascalCase sugar form so the fragment reads naturally
            // for someone reviewing `ark scene graph` output.
            let selector = pascal_case_kind(kind);
            out.push_str(&format!("// hook[{idx}] -> {selector}\n"));
            out.push_str("on \"");
            out.push_str(&selector);
            out.push('"');
            if let Some(pred) = predicate.as_deref() {
                out.push_str(" if=\"");
                out.push_str(&kdl_escape(pred));
                out.push('"');
            }
            out.push_str(" {\n");
            out.push_str("    exec script=\"");
            out.push_str(&kdl_escape(&script));
            out.push_str("\"\n");
            out.push_str("}\n");
        }
    }
    out
}

/// Build a [`ReactionRegistry`] whose entries correspond to the legacy
/// `[[hooks]]` array, with [`ReactionOrigin::HookConfig`] tagged.
///
/// This bypasses [`crate::reactions::populate_registry`] (which walks
/// a parsed [`crate::ast::SceneDoc`]) because the AST's `OpNode`
/// type currently drops op names — see the TODO inside
/// `op_node_to_compiled` in `reactions.rs`. Synthesising the
/// `CompiledOp` directly here keeps the `exec` op's KDL node intact
/// so the dispatcher can round-trip it through facet-kdl into
/// [`crate::ops::control::ExecArgs`] at fire time.
///
/// Callers typically merge this into the user-scene registry by
/// extending its entries; see [`extend_registry_with_hooks`].
pub fn build_hook_registry(hooks: &[HookEntry]) -> ReactionRegistry {
    let mut reg = ReactionRegistry::new();
    extend_registry_with_hooks(&mut reg, hooks);
    reg
}

/// Append every hook-derived reaction to the supplied registry.
///
/// Use this when the supervisor has already populated a
/// [`ReactionRegistry`] from the user's scene and wants to add the
/// legacy hook entries on top. Order: hook reactions are appended
/// after user-scene reactions, mirroring the historical hook fire
/// order (which ran after every other consumer subscribed at boot).
pub fn extend_registry_with_hooks(registry: &mut ReactionRegistry, hooks: &[HookEntry]) {
    for hook in hooks {
        let predicate = render_predicate(hook);
        let kinds = effective_event_kinds(hook);
        for kind in kinds {
            let entry = ReactionEntry {
                selector: pascal_case_kind(kind.clone()),
                predicate: predicate
                    .as_deref()
                    .and_then(|src| build_predicate_program(src)),
                ops: vec![hook_to_compiled_exec(hook)],
                origin: ReactionOrigin::HookConfig,
            };
            registry.insert(kind, None, entry);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render the hook's command into a single `sh -c`-friendly string.
///
/// `cmd_argv` (when present) wins, mirroring `HookEntry::render_form`
/// in `ark-config`. Each argv element is shell-quoted via
/// [`shlex::try_quote`] so metacharacters in argv slots are preserved
/// literally when `sh -c` parses the joined string.
fn render_script(hook: &HookEntry) -> String {
    if !hook.cmd_argv.is_empty() {
        let parts: Vec<String> = hook
            .cmd_argv
            .iter()
            .map(|a| match shlex::try_quote(a) {
                Ok(q) => q.into_owned(),
                Err(_) => "''".to_string(),
            })
            .collect();
        parts.join(" ")
    } else {
        hook.cmd.clone()
    }
}

/// Build the CEL `if=` predicate string from the hook's filters.
///
/// Returns `None` when the hook has no orchestrator / severity
/// filters — the dispatcher then runs the reaction unconditionally.
/// When both filters are populated they're AND'd; multiple values
/// inside a single filter are OR'd.
fn render_predicate(hook: &HookEntry) -> Option<String> {
    let mut clauses: Vec<String> = Vec::new();
    if !hook.on_orchestrator.is_empty() {
        let parts: Vec<String> = hook
            .on_orchestrator
            .iter()
            .map(|o| format!("agent.orchestrator == \"{}\"", cel_escape(o)))
            .collect();
        clauses.push(parts.join(" || "));
    }
    if !hook.on_severity.is_empty() {
        let parts: Vec<String> = hook
            .on_severity
            .iter()
            .map(|s| format!("event.severity == \"{}\"", cel_escape(s)))
            .collect();
        clauses.push(parts.join(" || "));
    }
    if clauses.is_empty() {
        None
    } else if clauses.len() == 1 {
        Some(clauses.into_iter().next().unwrap())
    } else {
        Some(
            clauses
                .into_iter()
                .map(|c| format!("({c})"))
                .collect::<Vec<_>>()
                .join(" && "),
        )
    }
}

/// Resolve the hook's `on_event` filter to the concrete set of
/// [`EventKind`]s the reaction(s) must subscribe to.
///
/// Empty filter → every kind in [`EventKind::ALL`] (same semantics as
/// the legacy `hook_dispatcher`'s "empty filter list = match anything"
/// rule). Non-empty → the parsed kinds; unknown slugs are silently
/// dropped (the same dispatcher would have skipped them at runtime
/// anyway).
fn effective_event_kinds(hook: &HookEntry) -> Vec<EventKind> {
    if hook.on_event.is_empty() {
        return EventKind::ALL.to_vec();
    }
    hook.on_event
        .iter()
        .filter_map(|slug| EventKind::parse(slug))
        .collect()
}

/// Render an [`EventKind`] in the PascalCase sugar form the scene
/// selector grammar accepts (`"Started"`, `"PhaseTransition"`).
fn pascal_case_kind(kind: EventKind) -> String {
    match kind {
        EventKind::Started => "Started".into(),
        EventKind::TabOpened => "TabOpened".into(),
        EventKind::TabClosed => "TabClosed".into(),
        EventKind::Progress => "Progress".into(),
        EventKind::TaskDone => "TaskDone".into(),
        EventKind::Iteration => "Iteration".into(),
        EventKind::PhaseTransition => "PhaseTransition".into(),
        EventKind::ToolUse => "ToolUse".into(),
        EventKind::Message => "Message".into(),
        EventKind::FileEdited => "FileEdited".into(),
        EventKind::ReviewComment => "ReviewComment".into(),
        EventKind::PermissionAsked => "PermissionAsked".into(),
        EventKind::PermissionResolved => "PermissionResolved".into(),
        EventKind::Stall => "Stall".into(),
        EventKind::Log => "Log".into(),
        EventKind::Error => "Error".into(),
        EventKind::Done => "Done".into(),
        EventKind::UserEvent => "UserEvent".into(),
    }
}

/// Build an `exec` `CompiledOp` for the hook by rendering the script
/// into a KDL node we can hand to the dispatcher unchanged.
///
/// The dispatcher (T-4.5) parses this node back into
/// [`crate::ops::control::ExecArgs`] via facet-kdl at fire time, so
/// the shape produced here MUST match that struct's KDL grammar.
fn hook_to_compiled_exec(hook: &HookEntry) -> CompiledOp {
    let script = render_script(hook);
    let src = format!("exec script=\"{}\"", kdl_escape(&script));
    // Parse the single-line `exec` node back into a `KdlNode`. Failure
    // here would mean we built malformed KDL — a programming error
    // rather than user input — so we panic with a descriptive message.
    let doc: ::kdl::KdlDocument = src
        .parse()
        .expect("hook_compat produced invalid KDL for synthetic exec node");
    let node: KdlNode = doc
        .nodes()
        .first()
        .cloned()
        .expect("synthetic exec KDL document had no node");
    CompiledOp::new("ark.core.exec", Idempotency::AlwaysSideEffect, node)
}

/// Best-effort CEL compile of the predicate. Failures degrade to no
/// predicate (the reaction fires unconditionally) and emit a tracing
/// warning so operators can spot the regression in logs.
///
/// The predicate strings produced by [`render_predicate`] are
/// hand-rolled and trivial; failures here would indicate either
/// (a) a regression in the renderer or (b) a future CEL grammar
/// change. Either way, "fire the hook" is a strictly safer fallback
/// than "drop it silently".
fn build_predicate_program(src: &str) -> Option<std::sync::Arc<crate::cel::Program>> {
    match crate::cel::compile(src, "<hook-compat>", 0) {
        Ok(prog) => Some(std::sync::Arc::new(prog)),
        Err(e) => {
            tracing::warn!(
                target = "scene::hook_compat",
                cel_src = %src,
                error = %e,
                "hook_compat: failed to compile synthesised CEL predicate; reaction will fire unconditionally"
            );
            None
        }
    }
}

/// Escape characters that would break out of a KDL double-quoted
/// string literal. KDL 2.0 string escapes are a small superset of
/// JSON's; this covers the four characters that show up in practice
/// (`"`, `\`, newline, tab) without pulling a full KDL string
/// formatter dependency.
fn kdl_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// Escape characters that would break out of a CEL double-quoted
/// string literal. Mirrors [`kdl_escape`] — CEL's lexer accepts the
/// same minimal escape set for our purposes (orchestrator and
/// severity slugs are alphanumeric in practice; this is belt-and-
/// suspenders for malformed config).
fn cel_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn wrap_scene(fragment: &str) -> String {
        format!("scene \"hooks\" {{\n{fragment}}}\n")
    }

    /// The plan's canonical mapping example: a `cmd_argv` form with
    /// two argv slots, single event filter, no other filters.
    #[test]
    fn plan_canonical_mapping_renders_expected_fragment() {
        let hook = HookEntry::new(
            "",
            vec!["echo".into(), "hi".into()],
            vec!["started".into()],
            vec![],
            vec![],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        // Snapshot via insta so the rendered KDL is reviewable on PR.
        insta::assert_snapshot!(frag, @r#"
        // hook[0] -> Started
        on "Started" {
            exec script="echo hi"
        }
        "#);
    }

    /// Snapshot covering every translation lever: cmd-form,
    /// orchestrator filter, severity filter, multi-event fan-out.
    #[test]
    fn full_translation_matrix_snapshot() {
        let hook = HookEntry::new(
            "notify-send done",
            vec![],
            vec!["done".into(), "fail".into()],
            vec!["cavekit".into(), "claude-code".into()],
            vec!["P0".into()],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        insta::assert_snapshot!(frag);
    }

    /// The fragment must round-trip through `parse_scene` so we know
    /// the synthesiser produces valid KDL the rest of the pipeline
    /// accepts.
    #[test]
    fn fragment_parses_via_parse_scene() {
        let hook = HookEntry::new(
            "",
            vec!["echo".into(), "hi".into()],
            vec!["started".into()],
            vec![],
            vec![],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        let scene = wrap_scene(&frag);
        let doc =
            parse_scene(&scene, std::path::Path::new("hooks.kdl")).expect("synthetic fragment parses");
        assert_eq!(doc.scene.name, "hooks");
        assert_eq!(doc.scene.ons.len(), 1);
        assert_eq!(doc.scene.ons[0].selector, "Started");
    }

    /// Empty filter list expands into one `on` block per
    /// [`EventKind::ALL`] slug — same semantics as the legacy
    /// `hook_dispatcher` "match anything" rule.
    #[test]
    fn empty_event_filter_fans_out_to_every_kind() {
        let hook = HookEntry::new("true", vec![], vec![], vec![], vec![]);
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        let scene = wrap_scene(&frag);
        let doc = parse_scene(&scene, std::path::Path::new("hooks.kdl"))
            .expect("multi-kind fragment parses");
        assert_eq!(doc.scene.ons.len(), EventKind::ALL.len());
    }

    /// Multi-event filter renders one `on` block per slug.
    #[test]
    fn multi_event_renders_per_kind_blocks() {
        let hook = HookEntry::new(
            "",
            vec!["echo".into(), "x".into()],
            vec!["done".into(), "fail".into()],
            vec![],
            vec![],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        // "fail" isn't a known EventKind slug; only "done" survives.
        let scene = wrap_scene(&frag);
        let doc = parse_scene(&scene, std::path::Path::new("hooks.kdl"))
            .expect("multi-event fragment parses");
        // Just one valid slug; unknown slug is silently dropped to
        // match the dispatcher's "skip on unknown event_kind" behavior.
        assert_eq!(doc.scene.ons.len(), 1);
        assert_eq!(doc.scene.ons[0].selector, "Done");
    }

    /// Orchestrator + severity filters AND together; multiple values
    /// inside a single filter OR.
    #[test]
    fn predicate_renders_orchestrator_and_severity_filters() {
        let hook = HookEntry::new(
            "true",
            vec![],
            vec!["review_comment".into()],
            vec!["cavekit".into()],
            vec!["P0".into(), "P1".into()],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        let scene = wrap_scene(&frag);
        let doc =
            parse_scene(&scene, std::path::Path::new("hooks.kdl")).expect("predicate fragment parses");
        assert_eq!(doc.scene.ons.len(), 1);
        let pred = doc.scene.ons[0]
            .if_
            .as_ref()
            .expect("predicate populated");
        assert!(pred.contains("agent.orchestrator == \"cavekit\""));
        assert!(pred.contains("event.severity == \"P0\""));
        assert!(pred.contains("event.severity == \"P1\""));
        assert!(pred.contains("&&"));
    }

    /// `cmd_argv` shell-escapes per element so a metacharacter in
    /// argv slot 1 doesn't break out of `sh -c`.
    #[test]
    fn argv_shell_escapes_metacharacters() {
        let hook = HookEntry::new(
            "",
            vec!["touch".into(), "a; rm -rf /tmp/evil".into()],
            vec!["done".into()],
            vec![],
            vec![],
        );
        let frag = hooks_to_scene_fragment(std::slice::from_ref(&hook));
        // The metacharacter-laden arg lives inside single quotes so
        // `sh -c` treats it as one argument to touch.
        assert!(
            frag.contains("touch 'a; rm -rf /tmp/evil'"),
            "expected shlex-quoted argv in fragment, got:\n{frag}"
        );
    }

    /// Empty hooks slice yields an empty string (no trailing newline)
    /// so callers can unconditionally concatenate.
    #[test]
    fn empty_hooks_yield_empty_fragment() {
        assert!(hooks_to_scene_fragment(&[]).is_empty());
    }

    /// `build_hook_registry` produces entries tagged with
    /// `ReactionOrigin::HookConfig` for telemetry attribution.
    #[test]
    fn registry_entries_tagged_with_hook_config_origin() {
        let hook = HookEntry::new(
            "",
            vec!["echo".into(), "hi".into()],
            vec!["started".into()],
            vec![],
            vec![],
        );
        let reg = build_hook_registry(std::slice::from_ref(&hook));
        let entries = reg.by_kind(&EventKind::Started);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].origin, ReactionOrigin::HookConfig);
        assert_eq!(entries[0].ops.len(), 1);
        assert_eq!(entries[0].ops[0].name, "ark.core.exec");
    }

    /// Predicate that compiles cleanly through CEL is attached to the
    /// reaction; otherwise the reaction degrades to unconditional.
    #[test]
    fn registry_entries_carry_compiled_predicate_when_filters_present() {
        let hook = HookEntry::new(
            "true",
            vec![],
            vec!["done".into()],
            vec!["cavekit".into()],
            vec![],
        );
        let reg = build_hook_registry(std::slice::from_ref(&hook));
        let entries = reg.by_kind(&EventKind::Done);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].predicate.is_some(),
            "orchestrator filter must compile to a CEL predicate"
        );
    }

    /// `extend_registry_with_hooks` appends to an existing registry
    /// without dropping prior entries — the supervisor merges hook
    /// reactions into the user-scene registry at boot.
    #[test]
    fn extend_preserves_existing_entries() {
        let mut reg = ReactionRegistry::new();
        // Pre-existing user-scene reaction on Done.
        reg.insert(
            EventKind::Done,
            None,
            ReactionEntry {
                selector: "Done".into(),
                predicate: None,
                ops: vec![],
                origin: ReactionOrigin::UserScene,
            },
        );
        let hook = HookEntry::new(
            "true",
            vec![],
            vec!["done".into()],
            vec![],
            vec![],
        );
        extend_registry_with_hooks(&mut reg, std::slice::from_ref(&hook));
        let entries = reg.by_kind(&EventKind::Done);
        assert_eq!(entries.len(), 2, "user + hook entries must coexist");
        assert_eq!(entries[0].origin, ReactionOrigin::UserScene);
        assert_eq!(entries[1].origin, ReactionOrigin::HookConfig);
    }
}
