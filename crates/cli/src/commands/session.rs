//! Shared zellij session helpers.
//!
//! Extracted from `spawn.rs` (T-115) so that `launch.rs` (bare `ark`)
//! and any future session-management paths share a single source of
//! truth for zellij preflight, session detection, layout resolution,
//! and command building.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::CliError;

// ---------------------------------------------------------- detection ----

/// Whether we are already inside a zellij session (env snapshot).
///
/// Kept public for diagnostics / doctor; no longer steers
/// `build_zellij_command` since F-516 unifies both paths behind
/// `setsid zellij -s …`.
pub fn inside_zellij<F: Fn(&str) -> Option<String>>(getter: F) -> bool {
    matches!(getter("ZELLIJ"), Some(v) if !v.is_empty())
}

/// Preflight: `zellij` must be on PATH. Returns `PreflightFail`
/// with a clear reason when the binary is missing. No-op on success.
pub fn require_zellij_on_path() -> Result<(), CliError> {
    let status = Command::new("zellij")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(CliError::PreflightFail {
            reason: "zellij not found on PATH".to_string(),
        }),
    }
}

// --------------------------------------------------------- spawn plan ----

/// A resolved zellij spawn plan: create a dedicated per-agent
/// session via `setsid zellij -s <name> --layout <path>`.
///
/// F-516 / F-517: prior cycles branched on `$ZELLIJ` and emitted
/// either `zellij action new-tab` (which only adds a tab to the
/// caller's session, violating R2's 1:1 agent↔session mapping) or
/// `zellij attach --create` (which needs a TTY — incompatible with
/// `/dev/null` stdio + `spawn()`). Unifying on `setsid zellij -s`
/// mirrors the canonical pattern in `crates/mux/zellij/src/mux.rs`
/// and detaches cleanly from the caller's controlling terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZellijSpawn {
    /// Session name (1:1 with agent id).
    pub session: String,
    /// Layout path (stem or absolute) — required: R2 defines a
    /// default layout for every spawn so this is never `None` in
    /// practice, but we keep the type `Option` for forward-compat
    /// with future layout-less spawn modes (e.g. `ark spawn --bare`).
    pub layout: Option<String>,
}

/// Build the command for a given [`ZellijSpawn`] plan.
///
/// F-526: the argv is now pure `zellij -s <session> [--layout <path>]` —
/// the external `setsid` binary was dropped because macOS does not ship
/// it on a default install, which caused spawn to fail with "No such file
/// or directory" even when zellij itself was installed. Detaching from
/// the caller's controlling TTY is handled POSIX-natively by
/// `apply_detach` via `pre_exec(nix::unistd::setsid)`, which works
/// identically on Linux and macOS.
pub fn build_zellij_command(plan: &ZellijSpawn) -> Command {
    let mut c = Command::new("zellij");
    c.arg("-s").arg(&plan.session);
    if let Some(p) = &plan.layout {
        c.arg("--layout").arg(p);
    }
    c
}

/// Build the command for switching to an existing session from inside
/// zellij.
///
/// `zellij action switch-session <name> [--layout <path>]` (with the
/// `--create` flag). Works without pty, setsid, or stdio changes because
/// the command is an IPC dispatch over the caller's live zellij socket;
/// `Command::status()` blocks until the dispatch acks and returns.
///
/// Mirrors the argv shape used by `crates/mux/zellij/src/mux.rs:266`.
pub fn build_switch_session_command(plan: &ZellijSpawn) -> Command {
    let mut c = Command::new("zellij");
    c.arg("action").arg("switch-session");
    if let Some(p) = &plan.layout {
        c.arg("--layout").arg(p);
    }
    c.arg(&plan.session);
    c
}

// ------------------------------------------------------- layout resolution ----

/// T-3.5 / T-8.2: multi-rung decision for "how does this spawn acquire
/// a zellij layout?".
///
/// Reported back to the caller as a discriminated enum so tests can
/// assert the resolution path independently of the rendered output.
///
/// T-8.2 re-homed the internals of [`resolve_layout_source`] onto the
/// T-8.0 scene resolver ([`ark_scene::path::resolve_scene_path_from_env`]),
/// which also consults `ARK_SCENE`, `ARK_APPNAME`, project-local
/// `.ark/scene.kdl`, and the XDG default scene. The enum shape is
/// preserved so the spawn pipeline + existing tests keep working:
///   - `ResolvedScene::Named(n)` → [`Self::SceneExplicit`] under
///     `${config_dir}/scenes/<n>.kdl` (combo 3A).
///   - `ResolvedScene::Path(p)` → [`Self::SceneDefault`] (both the
///     project-local rung and the XDG-default rung yielded a concrete
///     file on disk).
///   - `ResolvedScene::BuiltIn(_)` → [`Self::Legacy`] (T-14.1 will
///     materialize the embedded scene to disk and promote it to a
///     proper scene compile; today it falls through to the legacy
///     `--layout <stem>` path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutResolution {
    /// Scene identified by name: either `--scene NAME` from the CLI
    /// (rung 1) or `ARK_SCENE=NAME` from the environment (rung 2).
    /// `path` is always `${config_dir}/scenes/<name>.kdl`.
    SceneExplicit { path: PathBuf },
    /// Scene identified by a concrete file on disk: project-local
    /// `./.ark/scene.kdl` (rung 3) or XDG-default
    /// `$XDG_CONFIG_HOME/<appname>/scenes/default.kdl` (rung 4).
    SceneDefault { path: PathBuf },
    /// No scene resolved at any rung — fall through to the legacy
    /// `--layout <stem>` path (T-14.1 will replace this branch with
    /// an auto-wrapped minimal scene so both tiers share the compile
    /// pipeline).
    Legacy,
}

/// T-3.5 / T-8.2: resolve which scene-file, if any, drives this spawn.
///
/// Delegates to [`ark_scene::path::resolve_scene_path_from_env`], which
/// implements the `cavekit-scene.md` R13 precedence (CLI flag →
/// `ARK_SCENE` → `./.ark/scene.kdl` → XDG default → built-in) and
/// reads `ARK_SCENE`, `ARK_APPNAME`, and `XDG_CONFIG_HOME` from the
/// process environment.
///
/// The translation from [`ResolvedScene`] to [`LayoutResolution`]
/// preserves the enum shape expected by the downstream pipeline:
///   - [`ResolvedScene::Named`] → [`LayoutResolution::SceneExplicit`]
///     with the path rooted at `${config_dir}/scenes/<name>.kdl`.
///     Named scenes intentionally resolve under `ctx.config_dir` (NOT
///     the XDG-derived path) per the decided combo 3A: `ARK_APPNAME`
///     matters only for rung 4 (XDG default lookup), which T-8.0
///     already handles internally.
///   - [`ResolvedScene::Path`] → [`LayoutResolution::SceneDefault`]
///     with the path straight through. Covers both rung 3 (project-
///     local) and rung 4 (XDG default).
///   - [`ResolvedScene::BuiltIn`] → [`LayoutResolution::Legacy`]. The
///     embedded default scene is not materialized to disk by this
///     function; falling through to the legacy `--layout <stem>`
///     path preserves zero-migration behaviour for users who never
///     adopted scenes.
///
/// Reads from the process environment via [`ark_scene::path::resolve_scene_path_from_env`];
/// tests that cover env-var rungs must serialize on
/// [`crate::test_lock::ENV_LOCK`].
pub fn resolve_layout_source(
    config_dir: &Path,
    cwd: &Path,
    explicit_scene: Option<&str>,
) -> LayoutResolution {
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
        explicit_scene,
        env_scene.as_deref(),
        env_appname.as_deref(),
        xdg_config_home.as_deref(),
        cwd,
    ) {
        ark_scene::resolve_path::SceneSource::Flag(path)
        | ark_scene::resolve_path::SceneSource::EnvVar(path) => {
            // Named scenes (via flag or ARK_SCENE) resolve under
            // ctx.config_dir/scenes/ when the value looks like a bare
            // name (no path separators). Otherwise use verbatim.
            if path.components().count() == 1 && !path.to_string_lossy().contains('/') {
                let name = path.to_string_lossy();
                let rooted = config_dir.join("scenes").join(format!("{name}.kdl"));
                LayoutResolution::SceneExplicit { path: rooted }
            } else {
                LayoutResolution::SceneExplicit { path }
            }
        }
        ark_scene::resolve_path::SceneSource::ProjectLocal(path)
        | ark_scene::resolve_path::SceneSource::UserConfig(path) => {
            LayoutResolution::SceneDefault { path }
        }
        ark_scene::resolve_path::SceneSource::BuiltIn => {
            // TODO(T-14.1): materialize the embedded DEFAULT_SCENE_KDL
            // to a per-agent scene file and compile it via the scene
            // pipeline so the "zero-migration" path also benefits from
            // scene-driven rendering. Today we preserve the legacy
            // `--layout <stem>` behaviour so users who never adopted
            // scenes see no change.
            LayoutResolution::Legacy
        }
    }
}
