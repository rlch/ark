//! `ark scene fmt` — canonical-format scene files.
//!
//! T-12.3 (cavekit-scene R13). Ark-specific node ordering:
//! extends/include/use → layout → plugin → on → keybind. Idempotent.
//!
//! Node ordering convention for top-level children of `scene { }`:
//!
//! 1. `extends` / `include` / `use` — composition / activation nodes
//! 2. `layout` — structural layout
//! 3. `plugin` — plugin lifecycle blocks
//! 4. `on` — reaction declarations
//! 5. `keybind` — keybind declarations
//! 6. `engine` / `clear-reactions` / `clear-keybind` / `disable-plugin` — misc
//!
//! Unknown nodes are preserved at the end. The formatter operates on
//! raw KDL (no scene-grammar validation) so it can format files that
//! haven't passed `ark scene check` yet.

use std::path::PathBuf;

use clap::Args;

use ark_scene::path::{ResolvedScene, resolve_scene_path_from_env};

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
        "plugin" => 2,
        "on" => 3,
        "keybind" => 4,
        "engine" => 5,
        "clear-reactions" | "clear-keybind" | "clear-keybinds" | "disable-plugin" | "disable-plugins" => 6,
        _ => 7, // unknown nodes go last
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
    } else {
        // For built-in scenes or when no path was given, just print to stdout.
        if args.path.is_some() {
            std::fs::write(&path, &formatted).map_err(|e| CliError::Generic {
                reason: format!("cannot write {}: {e}", path.display()),
            })?;
            eprintln!("scene fmt: {}", path.display());
        } else {
            // No explicit path means we resolved the default. If it's a real
            // file, write back; if it's built-in, print to stdout.
            let cwd = std::env::current_dir().unwrap_or_default();
            match resolve_scene_path_from_env(None, &cwd) {
                ResolvedScene::Path(p) => {
                    std::fs::write(&p, &formatted).map_err(|e| CliError::Generic {
                        reason: format!("cannot write {}: {e}", p.display()),
                    })?;
                    eprintln!("scene fmt: {}", p.display());
                }
                _ => {
                    // Built-in or named — just print to stdout.
                    print!("{formatted}");
                }
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
        match resolve_scene_path_from_env(None, &cwd) {
            ResolvedScene::Named(name) => Err(CliError::Generic {
                reason: format!(
                    "scene `{name}` resolved by name; pass an explicit path to format"
                ),
            }),
            ResolvedScene::Path(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                Ok((p, src))
            }
            ResolvedScene::BuiltIn(src) => {
                Ok((PathBuf::from("<built-in>"), src.to_string()))
            }
        }
    }
}
