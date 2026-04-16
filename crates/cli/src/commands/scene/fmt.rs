//! `ark scene fmt` — canonical-format scene files.
//!
//! T-119 (cavekit-scene R13). Ark-specific node ordering:
//! extends/include/use → layout → plugin → on → keybind. Idempotent.
//!
//! Node ordering convention for top-level children of `scene { }`:
//!
//! 1. `extends` / `include` / `use` — composition / activation nodes
//! 2. `layout` — structural layout
//! 3. `mode` — alternate whole-tab layouts
//! 4. `plugin` — plugin lifecycle blocks
//! 5. `on` — reaction declarations
//! 6. `keybind` / `bind` — keybind declarations
//! 7. `engine` — engine config
//! 8. `clear-*` / `disable-*` — removal / override nodes
//!
//! Unknown nodes are preserved at the end. The formatter operates on
//! raw KDL (no scene-grammar validation) so it can format files that
//! haven't passed `ark scene check` yet.

use std::path::PathBuf;

use clap::Args;

use ark_scene_v3::default_scene::DEFAULT_SCENE_KDL;
use ark_scene_v3::resolve_path::{resolve_scene_path, SceneSource};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene fmt`.
#[derive(Debug, Args)]
pub struct FmtArgs {
    /// Path to a scene file. Formats the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Check formatting without writing. Exit 1 if changes needed.
    #[arg(long)]
    pub check: bool,
}

/// Priority bucket for node ordering inside `scene { }`. Lower = earlier.
fn node_priority(name: &str) -> u8 {
    match name {
        "extends" | "include" | "use" => 0,
        "layout" => 1,
        "mode" => 2,
        "plugin" => 3,
        "on" => 4,
        "keybind" | "bind" => 5,
        "engine" => 6,
        "clear-reactions" | "clear-keybind" | "clear-keybinds"
        | "clear-bind" | "disable-extension" | "disable-plugin" | "disable-plugins" => 7,
        _ => 8, // unknown nodes go last
    }
}

/// Reorder top-level children of every `scene { }` node per ark convention,
/// then autoformat the full document. Returns the formatted string.
fn canonical_format(src: &str, path: &std::path::Path) -> Result<String, CliError> {
    let mut doc = kdl::KdlDocument::parse(src).map_err(|e| CliError::Generic {
        reason: format!("{}: {e}", path.display()),
    })?;

    // Walk top-level nodes looking for `scene` blocks with children.
    for node in doc.nodes_mut() {
        if node.name().to_string() == "scene" {
            if let Some(children_doc) = node.children_mut() {
                let nodes = children_doc.nodes_mut();
                nodes.sort_by_key(|n| node_priority(&n.name().to_string()));
            }
        }
    }

    doc.autoformat();
    Ok(doc.to_string())
}

pub fn run(args: FmtArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let (path, src) = resolve_input(&args)?;

    let formatted = canonical_format(&src, &path)?;

    if args.check {
        if formatted == src {
            eprintln!("scene fmt: {} ok", path.display());
            Ok(())
        } else {
            Err(CliError::Generic {
                reason: format!("scene fmt: {} needs formatting", path.display()),
            })
        }
    } else if args.path.is_some() {
        // Explicit path: write back to file.
        std::fs::write(&path, &formatted).map_err(|e| CliError::Generic {
            reason: format!("cannot write {}: {e}", path.display()),
        })?;
        eprintln!("scene fmt: {}", path.display());
        Ok(())
    } else {
        // No explicit path: resolved via T-113. Write back if it's a
        // real file; print to stdout if it's the built-in default.
        let resolved = resolve_default_scene_source();
        match resolved {
            Some(p) => {
                std::fs::write(&p, &formatted).map_err(|e| CliError::Generic {
                    reason: format!("cannot write {}: {e}", p.display()),
                })?;
                eprintln!("scene fmt: {}", p.display());
            }
            None => {
                // Built-in — just print to stdout.
                print!("{formatted}");
            }
        }
        Ok(())
    }
}

/// Resolve the input file: explicit path or default scene via resolver.
fn resolve_input(args: &FmtArgs) -> Result<(PathBuf, String), CliError> {
    if let Some(ref path) = args.path {
        let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
            reason: format!("cannot read {}: {e}", path.display()),
        })?;
        Ok((path.clone(), src))
    } else {
        let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
            reason: format!("cannot determine cwd: {e}"),
        })?;
        let env_scene = std::env::var("ARK_SCENE").ok();
        let xdg_config = xdg_config_dir();
        match resolve_scene_path(
            None,
            env_scene.as_deref(),
            None,
            xdg_config.as_deref(),
            &cwd,
        ) {
            SceneSource::Flag(p)
            | SceneSource::EnvVar(p)
            | SceneSource::ProjectLocal(p)
            | SceneSource::UserConfig(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                Ok((p, src))
            }
            SceneSource::BuiltIn => {
                Ok((PathBuf::from("<built-in>"), DEFAULT_SCENE_KDL.to_string()))
            }
        }
    }
}

/// If the default scene resolves to a real file path (not built-in),
/// return that path. Used by the write-back logic.
fn resolve_default_scene_source() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let env_scene = std::env::var("ARK_SCENE").ok();
    let xdg_config = xdg_config_dir();
    match resolve_scene_path(
        None,
        env_scene.as_deref(),
        None,
        xdg_config.as_deref(),
        &cwd,
    ) {
        SceneSource::Flag(p)
        | SceneSource::EnvVar(p)
        | SceneSource::ProjectLocal(p)
        | SceneSource::UserConfig(p) => Some(p),
        SceneSource::BuiltIn => None,
    }
}

/// Best-effort XDG config dir using `$XDG_CONFIG_HOME` or `~/.config`.
fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".config"));
    }
    None
}
