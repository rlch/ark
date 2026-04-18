//! Bare-`ark` session launch.
//!
//! When the user runs `ark` with no subcommand, this module resolves
//! a scene file, compiles it into a zellij layout, forks the
//! supervisor, and launches (or attaches to) a zellij session.
//!
//! ## Flags
//!
//! - `--scene <NAME_OR_PATH>` — resolve by name through
//!   `$ARK_CONFIG_DIR/scenes/<name>.kdl` or by explicit path.
//! - `--session <NAME>` — attach-or-create named zellij session.
//!   Inside zellij (`$ZELLIJ` set) dispatches `switch-session`;
//!   outside creates a new session.
//!
//! ## Injection seams
//!
//! [`run`] wires the production [`real::ZellijMultiplexer`] +
//! [`real::ForkSupervisor`] and delegates to [`run_with`], which
//! takes trait objects so integration tests can swap in mocks (see
//! [`traits`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ark_types::{SessionId, SessionSpec, StateLayout};
use chrono::Utc;

use crate::commands::session::{LayoutResolution, resolve_layout_source};
use crate::ctx::Ctx;
use crate::error::CliError;

pub mod compile;
pub mod mock;
pub mod real;
pub mod traits;

pub use traits::{Multiplexer, SupervisorSpawner};

// ---------------------------------------------------------- resolution ----

/// Determine whether a `--scene` value looks like a path (contains
/// `/` or ends with `.kdl`) rather than a bare name.
fn is_scene_path(value: &str) -> bool {
    value.contains('/') || value.ends_with(".kdl")
}

/// Resolve the scene file path on disk using the five-rung precedence
/// chain:
///
/// 1. `--scene` flag (name → `config_dir/scenes/<name>.kdl`; path → verbatim)
/// 2. `ARK_SCENE` env var
/// 3. `.ark/scene.kdl` in cwd
/// 4. `$XDG_CONFIG_HOME/ark/scenes/default.kdl`
/// 5. Built-in default (materialized to a per-run temp file)
fn resolve_scene_file(
    config_dir: &Path,
    cwd: &Path,
    scene_flag: Option<&str>,
) -> Option<PathBuf> {
    if let Some(val) = scene_flag {
        if is_scene_path(val) {
            return Some(PathBuf::from(val));
        }
    }

    match resolve_layout_source(config_dir, cwd, scene_flag) {
        LayoutResolution::SceneExplicit { path } | LayoutResolution::SceneDefault { path } => {
            Some(path)
        }
        LayoutResolution::Legacy => {
            // No scene file found at any rung — materialize the
            // embedded default scene to a per-run temp file so the
            // compile pipeline handles it the same as any other scene
            // file.
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .unwrap_or_else(|_| std::env::temp_dir().display().to_string());
            let default_path = PathBuf::from(runtime_dir).join("ark/default-scene.kdl");
            if let Some(parent) = default_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&default_path, ark_scene::default_scene::DEFAULT_SCENE_KDL).ok();
            Some(default_path)
        }
    }
}

/// Derive the session name.
///
/// Precedence:
/// 1. Explicit `--session NAME` flag.
/// 2. `"ark"` — fixed default so bare `ark` always gets the same
///    attach-or-create session.
fn derive_session_name(explicit: Option<&str>) -> String {
    explicit
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| "ark".to_string())
}

// --------------------------------------------------------------- entry ----

/// Production entry point. Wires the real multiplexer + fork spawner
/// and delegates to [`run_with`].
pub fn run(
    scene_flag: Option<&str>,
    session_flag: Option<&str>,
    ctx: &Ctx,
) -> Result<(), CliError> {
    let mux = real::ZellijMultiplexer::new();
    let spawner = real::ForkSupervisor::new();
    run_with(&mux, &spawner, scene_flag, session_flag, ctx)
}

/// Dependency-injected launch entry point. Accepts trait objects so
/// integration tests can substitute mocks that record invocations
/// without forking or shelling out to zellij.
pub fn run_with(
    mux: &dyn Multiplexer,
    spawner: &dyn SupervisorSpawner,
    scene_flag: Option<&str>,
    session_flag: Option<&str>,
    ctx: &Ctx,
) -> Result<(), CliError> {
    mux.preflight()?;

    let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
        reason: format!("failed to determine working directory: {e}"),
    })?;

    let session = derive_session_name(session_flag);
    let scene_file = resolve_scene_file(&ctx.config_dir, &cwd, scene_flag);

    // Build the session spec FIRST so scene layout compilation can
    // interpolate `{cwd}` / `{id}` / `{name}` / `{env.*}` brace-holes
    // against real session values. Post-cavekit-soul Phase 1 the bare
    // launch constructs a plain `SessionSpec`; orchestrator / engine /
    // cmd concepts have re-homed inside extensions.
    let spec = SessionSpec {
        id: SessionId::new(&session),
        name: session.clone(),
        scene_path: scene_file.clone(),
        cwd: cwd.clone(),
        env: BTreeMap::new(),
        created_at: Utc::now(),
        ext_config: BTreeMap::new(),
    };

    let compiled = compile::compile_scene_to_layout(
        scene_file.as_deref(),
        &spec.cwd.display().to_string(),
        &spec.id.as_path_leaf(),
        &spec.name,
        &spec.env,
    )?;

    let state_layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );

    // Fork the supervisor BEFORE invoking zellij so that `ark list`,
    // `ark kill`, plugin lifecycle, and hook dispatch all work for
    // this session. On real impl: the daemon grandchild never returns
    // from this call (exits internally). On mock impl: returns
    // immediately with simulated ready-ack.
    spawner.spawn_and_wait_for_ready(spec, &state_layout)?;

    mux.run_session(&session, compiled.layout_path.as_deref())?;

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
