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

pub mod ext_sections;
pub mod hooks;
pub mod schema;

pub use ext_sections::{
    ExtConfigError, extract_section, section_key, validate_all_extensions, validate_ext_sections,
};
pub use hooks::{HookContext, HookEntry};
pub use schema::{
    Config, DefaultsSection, DiffSection,
    EngineClaudeCodeSection, EngineLaunchSpec, EngineSection, MuxSection, MuxZellijSection,
    OrchestratorCavekitSection, OrchestratorClaudeCodeSection, OrchestratorSection,
};

/// Default env-var prefix for ark config overrides.
pub const DEFAULT_ENV_PREFIX: &str = "ARK_";

/// Double-underscore delimiter separating nested keys in env vars.
///
/// Example: `ARK_DIFF__DEBOUNCE_MS` → `diff.debounce_ms`.
pub const ENV_NESTED_SPLIT: &str = "__";

/// User-facing shipped template config.  Embedded at compile time via
/// [`include_str!`]; `ark doctor --fix` uses this when writing the initial
/// `$XDG_CONFIG_HOME/ark/config.toml`.  See cavekit-config.md R3/R4/R5.
pub const TEMPLATE_CONFIG_TOML: &str = include_str!("../templates/config.toml");

/// Documentation blob for `ARK_*` env-var shortcuts — rendered by
/// `ark config show --help` / README / `ark doctor` output.
///
/// Covers cavekit-config.md R5:
/// - `ARK_*__*` double-underscore flattening for arbitrary nested keys
/// - convenience shortcuts for the common toggles
/// - the v1 limitation that arrays are unsupported via env.
pub const ENV_SHORTCUTS_DOC: &str = "\
ARK_* environment variables override config.toml keys.

Nested keys flatten with double underscore:
  ARK_DEFAULTS__ENGINE=claude-code         -> defaults.engine
  ARK_DIFF__DEBOUNCE_MS=500                -> diff.debounce_ms
  ARK_ENGINE__CLAUDE_CODE__TRANSCRIPT_TAIL=false

Convenience shortcuts (expanded to their canonical key by the loader):
  ARK_ORCHESTRATOR   -> defaults.orchestrator
  ARK_ENGINE         -> defaults.engine
  ARK_LOG            -> tracing log filter (consumed by logging subsystem)
  ARK_CONFIG_PATH    -> override for user config file
  ARK_STATE_DIR      -> override for state directory
  ARK_CONFIG_DIR     -> override for config directory
  ARK_RUNTIME_DIR    -> override for runtime directory

Limitations:
  * Arrays cannot be set via env vars (figment Env is scalar-only). Use
    config.toml for list-valued fields like `engine.claude_code.inject_hooks`
    or `[[hooks]]`.
";

/// Env var names consumed by the path resolver / logging subsystem rather
/// than the TOML layer — documented so the env layer can skip them when
/// figment asks "which keys are mine?".
pub const RESERVED_ENV_VARS: &[&str] = &[
    "ARK_LOG",
    "ARK_CONFIG_PATH",
    "ARK_STATE_DIR",
    "ARK_CONFIG_DIR",
    "ARK_RUNTIME_DIR",
];

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
            // Shortcut promotion (cavekit-config.md R5) is scoped to the
            // canonical `ARK_` prefix so legacy callers / tests using a
            // different prefix (e.g. `ARKTEST_`) keep pure double-underscore
            // semantics.
            let apply_shortcuts = prefix == DEFAULT_ENV_PREFIX;

            // Env layer A: the general double-underscore-nested keys.
            // When shortcuts are active we filter their bare suffixes out so
            // they don't collide as top-level config keys (and so reserved
            // names like ARK_LOG don't surface as unknown fields).
            fig = fig.merge(
                Env::prefixed(prefix)
                    .filter(move |key| {
                        if !apply_shortcuts {
                            return true;
                        }
                        let upper = key.as_str().to_uppercase();
                        !is_reserved_env_suffix(&upper) && !is_shortcut_env_suffix(&upper)
                    })
                    .split(ENV_NESTED_SPLIT),
            );

            // Env layer B: the explicit shortcuts (`ARK_ORCHESTRATOR`,
            // `ARK_ENGINE`) mapped into their canonical nested keys.
            if apply_shortcuts && let Some(overrides) = collect_shortcut_overrides(prefix) {
                fig = fig.merge(Serialized::defaults(overrides));
            }
        }

        if let Some(overrides) = self.overrides.as_ref() {
            fig = fig.merge(Serialized::defaults(overrides));
        }

        fig.extract::<T>()
    }
}

/// Suffixes (prefix already stripped + uppercased) that are consumed by the
/// path resolver / logging subsystem, not the TOML layer.
fn is_reserved_env_suffix(suffix_upper: &str) -> bool {
    matches!(
        suffix_upper,
        "LOG" | "CONFIG_PATH" | "STATE_DIR" | "CONFIG_DIR" | "RUNTIME_DIR"
    )
}

/// Suffixes that are promoted to nested config keys via
/// [`collect_shortcut_overrides`].
fn is_shortcut_env_suffix(suffix_upper: &str) -> bool {
    matches!(suffix_upper, "ORCHESTRATOR" | "ENGINE")
}

/// Read the shortcut env vars and serialize them into the canonical nested
/// structure.  Returns `None` when no shortcut is set so the layer is simply
/// skipped.
///
/// Currently handled:
/// - `{prefix}ORCHESTRATOR` -> `defaults.orchestrator`
/// - `{prefix}ENGINE`       -> `defaults.engine`
fn collect_shortcut_overrides(prefix: &str) -> Option<serde_json::Value> {
    let orch = std::env::var(format!("{prefix}ORCHESTRATOR")).ok();
    let eng = std::env::var(format!("{prefix}ENGINE")).ok();
    if orch.is_none() && eng.is_none() {
        return None;
    }
    let mut defaults = serde_json::Map::new();
    if let Some(v) = orch {
        defaults.insert("orchestrator".into(), serde_json::Value::String(v));
    }
    if let Some(v) = eng {
        defaults.insert("engine".into(), serde_json::Value::String(v));
    }
    let mut root = serde_json::Map::new();
    root.insert("defaults".into(), serde_json::Value::Object(defaults));
    Some(serde_json::Value::Object(root))
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

    // -----------------------------------------------------------------
    // Full `Config` integration — schema + env shortcuts + template.
    // -----------------------------------------------------------------

    #[test]
    fn canonical_config_loads_defaults_from_empty_loader() {
        let cfg: Config = ConfigLoader::new().load().expect("defaults load");
        assert_eq!(cfg, Config::defaults());
    }

    #[test]
    fn ark_defaults_engine_via_double_underscore() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_DEFAULTS__ENGINE", "claude-code");
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("env load");
            assert_eq!(cfg.defaults.engine, "claude-code");
            // sibling untouched
            assert_eq!(cfg.defaults.orchestrator, "auto");
            Ok(())
        });
    }

    #[test]
    fn ark_orchestrator_shortcut_maps_to_defaults_orchestrator() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_ORCHESTRATOR", "cavekit");
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("env shortcut load");
            assert_eq!(cfg.defaults.orchestrator, "cavekit");
            Ok(())
        });
    }

    #[test]
    fn ark_engine_shortcut_maps_to_defaults_engine() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_ENGINE", "some-future-engine");
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("engine shortcut");
            assert_eq!(cfg.defaults.engine, "some-future-engine");
            Ok(())
        });
    }

    #[test]
    fn ark_log_reserved_does_not_become_config_key() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_LOG", "debug");
            // Must not error even though `log` is not a field on Config.
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("ARK_LOG must be ignored by config layer");
            assert_eq!(cfg, Config::defaults());
            Ok(())
        });
    }

    #[test]
    fn ark_nested_engine_claude_code_transcript_tail() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_ENGINE__CLAUDE_CODE__TRANSCRIPT_TAIL", "false");
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("nested env");
            assert!(!cfg.engine.claude_code.transcript_tail);
            Ok(())
        });
    }

    #[test]
    fn shipped_template_parses_as_default_config() {
        // All keys are commented out so the template parses as defaults.
        Jail::expect_with(|jail| {
            jail.create_file("ark.toml", TEMPLATE_CONFIG_TOML)?;
            let cfg: Config = Figment::new()
                .merge(Toml::file(jail.directory().join("ark.toml")))
                .extract()
                .expect("template config must parse");
            assert_eq!(cfg, Config::defaults());
            Ok(())
        });
    }

    #[test]
    fn template_documents_every_section_and_env_shortcuts() {
        for section in [
            "[defaults]",
            "[diff]",
            "[engine.claude_code]",
            "[orchestrator.cavekit]",
            "[orchestrator.claude_code]",
            "[mux.zellij]",
            "[[hooks]]",
            "[engines.claude]",
            "[engines.codex]",
            "[engines.gemini-cli]",
        ] {
            assert!(
                TEMPLATE_CONFIG_TOML.contains(section),
                "template missing section header {section}"
            );
        }
        // R5 documentation
        assert!(TEMPLATE_CONFIG_TOML.contains("ARK_ORCHESTRATOR"));
        assert!(TEMPLATE_CONFIG_TOML.contains("ARK_ENGINE"));
        assert!(TEMPLATE_CONFIG_TOML.contains("ARK_LOG"));
        assert!(TEMPLATE_CONFIG_TOML.contains("double underscore"));
    }

    #[test]
    fn env_shortcuts_doc_covers_kit_r5() {
        for piece in [
            "ARK_ORCHESTRATOR",
            "ARK_ENGINE",
            "ARK_LOG",
            "ARK_STATE_DIR",
            "double underscore",
            "Arrays",
        ] {
            assert!(
                ENV_SHORTCUTS_DOC.contains(piece),
                "ENV_SHORTCUTS_DOC missing {piece}"
            );
        }
    }

    #[test]
    fn reserved_env_vars_listed_for_discovery() {
        for name in ["ARK_LOG", "ARK_CONFIG_PATH", "ARK_STATE_DIR"] {
            assert!(RESERVED_ENV_VARS.contains(&name), "missing {name}");
        }
    }

    // -----------------------------------------------------------------
    // T-119 (cavekit-testing R3): coverage for kit R1 layering +
    // R5 env precedence that existing tests don't exercise end-to-end.
    // -----------------------------------------------------------------

    /// Full 4-layer precedence chain on a single scalar field, so a
    /// regression in any single layer boundary is caught here.
    /// Order (lowest → highest): defaults < user < project < env < overrides.
    #[test]
    fn full_layer_chain_highest_wins_per_field() {
        Jail::expect_with(|jail| {
            // user sets field to "from-user"
            jail.create_file("user.toml", r#"orchestrator = "from-user""#)?;
            // project overrides with "from-project"
            jail.create_file("project.toml", r#"orchestrator = "from-project""#)?;
            // env overrides with "from-env"
            jail.set_env("ARKTEST_ORCHESTRATOR", "from-env");

            // Without overrides → env wins
            let cfg: TestConfig = loader_for(jail, Some("user.toml"), Some("project.toml"))
                .with_env_prefix("ARKTEST_")
                .load()
                .expect("load env-wins");
            assert_eq!(cfg.orchestrator, "from-env");

            // With overrides → overrides win
            let cfg: TestConfig = loader_for(jail, Some("user.toml"), Some("project.toml"))
                .with_env_prefix("ARKTEST_")
                .with_overrides(serde_json::json!({
                    "orchestrator": "from-flags",
                }))
                .load()
                .expect("load overrides-wins");
            assert_eq!(cfg.orchestrator, "from-flags");
            Ok(())
        });
    }

    /// Reserved path-resolver env vars (ARK_STATE_DIR, ARK_CONFIG_PATH,
    /// ARK_RUNTIME_DIR, ARK_CONFIG_DIR) must not surface as config
    /// fields when the ARK_ prefix + `deny_unknown_fields` Config type is
    /// used — otherwise figment rejects them as unknown top-level keys.
    #[test]
    fn reserved_path_env_vars_do_not_leak_into_config() {
        Jail::expect_with(|jail| {
            jail.set_env("ARK_STATE_DIR", "/tmp/ark-state");
            jail.set_env("ARK_CONFIG_PATH", "/tmp/ark.toml");
            jail.set_env("ARK_RUNTIME_DIR", "/tmp/ark-run");
            jail.set_env("ARK_CONFIG_DIR", "/tmp/ark-cfg");
            let cfg: Config = ConfigLoader::new()
                .with_env_prefix("ARK_")
                .load()
                .expect("reserved env vars must be filtered out of config layer");
            assert_eq!(cfg, Config::defaults());
            Ok(())
        });
    }

    /// `ARK_ORCHESTRATOR` shortcut must beat a project-level TOML value —
    /// env layer sits above the file layers in the precedence stack.
    #[test]
    fn ark_orchestrator_shortcut_beats_file_values() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "project.toml",
                r#"
                [defaults]
                orchestrator = "from-file"
                "#,
            )?;
            jail.set_env("ARK_ORCHESTRATOR", "from-env");
            let cfg: Config = ConfigLoader::new()
                .with_project_path(Some(jail.directory().join("project.toml")))
                .with_env_prefix("ARK_")
                .load()
                .expect("shortcut precedence");
            assert_eq!(cfg.defaults.orchestrator, "from-env");
            Ok(())
        });
    }

    /// `default_user_path()` should resolve to a concrete path whose
    /// filename is `config.toml` when HOME is set.  Sanity check that
    /// the public helper doesn't silently return None in normal envs.
    #[test]
    fn default_user_path_ends_with_config_toml_when_home_is_set() {
        Jail::expect_with(|jail| {
            // Jail wipes the process env on construction; set only the
            // minimum we need so we go through the canonical HOME/XDG
            // fallback branch in EnvPaths.
            jail.set_env("HOME", jail.directory().to_string_lossy().to_string());
            let path = default_user_path().expect("should resolve via HOME");
            assert!(
                path.file_name().and_then(|s| s.to_str()) == Some("config.toml"),
                "default_user_path leaf must be config.toml, got {path:?}"
            );
            Ok(())
        });
    }

    /// An unknown top-level TOML key must produce an error whose Display
    /// message names the offending key — otherwise `ark doctor` users
    /// can't locate the typo in their config file.
    #[test]
    fn unknown_key_error_mentions_key_name() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "bad.toml",
                r#"
                mystery_flag = "oops"
                "#,
            )?;
            let err = ConfigLoader::new()
                .with_project_path(Some(jail.directory().join("bad.toml")))
                .load::<Config>()
                .expect_err("unknown top-level key must error");
            let msg = format!("{err}");
            assert!(
                msg.contains("mystery_flag"),
                "error must mention offending key; got: {msg}"
            );
            Ok(())
        });
    }

    /// Unknown key inside a `[[hooks]]` entry should be rejected by the
    /// HookEntry deny_unknown_fields attribute when loaded through the
    /// full Config type.
    #[test]
    fn unknown_key_inside_hooks_entry_rejected_via_config() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [[hooks]]
                cmd = "true"
                bogus_hook_field = "nope"
                "#,
            )?;
            let res: Result<Config, _> = ConfigLoader::new()
                .with_project_path(Some(jail.directory().join("c.toml")))
                .load();
            assert!(
                res.is_err(),
                "unknown field inside [[hooks]] must error; got {res:?}"
            );
            Ok(())
        });
    }

    /// Hooks array parses end-to-end via the Config loader and fields
    /// round-trip (exercises the HookEntry integration with Config).
    #[test]
    fn hooks_array_parses_end_to_end_via_config() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [[hooks]]
                cmd = "notify-send 'ark: {{name}} done'"
                on_event = ["done"]

                [[hooks]]
                cmd_argv = ["say", "agent stalled"]
                on_event = ["stall"]
                on_orchestrator = ["cavekit"]
                "#,
            )?;
            let cfg: Config = ConfigLoader::new()
                .with_project_path(Some(jail.directory().join("c.toml")))
                .load()
                .expect("hooks parse");
            assert_eq!(cfg.hooks.len(), 2);
            assert_eq!(cfg.hooks[0].on_event, vec!["done".to_string()]);
            assert_eq!(
                cfg.hooks[1].cmd_argv,
                vec!["say".to_string(), "agent stalled".to_string()]
            );
            assert_eq!(cfg.hooks[1].on_orchestrator, vec!["cavekit".to_string()]);
            Ok(())
        });
    }
}
