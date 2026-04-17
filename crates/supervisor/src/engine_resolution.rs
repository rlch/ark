//! Engine resolution — precedence chain for the ACP engine launch spec.
//!
//! Implements the R17 engine-resolution rungs (cavekit-scene.md R17):
//!
//! | Rung | Source                                         | Ships in |
//! |------|------------------------------------------------|----------|
//! | 1    | `--engine NAME` CLI flag                        | T-ACP.4a |
//! | 2    | Scene `engine { }` block                        | T-ACP.4a |
//! | 3    | Extension-declared engine (`use "engine-*"`)    | T-ACP.4b |
//! | 4    | `[engines.<name>]` in `config.toml`             | T-ACP.4a |
//! | 5    | Hardcoded default: `claude --acp`               | T-ACP.4a |
//!
//! First match wins. [`resolve_engine`] is a pure function whose inputs
//! are the CLI flag, the compiled `SceneDoc`, the loaded `Config`, and
//! the resolved extension `use`s (T-ACP.4b). It returns an
//! [`EngineLaunch`] the supervisor hands to [`acp_client::AcpClient::spawn`].
//!
//! # Intra-scene mutual exclusion (T-ACP.4b)
//!
//! Per R17: a scene may NOT declare BOTH an inline `engine { }` block
//! AND a `use "engine-*"` extension with an engine capability. That
//! pre-condition is enforced by the scene compile pipeline (see
//! [`ark_scene::error::SceneError::EngineConflict`]); this module's
//! rung-3 walker short-circuits on a conflict signal so the
//! supervisor surfaces the same diagnostic regardless of which layer
//! detected it first.
//!
//! # Why `SupervisorError` here
//!
//! The resolver returns a thin [`SupervisorError`] surface so CLI +
//! supervisor callers get one actionable message ("unknown engine
//! `claud-code` in --engine flag — did you mean `claude-code`?") without
//! having to re-thread `anyhow` error chains through the orchestration
//! boot sequence. Callers that want rich miette diagnostics compile
//! the underlying scene error separately — engine resolution itself
//! is a small, typed surface.

use std::collections::BTreeMap;

use ark_config::schema::{Config, EngineLaunchSpec};
use ark_scene::parse::SceneIR;

use crate::factory::SupervisorError;

/// V3 engine launch descriptor.
///
/// V3 migration: the v2 `ark_scene::engine::EngineLaunch` carried
/// `name`, `command`, `args`, `env`. The v3 compat shim in
/// `engine_compat::EngineLaunch` has `command`, `args`,
/// `extension_name` — a different shape. The supervisor's own
/// `EngineLaunch` retains the v2 fields since they are consumed by
/// `acp_client::AcpClient::spawn` which hasn't migrated.
#[derive(Debug, Clone)]
pub struct EngineLaunch {
    /// Engine name (e.g. `"claude"`).
    pub name: String,
    /// Executable name or path (`argv[0]`).
    pub command: String,
    /// Arguments passed to the subprocess (`argv[1..]`).
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: BTreeMap<String, String>,
}

/// Name convention for extension-declared engine cartridges.
///
/// T-ACP.4b: extensions named `engine-<slug>` (e.g. `engine-claude`,
/// `engine-codex`) are the canonical shape for the rung-3 engine.
/// Used by the mutual-exclusion check + the rung-3 walker.
pub const ENGINE_EXTENSION_PREFIX: &str = "engine-";

/// Hardcoded default engine name when no earlier rung matches.
///
/// R17: the shipped default is `claude --acp`. Rung 5 runs when every
/// other rung has missed, so the supervisor still launches a working
/// engine for a scene that declared neither an inline `engine { }` nor
/// a `use "engine-*"`, with no user-overridden `[engines.claude]`.
pub const DEFAULT_ENGINE_NAME: &str = "claude";

/// Build the hardcoded default [`EngineLaunch`] per rung 5.
///
/// Returned as a free function (rather than a constant) because
/// `EngineLaunch` holds a `Vec<String>` + `BTreeMap`, neither of which
/// are `const`-constructible in the workspace's MSRV.
pub fn default_engine_launch() -> EngineLaunch {
    EngineLaunch {
        name: DEFAULT_ENGINE_NAME.to_string(),
        command: "claude".to_string(),
        args: vec!["--acp".to_string()],
        env: BTreeMap::new(),
    }
}

/// Convert a config-crate [`EngineLaunchSpec`] to a scene-crate
/// [`EngineLaunch`]. `name` comes from the config map key (e.g.
/// `"claude"` for `[engines.claude]`).
fn launch_from_config_spec(name: &str, spec: &EngineLaunchSpec) -> EngineLaunch {
    EngineLaunch {
        name: name.to_string(),
        command: spec.command.clone(),
        args: spec.args.clone(),
        env: spec.env.clone(),
    }
}

/// Resolve the engine launch spec for the given runtime inputs.
///
/// Walks rungs 1 → 5 in order; returns the first match. On `--engine`
/// flag with no matching `[engines.<name>]`, returns
/// [`SupervisorError::UnknownEngine`]. Rung 3 (extension-declared
/// engine) is wired through `resolved_uses` — callers that haven't
/// yet resolved the scene's `use`s pass an empty slice, which skips
/// rung 3. The rung-3 lookup also takes over as the mutual-exclusion
/// gate when a scene contains BOTH an inline `engine { }` block AND a
/// `use "engine-*"` with engine capability (see
/// [`ark_scene::error::SceneError::EngineConflict`]).
///
/// The resolver does NOT inspect `config.engines` for built-in
/// defaults — those are layered in by the config loader's shipped
/// template or the `ark-config` defaults. A user who never writes an
/// `[engines.claude]` falls through rung 4 into rung 5's hardcoded
/// default, producing the same `claude --acp` invocation either way.
pub fn resolve_engine(
    flag: Option<&str>,
    _scene: &SceneIR,
    config: &Config,
    _resolved_uses: &[()],
) -> Result<EngineLaunch, SupervisorError> {
    // V3 migration: rungs 2 (inline engine block) and 3 (extension-
    // declared engine) are deferred — v3's SceneNode doesn't have an
    // `engine { }` child and `MergedUse` is no longer available.
    // The supervisor falls through to rungs 1/4/5 which cover the
    // common --engine flag and config.toml paths.
    //
    // TODO(v3-engine-resolution): wire rung 2 via v3 scene extensions
    // and rung 3 via the extension registry when the full engine
    // migration lands.

    // -- Rung 1: --engine NAME --------------------------------------
    if let Some(name) = flag {
        let name = name.trim();
        if name.is_empty() {
            return Err(SupervisorError::UnknownEngine {
                name: String::new(),
                known: engine_names_in_config(config),
            });
        }
        if let Some(spec) = config.engines.get(name) {
            return Ok(launch_from_config_spec(name, spec));
        }
        if let Some(shipped) = shipped_engine(name) {
            return Ok(shipped);
        }
        return Err(SupervisorError::UnknownEngine {
            name: name.to_string(),
            known: engine_names_in_config(config),
        });
    }

    // -- Rung 4: [engines.<name>] in config.toml --------------------
    let slug = config.defaults.engine.trim();
    if !slug.is_empty() && slug != "auto" {
        if let Some(spec) = config.engines.get(slug) {
            return Ok(launch_from_config_spec(slug, spec));
        }
        if let Some(shipped) = shipped_engine(slug) {
            return Ok(shipped);
        }
    }

    // -- Rung 5: hardcoded default ----------------------------------
    Ok(default_engine_launch())
}

/// Return the shipped launch spec for a well-known engine name, if
/// any. T-ACP.8 pins these to `claude --acp`, `codex --acp`, and
/// `gemini --acp`. They serve as "safety-net" defaults so users who
/// write `--engine claude` without an explicit `[engines.claude]`
/// block in their config still get a working spawn.
pub fn shipped_engine(name: &str) -> Option<EngineLaunch> {
    match name {
        "claude" => Some(EngineLaunch {
            name: "claude".into(),
            command: "claude".into(),
            args: vec!["--acp".into()],
            env: BTreeMap::new(),
        }),
        "codex" => Some(EngineLaunch {
            name: "codex".into(),
            command: "codex".into(),
            args: vec!["--acp".into()],
            env: BTreeMap::new(),
        }),
        "gemini-cli" | "gemini" => Some(EngineLaunch {
            name: "gemini-cli".into(),
            command: "gemini".into(),
            args: vec!["--acp".into()],
            env: BTreeMap::new(),
        }),
        _ => None,
    }
}

/// Collect the engine names declared in `config.engines`, sorted, for
/// error-message enumeration ("known: claude, codex, …").
fn engine_names_in_config(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = config.engines.keys().cloned().collect();
    // Include shipped engines in the hint so `--engine claude` still
    // shows as a valid choice even when no user config declares it.
    for shipped in ["claude", "codex", "gemini-cli"] {
        if !names.iter().any(|n| n == shipped) {
            names.push(shipped.into());
        }
    }
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_config::schema::{Config, EngineLaunchSpec};
    use ark_scene::parse::parse_scene;
    use std::path::PathBuf;

    fn empty_scene() -> SceneIR {
        let src = r#"scene "s" { }"#;
        parse_scene(src, PathBuf::from("test.kdl")).expect("parse empty scene")
    }

    #[test]
    fn rung5_default_when_nothing_set() {
        let scene = empty_scene();
        let cfg = Config::defaults();
        let launch = resolve_engine(None, &scene, &cfg, &[]).expect("resolve");
        assert_eq!(launch.name, "claude");
        assert_eq!(launch.command, "claude");
        assert_eq!(launch.args, vec!["--acp"]);
    }

    // V3 migration: rung 2 test removed (no inline `engine { }` in v3 SceneNode).
    // rung 1 flag test simplified (no scene_with_engine).

    #[test]
    fn rung1_flag_wins_over_config() {
        let scene = empty_scene();
        let mut cfg = Config::defaults();
        cfg.engines.insert(
            "flag-engine".into(),
            EngineLaunchSpec {
                command: "flag-bin".into(),
                args: vec!["--from-flag".into()],
                env: BTreeMap::new(),
            },
        );
        let launch = resolve_engine(Some("flag-engine"), &scene, &cfg, &[]).expect("resolve");
        assert_eq!(launch.command, "flag-bin");
        assert_eq!(launch.args, vec!["--from-flag"]);
    }

    #[test]
    fn rung1_unknown_flag_errors_with_known_list() {
        let scene = empty_scene();
        let cfg = Config::defaults();
        let err = resolve_engine(Some("nonsense"), &scene, &cfg, &[])
            .expect_err("unknown engine");
        match err {
            SupervisorError::UnknownEngine { name, known } => {
                assert_eq!(name, "nonsense");
                assert!(known.contains(&"claude".to_string()));
                assert!(known.contains(&"codex".to_string()));
            }
            other => panic!("expected UnknownEngine, got {other:?}"),
        }
    }

    #[test]
    fn rung1_shipped_shortcut_works_without_config_entry() {
        let scene = empty_scene();
        let cfg = Config::defaults();
        let launch = resolve_engine(Some("codex"), &scene, &cfg, &[]).expect("shipped codex");
        assert_eq!(launch.name, "codex");
        assert_eq!(launch.command, "codex");
        assert_eq!(launch.args, vec!["--acp"]);
    }

    #[test]
    fn rung4_uses_defaults_engine_slug() {
        let scene = empty_scene();
        let mut cfg = Config::defaults();
        cfg.defaults.engine = "my-engine".into();
        cfg.engines.insert(
            "my-engine".into(),
            EngineLaunchSpec {
                command: "my-bin".into(),
                args: vec!["--rung4".into()],
                env: BTreeMap::new(),
            },
        );
        let launch = resolve_engine(None, &scene, &cfg, &[]).expect("resolve");
        assert_eq!(launch.command, "my-bin");
        assert_eq!(launch.args, vec!["--rung4"]);
    }

    #[test]
    fn rung4_auto_slug_falls_through_to_default() {
        let scene = empty_scene();
        let mut cfg = Config::defaults();
        cfg.defaults.engine = "auto".into();
        let launch = resolve_engine(None, &scene, &cfg, &[]).expect("resolve");
        // Falls through to rung 5 default.
        assert_eq!(launch.command, "claude");
    }

    #[test]
    fn default_engine_launch_is_claude_acp() {
        let d = default_engine_launch();
        assert_eq!(d.command, "claude");
        assert_eq!(d.args, vec!["--acp"]);
        assert!(d.env.is_empty());
    }

    #[test]
    fn shipped_engine_covers_claude_codex_gemini() {
        assert!(shipped_engine("claude").is_some());
        assert!(shipped_engine("codex").is_some());
        assert!(shipped_engine("gemini-cli").is_some());
        assert!(shipped_engine("gemini").is_some());
        assert!(shipped_engine("unknown").is_none());
    }

    // V3 migration: rung 2 (inline engine block) and rung 3 (extension-
    // declared engine) tests removed — these v2 features are not yet
    // wired in v3. The remaining tests cover rungs 1, 4, and 5 which
    // are the supervisor's own resolution paths.
}
