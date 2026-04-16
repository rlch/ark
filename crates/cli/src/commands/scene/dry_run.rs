//! `ark scene dry-run` — simulate one event fire against the current scene.
//!
//! T-12.4 (cavekit-scene R13). Prints resolved op list per matching
//! reaction without side effects. Uses the same selector grammar the
//! reaction registry / dispatcher use at runtime.
//!
//! The command:
//!
//! 1. Loads the scene source (explicit path or default resolver).
//! 2. Parses it into a [`SceneDoc`] and populates the raw KDL.
//! 3. Parses the user-supplied `--event` selector via
//!    [`ark_scene::selector::parse_selector`].
//! 4. Walks every `on { }` reaction in the scene (and its composed
//!    fragments, where applicable), matching the reaction's selector
//!    against the simulated event selector (kind + UserEvent name).
//! 5. When the reaction has an `if="<CEL>"` predicate, compiles it with
//!    [`ark_scene::cel::compile`] and evaluates against a CEL context
//!    built from the --payload (or `{}` when omitted) via
//!    [`ark_scene::context::build_context`]. Predicate errors surface as
//!    `skipped (predicate error: ...)` per reaction rather than failing
//!    the whole simulation.
//! 6. For each matching reaction, prints a compact summary: matching
//!    selector, predicate verdict, and ordered op names pulled from the
//!    raw KDL (op nodes don't yet carry names in the typed AST —
//!    TODO(T-3.2)). No side effects occur.

use std::path::{Path, PathBuf};

use clap::Args;
use kdl::{KdlDocument, KdlNode};
use serde_json::Value;

use ark_scene::cel;
use ark_scene::context::{AgentSnapshot, SessionSnapshot, build_context};
use ark_scene::path::{ResolvedScene, resolve_scene_path_from_env};
use ark_scene::selector::{EventSelector, parse_selector};
use ark_types::event::AgentEvent;
use ark_types::id::AgentId;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene dry-run`.
#[derive(Debug, Args)]
pub struct DryRunArgs {
    /// Event selector to simulate (e.g. `Started`, `UserEvent:ark.picker.accept`).
    #[arg(long, required = true)]
    pub event: String,

    /// Optional JSON payload for the simulated event. Bound to
    /// `event.payload` / top-level `payload` in CEL predicates.
    #[arg(long)]
    pub payload: Option<String>,

    /// Path to a scene file. Uses the default-resolver when omitted.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

pub fn run(args: DryRunArgs, _ctx: &Ctx) -> Result<(), CliError> {
    // ---- Step 1: load scene source ------------------------------------
    let (src, display_path) = load_scene_source(args.file.as_deref())?;

    // ---- Step 2: parse the user's event selector ----------------------
    let user_sel = parse_selector(&args.event).map_err(|e| CliError::Generic {
        reason: format!("invalid --event selector: {e}"),
    })?;

    // ---- Step 3: parse payload JSON (or default to null) --------------
    let payload: Value = match args.payload {
        Some(ref s) => serde_json::from_str(s).map_err(|e| CliError::Generic {
            reason: format!("invalid --payload JSON: {e}"),
        })?,
        None => Value::Null,
    };

    // ---- Step 4: build a synthetic AgentEvent so CEL context has fields
    let event = synthetic_event(&user_sel, &payload);

    // ---- Step 5: parse raw KDL so we can recover op node names --------
    // We deliberately walk the raw kdl::KdlDocument rather than the
    // facet-derived AST so reaction bodies expose their op node names
    // (the facet AST's OpNode is opaque — TODO(T-3.2)).
    let doc = KdlDocument::parse(&src).map_err(|e| CliError::Generic {
        reason: format!("{display_path}: {e}"),
    })?;

    // ---- Step 6: walk every `on { }` inside `scene { }` ---------------
    let reactions = collect_reactions(&doc);
    if reactions.is_empty() {
        println!("scene dry-run ({display_path}): no reactions declared");
        return Ok(());
    }

    // ---- Step 7: match + evaluate each reaction -----------------------
    let mut matched: Vec<ReactionDryRun> = Vec::new();
    for r in &reactions {
        let reaction_sel = match parse_selector(&r.selector) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !selectors_compatible(&user_sel, &reaction_sel) {
            continue;
        }
        let predicate_verdict = evaluate_predicate(&r.if_expr, &event, &payload);
        matched.push(ReactionDryRun {
            selector: r.selector.clone(),
            if_expr: r.if_expr.clone(),
            ops: r.ops.clone(),
            predicate: predicate_verdict,
        });
    }

    // ---- Step 8: render the report -----------------------------------
    render_report(&display_path, &args.event, &matched);
    Ok(())
}

// -----------------------------------------------------------------------
// Scene source loading
// -----------------------------------------------------------------------

/// Load the scene source text and a display-friendly path string.
///
/// When the user supplies an explicit path, we read from disk. Otherwise
/// we run the scene-path resolver (R13 precedence) to find the default
/// scene and read it — or use the built-in default if nothing on disk
/// matches.
fn load_scene_source(explicit: Option<&Path>) -> Result<(String, String), CliError> {
    if let Some(path) = explicit {
        let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
            reason: format!("cannot read {}: {e}", path.display()),
        })?;
        Ok((src, path.display().to_string()))
    } else {
        let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
            reason: format!("cannot determine cwd: {e}"),
        })?;
        match resolve_scene_path_from_env(None, &cwd) {
            ResolvedScene::Named(name) => Err(CliError::Generic {
                reason: format!(
                    "scene `{name}` resolved by name; pass --file to dry-run against a specific file"
                ),
            }),
            ResolvedScene::Path(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                Ok((src, p.display().to_string()))
            }
            ResolvedScene::BuiltIn(src) => Ok((src.to_string(), "<built-in>".to_string())),
        }
    }
}

// -----------------------------------------------------------------------
// Reaction extraction (raw KDL)
// -----------------------------------------------------------------------

/// One reaction extracted from the raw KDL source. Holds the selector,
/// optional `if=` expression, and the ordered op node names found in
/// the reaction body.
#[derive(Debug, Clone)]
struct ReactionDescriptor {
    selector: String,
    if_expr: Option<String>,
    ops: Vec<String>,
}

/// Compact per-reaction dry-run output row.
#[derive(Debug)]
struct ReactionDryRun {
    selector: String,
    if_expr: Option<String>,
    ops: Vec<String>,
    predicate: PredicateVerdict,
}

/// Outcome of evaluating a reaction's `if=` predicate against the
/// simulated event.
#[derive(Debug)]
enum PredicateVerdict {
    /// No `if=` predicate was declared — the reaction fires unconditionally.
    None,
    /// Predicate evaluated to `true`.
    True,
    /// Predicate evaluated to `false`.
    False,
    /// Predicate compile / eval error — reaction is skipped.
    Error(String),
}

fn collect_reactions(doc: &KdlDocument) -> Vec<ReactionDescriptor> {
    let mut out: Vec<ReactionDescriptor> = Vec::new();
    for node in doc.nodes() {
        if node.name().value() == "scene" {
            if let Some(children) = node.children() {
                collect_on_nodes(children, &mut out);
            }
        }
    }
    out
}

fn collect_on_nodes(doc: &KdlDocument, out: &mut Vec<ReactionDescriptor>) {
    for node in doc.nodes() {
        if node.name().value() == "on" {
            let selector = match node
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_string())
            {
                Some(s) => s.to_string(),
                None => continue,
            };
            let if_expr = node
                .entries()
                .iter()
                .find(|e| e.name().map(|n| n.value()) == Some("if"))
                .and_then(|e| e.value().as_string())
                .map(|s| s.to_string());
            let ops: Vec<String> = node
                .children()
                .map(|c| {
                    c.nodes()
                        .iter()
                        .map(|n| n.name().value().to_string())
                        .collect()
                })
                .unwrap_or_default();
            out.push(ReactionDescriptor {
                selector,
                if_expr,
                ops,
            });
        }
    }
}

// -----------------------------------------------------------------------
// Selector compatibility
// -----------------------------------------------------------------------

/// Return true when the scene reaction's selector could match an event
/// described by the user's selector.
///
/// Checks kind equality + UserEvent-name equality. Field patterns on the
/// reaction's selector are NOT re-evaluated here — dry-run works at the
/// selector-shape level; per-field matching requires a concrete event
/// instance (which we'd have to synthesize), and the selector itself
/// already declares the author's intent.
fn selectors_compatible(user: &EventSelector, reaction: &EventSelector) -> bool {
    if user.kind != reaction.kind {
        return false;
    }
    match (&user.user_event_name, &reaction.user_event_name) {
        (Some(u), Some(r)) => u == r,
        (_, None) => true, // reaction matches any user-event name
        (None, Some(_)) => false, // reaction wants a specific name, user didn't say
    }
}

// -----------------------------------------------------------------------
// Synthetic event + predicate evaluation
// -----------------------------------------------------------------------

/// Synthesise a minimal [`AgentEvent`] from the user's selector kind so
/// the CEL context has `event.kind` populated. `UserEvent` selectors
/// additionally carry the name + payload bound to `event.payload`.
fn synthetic_event(sel: &EventSelector, payload: &Value) -> AgentEvent {
    use ark_scene::reactions::EventKind;
    use ark_types::event::{LogLevel, Outcome};
    use ark_types::spec::AgentSpec;
    let id = AgentId::new("dryrun", "scene");
    match sel.kind {
        EventKind::UserEvent => AgentEvent::UserEvent {
            name: sel.user_event_name.clone().unwrap_or_default(),
            payload: payload.clone(),
            source: "dry-run".into(),
        },
        EventKind::Started => AgentEvent::Started {
            spec: AgentSpec::new(
                id,
                "dry-run",
                "dry-run",
                "dry-run",
                std::path::PathBuf::from("."),
                vec![],
            ),
        },
        EventKind::Log => AgentEvent::Log {
            id,
            level: LogLevel::Info,
            line: String::new(),
        },
        EventKind::Error => AgentEvent::Error {
            id,
            message: String::new(),
        },
        EventKind::Done => AgentEvent::Done {
            id,
            outcome: Outcome::Success { artifacts: vec![] },
        },
        // For every other kind we still synthesise a Log-shaped event —
        // the CEL context only needs `event.kind` to be present, and
        // fields on the real variant are often required arguments the
        // user isn't asked to provide in dry-run. Predicates that read
        // variant-specific fields will surface a CEL error (`no such
        // key`), which we report as "predicate error".
        _ => AgentEvent::Log {
            id,
            level: LogLevel::Info,
            line: String::new(),
        },
    }
}

fn evaluate_predicate(
    if_expr: &Option<String>,
    event: &AgentEvent,
    payload: &Value,
) -> PredicateVerdict {
    let Some(expr) = if_expr else {
        return PredicateVerdict::None;
    };
    let prog = match cel::compile(expr, "<if=>", 0) {
        Ok(p) => p,
        Err(e) => return PredicateVerdict::Error(format!("compile: {e}")),
    };
    let agent = AgentSnapshot {
        id: "dryrun".into(),
        name: "dryrun".into(),
        orchestrator: "dryrun".into(),
        engine: "dryrun".into(),
        cwd: ".".into(),
        cmd: "dryrun".into(),
        args: Vec::new(),
    };
    let session = SessionSnapshot {
        name: "dryrun".into(),
    };
    let ctx = match build_context(event, Some(payload), &agent, &session) {
        Ok(c) => c,
        Err(e) => return PredicateVerdict::Error(format!("context: {e}")),
    };
    match cel::eval_bool(&prog, &ctx) {
        Ok(true) => PredicateVerdict::True,
        Ok(false) => PredicateVerdict::False,
        Err(e) => PredicateVerdict::Error(format!("eval: {e}")),
    }
}

// -----------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------

fn render_report(display_path: &str, event: &str, matched: &[ReactionDryRun]) {
    let fired: Vec<&ReactionDryRun> = matched
        .iter()
        .filter(|r| {
            !matches!(
                r.predicate,
                PredicateVerdict::False | PredicateVerdict::Error(_)
            )
        })
        .collect();
    println!(
        "scene dry-run ({display_path}) event={event}  matched={}  would-fire={}",
        matched.len(),
        fired.len()
    );
    if matched.is_empty() {
        return;
    }
    for r in matched {
        let verdict = match &r.predicate {
            PredicateVerdict::None => "unconditional".to_string(),
            PredicateVerdict::True => "if → true".to_string(),
            PredicateVerdict::False => "if → false (skipped)".to_string(),
            PredicateVerdict::Error(e) => format!("if → error: {e} (skipped)"),
        };
        let if_tag = r
            .if_expr
            .as_deref()
            .map(|s| format!(" if=\"{s}\""))
            .unwrap_or_default();
        println!("  on \"{}\"{if_tag} [{verdict}]", r.selector);
        if r.ops.is_empty() {
            println!("    (no ops)");
        } else {
            for (i, op) in r.ops.iter().enumerate() {
                println!("    {}. {op}", i + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kdl::KdlDocument;

    fn parse_doc(src: &str) -> KdlDocument {
        KdlDocument::parse(src).expect("parse")
    }

    #[test]
    fn collect_reactions_pulls_selector_if_and_ops() {
        let src = r#"
scene "demo" {
    on "Started" {
        open_tab name="x"
        emit event="ready"
    }
    on "UserEvent:hello" if="event.kind == \"user_event\"" {
        emit event="bye"
    }
}
"#;
        let doc = parse_doc(src);
        let rs = collect_reactions(&doc);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].selector, "Started");
        assert_eq!(rs[0].if_expr, None);
        assert_eq!(rs[0].ops, vec!["open_tab".to_string(), "emit".to_string()]);
        assert_eq!(rs[1].selector, "UserEvent:hello");
        assert!(rs[1].if_expr.is_some());
    }

    #[test]
    fn selectors_compatible_matches_same_kind() {
        let u = parse_selector("Started").unwrap();
        let r = parse_selector("Started").unwrap();
        assert!(selectors_compatible(&u, &r));
    }

    #[test]
    fn selectors_compatible_rejects_different_kind() {
        let u = parse_selector("Started").unwrap();
        let r = parse_selector("PhaseTransition").unwrap();
        assert!(!selectors_compatible(&u, &r));
    }

    #[test]
    fn selectors_compatible_matches_same_user_event_name() {
        let u = parse_selector("UserEvent:hello").unwrap();
        let r = parse_selector("UserEvent:hello").unwrap();
        assert!(selectors_compatible(&u, &r));
    }

    #[test]
    fn selectors_compatible_rejects_different_user_event_name() {
        let u = parse_selector("UserEvent:hello").unwrap();
        let r = parse_selector("UserEvent:world").unwrap();
        assert!(!selectors_compatible(&u, &r));
    }

    #[test]
    fn reaction_without_user_event_name_matches_any_user_event() {
        // Bare `UserEvent` on the reaction side matches a user who
        // targets a specific name.
        let u = parse_selector("UserEvent:hello").unwrap();
        let r = parse_selector("user_event").unwrap();
        assert!(selectors_compatible(&u, &r));
    }
}
