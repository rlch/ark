//! T-113: Scene path resolver — pure function determining which scene file to load.
//!
//! Precedence:
//! 1. `--scene` CLI flag
//! 2. `ARK_SCENE` environment variable
//! 3. Project-local `.ark/scene.kdl` (relative to `cwd`)
//! 4. `$XDG_CONFIG_HOME/<appname>/scenes/default.kdl`
//! 5. Built-in default (T-110)

use std::path::{Path, PathBuf};

/// Where the resolved scene comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneSource {
    /// Explicitly passed via `--scene <path>`.
    Flag(PathBuf),
    /// Read from the `ARK_SCENE` environment variable.
    EnvVar(PathBuf),
    /// Found at `.ark/scene.kdl` relative to the working directory.
    ProjectLocal(PathBuf),
    /// Found under the user's XDG config directory.
    UserConfig(PathBuf),
    /// No external scene found; use the compiled-in default.
    BuiltIn,
}

/// Resolve which scene file to load.
///
/// This is a **pure function** — it never reads the environment or filesystem
/// itself (except for `Path::exists` checks on the two discovery paths).
/// All inputs are passed explicitly so the caller controls caching and
/// testing.
pub fn resolve_scene_path(
    flag: Option<&str>,
    env_scene: Option<&str>,
    env_appname: Option<&str>,
    xdg_config_home: Option<&Path>,
    cwd: &Path,
) -> SceneSource {
    // 1. --scene flag
    if let Some(path) = flag {
        return SceneSource::Flag(PathBuf::from(path));
    }

    // 2. ARK_SCENE env
    if let Some(path) = env_scene {
        return SceneSource::EnvVar(PathBuf::from(path));
    }

    // 3. ./.ark/scene.kdl
    let project = cwd.join(".ark/scene.kdl");
    if project.exists() {
        return SceneSource::ProjectLocal(project);
    }

    // 4. XDG config
    let appname = env_appname.unwrap_or("ark");
    if let Some(config) = xdg_config_home {
        let user = config.join(format!("{appname}/scenes/default.kdl"));
        if user.exists() {
            return SceneSource::UserConfig(user);
        }
    }

    // 5. Built-in
    SceneSource::BuiltIn
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a file (and any parent dirs) at the given path.
    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "// placeholder").unwrap();
    }

    #[test]
    fn flag_takes_precedence_over_everything() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        // Even when project-local exists, the flag wins.
        touch(&cwd.join(".ark/scene.kdl"));

        let result = resolve_scene_path(
            Some("/explicit/scene.kdl"),
            Some("/env/scene.kdl"),
            None,
            None,
            cwd,
        );
        assert_eq!(
            result,
            SceneSource::Flag(PathBuf::from("/explicit/scene.kdl"))
        );
    }

    #[test]
    fn env_var_takes_precedence_over_project_local() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        touch(&cwd.join(".ark/scene.kdl"));

        let result = resolve_scene_path(None, Some("/env/scene.kdl"), None, None, cwd);
        assert_eq!(result, SceneSource::EnvVar(PathBuf::from("/env/scene.kdl")));
    }

    #[test]
    fn project_local_ark_scene_kdl_found() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();

        touch(&cwd.join(".ark/scene.kdl"));

        let result = resolve_scene_path(None, None, None, None, cwd);
        assert_eq!(
            result,
            SceneSource::ProjectLocal(cwd.join(".ark/scene.kdl"))
        );
    }

    #[test]
    fn user_config_found() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg_config");

        touch(&xdg.join("ark/scenes/default.kdl"));

        let result = resolve_scene_path(None, None, None, Some(&xdg), tmp.path());
        assert_eq!(
            result,
            SceneSource::UserConfig(xdg.join("ark/scenes/default.kdl"))
        );
    }

    #[test]
    fn falls_back_to_built_in() {
        let tmp = TempDir::new().unwrap();
        let result = resolve_scene_path(None, None, None, None, tmp.path());
        assert_eq!(result, SceneSource::BuiltIn);
    }

    #[test]
    fn custom_appname_used_in_config_path() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg_config");

        touch(&xdg.join("myapp/scenes/default.kdl"));

        let result = resolve_scene_path(None, None, Some("myapp"), Some(&xdg), tmp.path());
        assert_eq!(
            result,
            SceneSource::UserConfig(xdg.join("myapp/scenes/default.kdl"))
        );
    }
}
