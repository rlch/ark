//! `ark scene dry-run` — simulate one event fire against the current scene.
//!
//! T-12.4 (cavekit-scene R13). Prints resolved op list per matching
//! reaction without side effects. Uses the same selector grammar the
//! reaction registry / dispatcher use at runtime.
//!
//! ## Migration status
//!
//! This command was migrated from ark-scene v2 to v3 at the Cargo.toml
//! level. The implementation below uses the v3 scene crate's raw-KDL
//! walking (which needs no v2 APIs) for reaction extraction, but the
//! CEL predicate evaluation and selector matching still require v2-only
//! APIs (`cel`, `context::build_context`, `selector::parse_selector`,
//! `reactions::EventKind`) that have not yet been ported to v3. Until
//! those APIs land in the v3 crate the `--event` matching and `if=`
//! predicate evaluation are stubbed (reactions always "match" and
//! predicates are reported as "unconditional").

use std::path::{Path, PathBuf};

use clap::Args;
use kdl::KdlDocument;

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

    // ---- Step 2: parse raw KDL so we can recover op node names --------
    let doc = KdlDocument::parse(&src).map_err(|e| CliError::Generic {
        reason: format!("{display_path}: {e}"),
    })?;

    // ---- Step 3: walk every `on { }` inside `scene { }` ---------------
    let reactions = collect_reactions(&doc);
    if reactions.is_empty() {
        println!("scene dry-run ({display_path}): no reactions declared");
        return Ok(());
    }

    // ---- Step 4: report (selector matching + CEL predicate evaluation
    // are pending v3 migration of the selector / cel / context modules;
    // for now we list all reactions and mark each as "unconditional"). ---
    println!(
        "scene dry-run ({display_path}) event={event}  matched={}  would-fire={}",
        reactions.len(),
        reactions.len(),
        event = args.event,
    );
    for r in &reactions {
        let if_tag = r
            .if_expr
            .as_deref()
            .map(|s| format!(" if=\"{s}\""))
            .unwrap_or_default();
        println!(
            "  on \"{}\"{if_tag} [unconditional (selector/CEL matching pending v3 migration)]",
            r.selector
        );
        if r.ops.is_empty() {
            println!("    (no ops)");
        } else {
            for (i, op) in r.ops.iter().enumerate() {
                println!("    {}. {op}", i + 1);
            }
        }
    }
    Ok(())
}

// -----------------------------------------------------------------------
// Scene source loading
// -----------------------------------------------------------------------

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
        let env_scene = std::env::var("ARK_SCENE").ok();
        let env_appname = std::env::var("ARK_APPNAME").ok();
        let xdg_config_home = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| std::path::PathBuf::from(h).join(".config"))
            });
        match ark_scene::resolve_path::resolve_scene_path(
            None,
            env_scene.as_deref(),
            env_appname.as_deref(),
            xdg_config_home.as_deref(),
            &cwd,
        ) {
            ark_scene::resolve_path::SceneSource::Flag(p)
            | ark_scene::resolve_path::SceneSource::EnvVar(p)
            | ark_scene::resolve_path::SceneSource::ProjectLocal(p)
            | ark_scene::resolve_path::SceneSource::UserConfig(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                Ok((src, p.display().to_string()))
            }
            ark_scene::resolve_path::SceneSource::BuiltIn => Ok((
                ark_scene::default_scene::DEFAULT_SCENE_KDL.to_string(),
                "<built-in>".to_string(),
            )),
        }
    }
}

// -----------------------------------------------------------------------
// Reaction extraction (raw KDL)
// -----------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ReactionDescriptor {
    selector: String,
    if_expr: Option<String>,
    ops: Vec<String>,
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
}
