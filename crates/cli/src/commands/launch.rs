//! T-115 / T-116 / T-117: Bare `ark` session launch.
//!
//! When the user runs `ark` with no subcommand, this module resolves a
//! scene file, compiles it into a zellij layout, and launches (or
//! attaches to) a zellij session.
//!
//! - **T-115**: bare `ark` resolves the default scene via the five-rung
//!   precedence chain and launches a session.
//! - **T-116**: `ark --scene <name-or-path>` — names resolve through
//!   the scene search path (`$ARK_CONFIG_DIR/scenes/<name>.kdl`); paths
//!   containing `/` or ending in `.kdl` are used verbatim.
//! - **T-117**: `ark --session <name>` — attach-or-create a named
//!   zellij session. Inside zellij (`$ZELLIJ` set) dispatches
//!   `switch-session`; outside creates a new session.

use std::path::{Path, PathBuf};

use crate::commands::session::{
    LayoutResolution, ZellijSpawn, build_switch_session_command, build_zellij_command,
    inside_zellij, require_zellij_on_path, resolve_layout_source,
};
use crate::ctx::Ctx;
use crate::error::CliError;

/// Determine whether a `--scene` value looks like a path (contains `/`
/// or ends with `.kdl`) rather than a bare name.
fn is_scene_path(value: &str) -> bool {
    value.contains('/') || value.ends_with(".kdl")
}

/// Resolve scene to a file path on disk using the same five-rung
/// precedence as `ark spawn`:
///
/// 1. `--scene` flag (name → `config_dir/scenes/<name>.kdl`; path → verbatim)
/// 2. `ARK_SCENE` env var
/// 3. `.ark/scene.kdl` in cwd
/// 4. `$XDG_CONFIG_HOME/ark/scenes/default.kdl`
/// 5. Built-in default (legacy path)
fn resolve_scene_file(
    config_dir: &Path,
    cwd: &Path,
    scene_flag: Option<&str>,
) -> Option<PathBuf> {
    // T-116: if the flag value looks like a filesystem path, use it
    // verbatim instead of routing through the name-based resolver.
    if let Some(val) = scene_flag {
        if is_scene_path(val) {
            return Some(PathBuf::from(val));
        }
    }

    match resolve_layout_source(config_dir, cwd, scene_flag) {
        LayoutResolution::SceneExplicit { path } | LayoutResolution::SceneDefault { path } => {
            Some(path)
        }
        LayoutResolution::Legacy => None,
    }
}

/// Derive the session name for a bare-`ark` launch.
///
/// Precedence:
/// 1. Explicit `--session NAME` flag.
/// 2. `"ark"` — a fixed default so bare `ark` always gets the same
///    session (attach-or-create semantics).
fn derive_session_name(explicit: Option<&str>) -> String {
    explicit
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| "ark".to_string())
}

/// Entry point for bare `ark` (no subcommand).
///
/// Resolves a scene, compiles the layout, and launches or attaches to
/// the zellij session.
pub fn run(
    scene_flag: Option<&str>,
    session_flag: Option<&str>,
    ctx: &Ctx,
) -> Result<(), CliError> {
    require_zellij_on_path()?;

    let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
        reason: format!("failed to determine working directory: {e}"),
    })?;

    let session = derive_session_name(session_flag);

    // Resolve scene file (T-115 + T-116).
    let scene_file = resolve_scene_file(&ctx.config_dir, &cwd, scene_flag);

    // Compile scene → layout KDL if a scene file was resolved.
    // For the built-in/legacy fallback (no scene file), we use the
    // default layout template path.
    let layout_path: Option<PathBuf> = match scene_file {
        Some(ref path) => {
            if !path.exists() {
                return Err(CliError::NotFound {
                    what: format!("scene file `{}`", path.display()),
                });
            }
            // Build a minimal AgentSpec-like snapshot for the compile
            // context. The launch path doesn't create a full agent —
            // it just needs the scene compiled to a layout.
            let compile_ctx = ark_scene::compile::CompileContext::new(
                ark_scene::context::AgentSnapshot {
                    id: String::new(),
                    name: session.clone(),
                    orchestrator: String::new(),
                    engine: String::new(),
                    cwd: cwd.display().to_string(),
                    cmd: String::new(),
                    args: Vec::new(),
                },
                ark_scene::context::SessionSnapshot {
                    name: session.clone(),
                },
            );

            let (rendered, _scene_id) =
                ark_scene::compile::compile_scene_file(path, &ctx.runtime_dir, &compile_ctx)
                    .map_err(|e| CliError::Generic {
                        reason: format!("compile scene `{}`: {e}", path.display()),
                    })?;
            Some(rendered)
        }
        None => None,
    };

    let plan = ZellijSpawn {
        session: session.clone(),
        layout: layout_path.map(|p| p.display().to_string()),
    };

    // T-117: $ZELLIJ detection — inside = switch-session, outside = new.
    let is_inside = inside_zellij(|k| std::env::var(k).ok());

    if is_inside {
        let mut zcmd = build_switch_session_command(&plan);
        let status = zcmd.status().map_err(|e| CliError::Internal {
            reason: format!("zellij action switch-session: {e}"),
        })?;
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            return Err(CliError::Internal {
                reason: format!(
                    "zellij action switch-session exited with code {code}"
                ),
            });
        }
    } else {
        // Outside zellij: create a new session (attach semantics —
        // zellij -s <name> attaches if the session already exists).
        let mut zcmd = build_zellij_command(&plan);
        // Inherit stdio so the user gets the TUI.
        let status = zcmd.status().map_err(|e| CliError::Internal {
            reason: format!("zellij: {e}"),
        })?;
        if !status.success() {
            let code = status.code().unwrap_or(-1);
            return Err(CliError::Internal {
                reason: format!("zellij exited with code {code}"),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_scene_path_detects_slash() {
        assert!(is_scene_path("./my-scene.kdl"));
        assert!(is_scene_path("/home/user/scene.kdl"));
        assert!(is_scene_path("scenes/work"));
    }

    #[test]
    fn is_scene_path_detects_kdl_extension() {
        assert!(is_scene_path("work.kdl"));
    }

    #[test]
    fn is_scene_path_bare_name_is_false() {
        assert!(!is_scene_path("work"));
        assert!(!is_scene_path("my-project"));
    }

    #[test]
    fn derive_session_name_explicit() {
        assert_eq!(derive_session_name(Some("work")), "work");
    }

    #[test]
    fn derive_session_name_default() {
        assert_eq!(derive_session_name(None), "ark");
    }

    #[test]
    fn derive_session_name_empty_falls_back() {
        assert_eq!(derive_session_name(Some("")), "ark");
    }
}
