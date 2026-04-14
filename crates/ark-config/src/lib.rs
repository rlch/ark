//! Figment-based layered config loader for ark.
//!
//! Implements cavekit-config.md R1 (layering precedence). Config is resolved
//! by merging sources in this deterministic order (lowest → highest):
//!
//!   1. Compiled-in defaults (`T::default()`)
//!   2. User config: `$XDG_CONFIG_HOME/ark/config.toml` (silently skipped if
//!      missing) — path derived from [`ark_types::EnvPaths`].
//!   3. Project config: `./.ark/config.toml` (silently skipped if missing)
//!   4. Env vars: `ARK_*` — nested keys flatten with double-underscore
//!      (e.g. `ARK_DIFF__DEBOUNCE_MS=500` → `config.diff.debounce_ms`). See
//!      cavekit-config.md R5.
//!   5. Explicit overrides (highest) — typically CLI flags serialized to a
//!      `serde_json::Value` map.
//!
//! # Missing files skipped
//! Both user and project paths use `Toml::file(path)`; figment's `Toml::file`
//! silently no-ops when the path does not exist (absolute paths just check
//! `is_file()`; relative paths walk up the directory tree). No `.nested()` —
//! ark's TOML schema uses top-level sections as data keys (`[diff]`,
//! `[engine.claude_code]`, ...), not figment profile selectors.
//!
//! # Array semantics (v1)
//! Figment's default merge strategy is **override**, not concatenate.  For v1
//! this means array-valued fields (`Vec<T>`) in later layers fully replace the
//! array from earlier layers rather than concatenating.  This is the standard
//! figment behavior and is what kit R1 is tracking as a future refinement for
//! `[[hooks]]` specifically (see T-021 for hooks-specific concatenation logic).
//!
//! Top-level non-array values — scalars, nested tables, optional fields —
//! merge in the expected "override-only-keys-that-are-set" manner.
//!
//! # Typical usage
//! ```no_run
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize, Serialize, Default)]
//! struct Config {
//!     orchestrator: String,
//! }
//!
//! let cfg: Config = ark_config::load_config().unwrap();
//! ```
//!
//! # Test-friendly path injection
//! [`ConfigLoader`] exposes builder methods so tests (and explicit-path CLI
//! invocations) can bypass env-derived paths without mutating the process
//! environment.

use std::path::PathBuf;

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Serialize, de::DeserializeOwned};

/// Default env-var prefix for ark config overrides.
pub const DEFAULT_ENV_PREFIX: &str = "ARK_";

/// Double-underscore delimiter separating nested keys in env vars.
///
/// Example: `ARK_DIFF__DEBOUNCE_MS` → `diff.debounce_ms`.
pub const ENV_NESTED_SPLIT: &str = "__";

/// Convenience entry point — loads `T` using env-derived user / project paths
/// and the `ARK_` env prefix.
///
/// Equivalent to:
///
/// ```ignore
/// ConfigLoader::new()
///     .with_user_path(default_user_path())
///     .with_project_path(default_project_path())
///     .with_env_prefix("ARK_")
///     .load::<T>()
/// ```
///
/// # Errors
/// - Propagates any `figment::Error` from deserialization, malformed TOML,
///   or env parsing.
/// - Silent on a missing `HOME` / XDG env (user path becomes `None`, layer
///   is simply skipped).
pub fn load_config<T>() -> Result<T, figment::Error>
where
    T: DeserializeOwned + Default + Serialize,
{
    ConfigLoader::new()
        .with_user_path(default_user_path())
        .with_project_path(default_project_path())
        .with_env_prefix(DEFAULT_ENV_PREFIX)
        .load::<T>()
}

/// Resolve `$XDG_CONFIG_HOME/ark/config.toml` (or platform fallback).
/// Returns `None` if `HOME` and XDG are both unset — layer is skipped.
pub fn default_user_path() -> Option<PathBuf> {
    ark_types::EnvPaths::resolve()
        .ok()
        .map(|layout| layout.config().join("config.toml"))
}

/// Project-local config path: `./.ark/config.toml`, resolved against the
/// current working directory at call time.
pub fn default_project_path() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".ark").join("config.toml"))
}

/// Builder-style layered config loader. Use when tests / explicit CLI paths
/// want to inject user / project paths rather than derive them from the env.
///
/// Precedence (lowest → highest) when [`load`][Self::load] is called:
///
/// 1. `T::default()` serialized via [`Serialized::defaults`]
/// 2. User TOML file (skipped if `None` or missing)
/// 3. Project TOML file (skipped if `None` or missing)
/// 4. Env vars under configured prefix (skipped if prefix is `None`)
/// 5. Explicit overrides (skipped if `None`)
pub struct ConfigLoader {
    user_path: Option<PathBuf>,
    project_path: Option<PathBuf>,
    env_prefix: Option<String>,
    overrides: Option<serde_json::Value>,
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigLoader {
    /// Empty loader — no user / project paths, no env prefix, no overrides.
    /// Calling [`load`][Self::load] returns `T::default()` unchanged.
    pub fn new() -> Self {
        Self {
            user_path: None,
            project_path: None,
            env_prefix: None,
            overrides: None,
        }
    }

    /// Set the user-level TOML path (typically `$XDG_CONFIG_HOME/ark/config.toml`).
    /// Pass `None` to disable the user layer entirely.
    pub fn with_user_path(mut self, path: Option<PathBuf>) -> Self {
        self.user_path = path;
        self
    }

    /// Set the project-level TOML path (typically `./.ark/config.toml`).
    /// Pass `None` to disable the project layer entirely.
    pub fn with_project_path(mut self, path: Option<PathBuf>) -> Self {
        self.project_path = path;
        self
    }

    /// Enable the env layer with the given prefix, e.g. `"ARK_"`. Nested keys
    /// are split by [`ENV_NESTED_SPLIT`] (double-underscore).
    pub fn with_env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Inject explicit overrides — typically the CLI flag map, serialized to
    /// `serde_json::Value`. Wins over every other layer.
    pub fn with_overrides(mut self, overrides: serde_json::Value) -> Self {
        self.overrides = Some(overrides);
        self
    }

    /// Materialize `T` by merging all configured layers.
    ///
    /// # Errors
    /// Returns any `figment::Error` produced by malformed TOML, invalid env
    /// values, or deserialization into `T`.
    pub fn load<T>(&self) -> Result<T, figment::Error>
    where
        T: DeserializeOwned + Default + Serialize,
    {
        let mut fig = Figment::new().merge(Serialized::defaults(T::default()));

        if let Some(path) = self.user_path.as_ref() {
            fig = fig.merge(Toml::file(path));
        }

        if let Some(path) = self.project_path.as_ref() {
            fig = fig.merge(Toml::file(path));
        }

        if let Some(prefix) = self.env_prefix.as_ref() {
            fig = fig.merge(Env::prefixed(prefix).split(ENV_NESTED_SPLIT));
        }

        if let Some(overrides) = self.overrides.as_ref() {
            fig = fig.merge(Serialized::defaults(overrides));
        }

        fig.extract::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Jail;
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct DiffConfig {
        command: String,
        debounce_ms: u64,
    }

    impl Default for DiffConfig {
        fn default() -> Self {
            Self {
                command: "delta".into(),
                debounce_ms: 300,
            }
        }
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct TestConfig {
        orchestrator: String,
        stall_timeout_secs: u64,
        tags: Vec<String>,
        diff: DiffConfig,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                orchestrator: "auto".into(),
                stall_timeout_secs: 120,
                tags: vec!["default".into()],
                diff: DiffConfig::default(),
            }
        }
    }

    fn loader_for(jail: &Jail, user: Option<&str>, project: Option<&str>) -> ConfigLoader {
        let mut l = ConfigLoader::new();
        if let Some(name) = user {
            l = l.with_user_path(Some(jail.directory().join(name)));
        }
        if let Some(name) = project {
            l = l.with_project_path(Some(jail.directory().join(name)));
        }
        l
    }

    #[test]
    fn defaults_applied_when_nothing_else_set() {
        let cfg: TestConfig = ConfigLoader::new().load().expect("load defaults");
        assert_eq!(cfg, TestConfig::default());
    }

    #[test]
    fn user_file_overrides_defaults() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "user.toml",
                r#"
                orchestrator = "cavekit"
                "#,
            )?;
            let cfg: TestConfig = loader_for(jail, Some("user.toml"), None)
                .load()
                .expect("load user");
            assert_eq!(cfg.orchestrator, "cavekit");
            // unaffected fields keep defaults
            assert_eq!(cfg.stall_timeout_secs, 120);
            Ok(())
        });
    }

    #[test]
    fn project_file_overrides_user_file() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "user.toml",
                r#"
                orchestrator = "cavekit"
                stall_timeout_secs = 500
                "#,
            )?;
            jail.create_file(
                "project.toml",
                r#"
                orchestrator = "claude-code"
                "#,
            )?;
            let cfg: TestConfig = loader_for(jail, Some("user.toml"), Some("project.toml"))
                .load()
                .expect("load project");
            // project wins
            assert_eq!(cfg.orchestrator, "claude-code");
            // user value falls through where project didn't set
            assert_eq!(cfg.stall_timeout_secs, 500);
            Ok(())
        });
    }

    #[test]
    fn missing_files_do_not_error() {
        let loader = ConfigLoader::new()
            .with_user_path(Some(PathBuf::from("/nonexistent/user.toml")))
            .with_project_path(Some(PathBuf::from("/nonexistent/project.toml")));
        let cfg: TestConfig = loader.load().expect("missing files skip cleanly");
        assert_eq!(cfg, TestConfig::default());
    }

    #[test]
    fn overrides_win_over_every_other_layer() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "project.toml",
                r#"
                orchestrator = "from-project"
                "#,
            )?;
            jail.set_env("ARKTEST_ORCHESTRATOR", "from-env");

            let overrides = serde_json::json!({
                "orchestrator": "from-flags",
            });

            let cfg: TestConfig = ConfigLoader::new()
                .with_project_path(Some(jail.directory().join("project.toml")))
                .with_env_prefix("ARKTEST_")
                .with_overrides(overrides)
                .load()
                .expect("load with overrides");

            assert_eq!(cfg.orchestrator, "from-flags");
            Ok(())
        });
    }

    #[test]
    fn env_overrides_project_and_user() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "user.toml",
                r#"
                orchestrator = "from-user"
                "#,
            )?;
            jail.create_file(
                "project.toml",
                r#"
                orchestrator = "from-project"
                "#,
            )?;
            jail.set_env("ARKTEST_ORCHESTRATOR", "from-env");

            let cfg: TestConfig = loader_for(jail, Some("user.toml"), Some("project.toml"))
                .with_env_prefix("ARKTEST_")
                .load()
                .expect("load with env");

            assert_eq!(cfg.orchestrator, "from-env");
            Ok(())
        });
    }

    #[test]
    fn nested_env_double_underscore_splits_keys() {
        Jail::expect_with(|jail| {
            jail.set_env("ARKTEST_DIFF__DEBOUNCE_MS", "500");
            jail.set_env("ARKTEST_DIFF__COMMAND", "custom-delta");

            let cfg: TestConfig = ConfigLoader::new()
                .with_env_prefix("ARKTEST_")
                .load()
                .expect("load with nested env");

            assert_eq!(cfg.diff.debounce_ms, 500);
            assert_eq!(cfg.diff.command, "custom-delta");
            // sibling scalar untouched
            assert_eq!(cfg.orchestrator, "auto");
            Ok(())
        });
    }

    #[test]
    fn malformed_toml_errors_clearly() {
        Jail::expect_with(|jail| {
            jail.create_file("bad.toml", "orchestrator = \n")?;
            let err = loader_for(jail, Some("bad.toml"), None)
                .load::<TestConfig>()
                .expect_err("malformed TOML should error");
            // sanity: figment wraps the parse error — just ensure it's surfaced.
            let msg = format!("{err}");
            assert!(!msg.is_empty());
            Ok(())
        });
    }

    #[test]
    fn array_field_replaced_by_later_layer() {
        // v1 behavior documented in module docs: arrays replace, not concat.
        Jail::expect_with(|jail| {
            jail.create_file(
                "user.toml",
                r#"
                tags = ["from-user-1", "from-user-2"]
                "#,
            )?;
            jail.create_file(
                "project.toml",
                r#"
                tags = ["from-project"]
                "#,
            )?;
            let cfg: TestConfig = loader_for(jail, Some("user.toml"), Some("project.toml"))
                .load()
                .expect("load arrays");
            assert_eq!(cfg.tags, vec!["from-project".to_string()]);
            Ok(())
        });
    }

    #[test]
    fn default_project_path_points_at_dot_ark_config_toml() {
        let path = default_project_path().expect("cwd resolvable");
        assert!(path.ends_with(".ark/config.toml"));
    }

    #[test]
    fn new_is_same_as_default() {
        // Trivial — exercise the Default impl so it doesn't rot unused.
        let a: TestConfig = ConfigLoader::new().load().unwrap();
        let b: TestConfig = ConfigLoader::default().load().unwrap();
        assert_eq!(a, b);
    }
}
