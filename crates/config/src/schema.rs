//! TOML schema for the ark configuration file.
//!
//! Implements cavekit-config.md R2 (top-level schema) and R3 (default values).
//! Every section uses `#[serde(deny_unknown_fields)]` so typos surface as
//! deserialization errors rather than being silently dropped — see R2.
//!
//! The shape mirrors the layout described in the kit:
//!
//! ```toml
//! [defaults]              # cross-cutting
//! [diff]                  # diff pane rendering
//! [engine.claude_code]    # engine-specific
//! [orchestrator.cavekit]  # orchestrator-specific
//! [orchestrator.claude_code]
//! [mux.zellij]            # multiplexer-specific
//! [[hooks]]               # repeatable hook definitions
//! ```
//!
//! `Config::defaults()` is the canonical entry point — the same as
//! `Config::default()` but named to match the kit (R3) and the call-site at
//! [`crate::ConfigLoader::load`].

use serde::{Deserialize, Serialize};

use crate::hooks::HookEntry;

// ---------------------------------------------------------------------------
// Public default constants — cavekit-config.md R3.
// Inlined into per-section `Default` impls; exposed here so other crates can
// reference the canonical values without re-deriving them.
// ---------------------------------------------------------------------------

/// Default capacity of the in-memory event bus (events). See cavekit-config.md R3.
pub const DEFAULT_EVENT_BUS_CAPACITY: usize = 256;

/// Default `auto_close_on_done`.
pub const DEFAULT_AUTO_CLOSE_ON_DONE: bool = true;
/// Default `auto_close_on_fail`.
pub const DEFAULT_AUTO_CLOSE_ON_FAIL: bool = false;
/// Default `auto_close_on_kill`.
pub const DEFAULT_AUTO_CLOSE_ON_KILL: bool = true;
/// Default `stall_timeout_secs`.
pub const DEFAULT_STALL_TIMEOUT_SECS: u64 = 120;
/// Default orchestrator slug. `"auto"` lets the supervisor pick.
pub const DEFAULT_ORCHESTRATOR: &str = "auto";
/// Default engine slug.
pub const DEFAULT_ENGINE: &str = "claude-code";
/// Default `session_prefix` used for tab / pane names.
pub const DEFAULT_SESSION_PREFIX: &str = "ark";

/// Default diff renderer command.
pub const DEFAULT_DIFF_COMMAND: &str = "delta --paging=never --side-by-side --line-numbers";
/// Default diff debounce window in milliseconds.
pub const DEFAULT_DIFF_DEBOUNCE_MS: u64 = 300;

/// Default `engine.claude_code.transcript_tail`.
pub const DEFAULT_TRANSCRIPT_TAIL: bool = true;
/// Default `engine.claude_code.permission_policy`.
pub const DEFAULT_PERMISSION_POLICY: &str = "auto_approve_all";
/// Default `engine.claude_code.hook_transport`. `socket` is reserved for v2.
pub const DEFAULT_HOOK_TRANSPORT: &str = "state_file";

/// Default `orchestrator.cavekit.default_layout`.
pub const DEFAULT_CAVEKIT_LAYOUT: &str = "builder";
/// Default `orchestrator.cavekit.review_layout`.
pub const DEFAULT_CAVEKIT_REVIEW_LAYOUT: &str = "review";
/// Default `orchestrator.cavekit.review_on_phase` — phase that triggers review tab.
pub const DEFAULT_CAVEKIT_REVIEW_ON_PHASE: &str = "check";

/// Default `orchestrator.claude_code.default_layout`.
pub const DEFAULT_CLAUDE_CODE_LAYOUT: &str = "classic";

/// Default `mux.zellij.status_plugin_path`.
pub const DEFAULT_ZELLIJ_STATUS_PLUGIN: &str = "~/.config/zellij/plugins/ark-status.wasm";
/// Default `mux.zellij.picker_plugin_path`.
pub const DEFAULT_ZELLIJ_PICKER_PLUGIN: &str = "~/.config/zellij/plugins/ark-picker.wasm";
/// Default `mux.zellij.default_layout_dir`.
pub const DEFAULT_ZELLIJ_LAYOUT_DIR: &str = "~/.config/ark/layouts";

// ---------------------------------------------------------------------------
// Top-level
// ---------------------------------------------------------------------------

/// Canonical, fully-typed ark configuration tree.
///
/// Round-trips through serde / figment.  Every nested section opts into
/// `#[serde(deny_unknown_fields)]` so a typo like `auto_clos_on_done` errors
/// out at load time instead of being silently ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Cross-cutting defaults — orchestrator pick, auto-close behaviour, etc.
    pub defaults: DefaultsSection,
    /// Diff pane rendering knobs.
    pub diff: DiffSection,
    /// Engine-specific configuration.
    pub engine: EngineSection,
    /// Orchestrator-specific configuration.
    pub orchestrator: OrchestratorSection,
    /// Multiplexer-specific configuration.
    pub mux: MuxSection,
    /// Repeatable hook definitions. Empty by default — users opt in.
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Config {
    /// Build the fully-populated default config.
    ///
    /// Same as `Config::default()` but named to match cavekit-config.md R3
    /// ("`Config::defaults()` returns shipped defaults").
    pub fn defaults() -> Self {
        Self {
            defaults: DefaultsSection::default(),
            diff: DiffSection::default(),
            engine: EngineSection::default(),
            orchestrator: OrchestratorSection::default(),
            mux: MuxSection::default(),
            hooks: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// [defaults]
// ---------------------------------------------------------------------------

/// `[defaults]` section — cross-cutting knobs consumed by the supervisor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DefaultsSection {
    /// Default orchestrator slug (`auto | cavekit | claude-code`).
    pub orchestrator: String,
    /// Default engine slug.
    pub engine: String,
    /// Prefix for spawned session / tab names.
    pub session_prefix: String,
    /// Auto-close pane on `done` outcome.
    pub auto_close_on_done: bool,
    /// Auto-close pane on `fail` outcome.
    pub auto_close_on_fail: bool,
    /// Auto-close pane on `kill` outcome.
    pub auto_close_on_kill: bool,
    /// Stall detection window in seconds.
    pub stall_timeout_secs: u64,
    /// In-memory event bus capacity (events). See `DEFAULT_EVENT_BUS_CAPACITY`.
    pub event_bus_capacity: usize,
}

impl Default for DefaultsSection {
    fn default() -> Self {
        Self {
            orchestrator: DEFAULT_ORCHESTRATOR.into(),
            engine: DEFAULT_ENGINE.into(),
            session_prefix: DEFAULT_SESSION_PREFIX.into(),
            auto_close_on_done: DEFAULT_AUTO_CLOSE_ON_DONE,
            auto_close_on_fail: DEFAULT_AUTO_CLOSE_ON_FAIL,
            auto_close_on_kill: DEFAULT_AUTO_CLOSE_ON_KILL,
            stall_timeout_secs: DEFAULT_STALL_TIMEOUT_SECS,
            event_bus_capacity: DEFAULT_EVENT_BUS_CAPACITY,
        }
    }
}

// ---------------------------------------------------------------------------
// [diff]
// ---------------------------------------------------------------------------

/// `[diff]` section — diff pane rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiffSection {
    /// Shell command to render diffs (typically `delta` invocation).
    pub command: String,
    /// Debounce window (ms) before re-rendering on file events.
    pub debounce_ms: u64,
}

impl Default for DiffSection {
    fn default() -> Self {
        Self {
            command: DEFAULT_DIFF_COMMAND.into(),
            debounce_ms: DEFAULT_DIFF_DEBOUNCE_MS,
        }
    }
}

// ---------------------------------------------------------------------------
// [engine.*]
// ---------------------------------------------------------------------------

/// `[engine.*]` parent — currently houses `claude_code`. Add new engines as
/// new fields here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EngineSection {
    pub claude_code: EngineClaudeCodeSection,
}

/// `[engine.claude_code]` — Claude Code engine knobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EngineClaudeCodeSection {
    /// Whether to tail the engine transcript into the supervisor log.
    pub transcript_tail: bool,
    /// Permission gate: `ask | auto_approve_read | auto_approve_all`.
    pub permission_policy: String,
    /// Hook transport: `state_file` (v1) or `socket` (reserved).
    pub hook_transport: String,
    /// Hook event names to inject into Claude Code's hook config.
    pub inject_hooks: Vec<String>,
}

impl Default for EngineClaudeCodeSection {
    fn default() -> Self {
        Self {
            transcript_tail: DEFAULT_TRANSCRIPT_TAIL,
            permission_policy: DEFAULT_PERMISSION_POLICY.into(),
            hook_transport: DEFAULT_HOOK_TRANSPORT.into(),
            inject_hooks: vec![
                "PostToolUse".into(),
                "Stop".into(),
                "PermissionRequest".into(),
                "Notification".into(),
                "TaskCompleted".into(),
                "SessionEnd".into(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// [orchestrator.*]
// ---------------------------------------------------------------------------

/// `[orchestrator.*]` parent — houses `cavekit` + `claude_code` orchestrators.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OrchestratorSection {
    pub cavekit: OrchestratorCavekitSection,
    pub claude_code: OrchestratorClaudeCodeSection,
}

/// `[orchestrator.cavekit]` — Cavekit orchestrator knobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OrchestratorCavekitSection {
    /// Watch the Ralph loop file for progress events.
    pub watch_ralph_loop: bool,
    /// Watch the impl-tracking files for progress events.
    pub watch_impl_tracking: bool,
    /// Spawn a dedicated review tab on phase transition.
    pub spawn_review_tab: bool,
    /// Default Zellij layout name.
    pub default_layout: String,
    /// Layout used when spawning the review tab.
    pub review_layout: String,
    /// Phase whose entry triggers the review tab spawn.
    pub review_on_phase: String,
}

impl Default for OrchestratorCavekitSection {
    fn default() -> Self {
        Self {
            watch_ralph_loop: true,
            watch_impl_tracking: true,
            spawn_review_tab: true,
            default_layout: DEFAULT_CAVEKIT_LAYOUT.into(),
            review_layout: DEFAULT_CAVEKIT_REVIEW_LAYOUT.into(),
            review_on_phase: DEFAULT_CAVEKIT_REVIEW_ON_PHASE.into(),
        }
    }
}

/// `[orchestrator.claude_code]` — Claude Code orchestrator knobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OrchestratorClaudeCodeSection {
    /// Default Zellij layout name.
    pub default_layout: String,
}

impl Default for OrchestratorClaudeCodeSection {
    fn default() -> Self {
        Self {
            default_layout: DEFAULT_CLAUDE_CODE_LAYOUT.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// [mux.*]
// ---------------------------------------------------------------------------

/// `[mux.*]` parent — currently houses `zellij`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MuxSection {
    pub zellij: MuxZellijSection,
}

/// `[mux.zellij]` — Zellij multiplexer knobs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MuxZellijSection {
    /// Path to the Zellij status-bar plugin wasm.
    pub status_plugin_path: String,
    /// Path to the Zellij picker plugin wasm.
    pub picker_plugin_path: String,
    /// Directory containing layout KDL files.
    pub default_layout_dir: String,
}

impl Default for MuxZellijSection {
    fn default() -> Self {
        Self {
            status_plugin_path: DEFAULT_ZELLIJ_STATUS_PLUGIN.into(),
            picker_plugin_path: DEFAULT_ZELLIJ_PICKER_PLUGIN.into(),
            default_layout_dir: DEFAULT_ZELLIJ_LAYOUT_DIR.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use figment::{
        Figment, Jail,
        providers::{Format, Toml},
    };

    #[test]
    fn defaults_match_kit_r3() {
        let cfg = Config::defaults();
        assert_eq!(cfg.defaults.orchestrator, "auto");
        assert_eq!(cfg.defaults.engine, "claude-code");
        assert_eq!(cfg.defaults.session_prefix, "ark");
        assert!(cfg.defaults.auto_close_on_done);
        assert!(!cfg.defaults.auto_close_on_fail);
        assert!(cfg.defaults.auto_close_on_kill);
        assert_eq!(cfg.defaults.stall_timeout_secs, 120);
        assert_eq!(cfg.defaults.event_bus_capacity, 256);

        assert_eq!(cfg.diff.debounce_ms, 300);
        assert!(cfg.diff.command.contains("delta"));

        assert!(cfg.engine.claude_code.transcript_tail);
        assert_eq!(cfg.engine.claude_code.permission_policy, "auto_approve_all");
        assert_eq!(cfg.engine.claude_code.hook_transport, "state_file");
        assert_eq!(cfg.engine.claude_code.inject_hooks.len(), 6);

        assert!(cfg.orchestrator.cavekit.watch_ralph_loop);
        assert!(cfg.orchestrator.cavekit.watch_impl_tracking);
        assert!(cfg.orchestrator.cavekit.spawn_review_tab);
        assert_eq!(cfg.orchestrator.cavekit.default_layout, "builder");
        assert_eq!(cfg.orchestrator.cavekit.review_layout, "review");
        assert_eq!(cfg.orchestrator.cavekit.review_on_phase, "check");

        assert_eq!(cfg.orchestrator.claude_code.default_layout, "classic");

        assert!(cfg.mux.zellij.status_plugin_path.contains("ark-status"));
        assert!(cfg.mux.zellij.picker_plugin_path.contains("ark-picker"));

        assert!(cfg.hooks.is_empty());
    }

    #[test]
    fn default_equals_defaults() {
        assert_eq!(Config::default(), Config::defaults());
    }

    #[test]
    fn empty_toml_round_trips_to_defaults() {
        Jail::expect_with(|jail| {
            jail.create_file("c.toml", "")?;
            let cfg: Config = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract()
                .expect("empty toml should yield defaults");
            assert_eq!(cfg, Config::defaults());
            Ok(())
        });
    }

    #[test]
    fn unknown_top_level_key_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                bogus_top_level = true
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(res.is_err(), "deny_unknown_fields should reject typo");
            Ok(())
        });
    }

    #[test]
    fn unknown_nested_key_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [defaults]
                auto_clos_on_done = true   # typo
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(res.is_err(), "typo in nested section must error");
            Ok(())
        });
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [defaults]
                stall_timeout_secs = 999
                "#,
            )?;
            let cfg: Config = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract()
                .expect("partial section ok");
            assert_eq!(cfg.defaults.stall_timeout_secs, 999);
            // sibling untouched
            assert_eq!(cfg.defaults.engine, "claude-code");
            // other sections untouched
            assert_eq!(cfg.diff.debounce_ms, 300);
            Ok(())
        });
    }

    // -----------------------------------------------------------------
    // T-119 (cavekit-testing R3): deny_unknown_fields coverage for the
    // remaining nested sections — kit R2 demands typos surface across
    // EVERY section, not just the top level + [defaults].
    // -----------------------------------------------------------------

    #[test]
    fn unknown_key_in_engine_claude_code_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [engine.claude_code]
                transcrpt_tail = true    # typo: missing 'i'
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(
                res.is_err(),
                "typo in [engine.claude_code] must error; got {res:?}"
            );
            Ok(())
        });
    }

    #[test]
    fn unknown_key_in_orchestrator_cavekit_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [orchestrator.cavekit]
                watch_ralf_loop = true   # typo
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(res.is_err(), "typo in [orchestrator.cavekit] must error");
            Ok(())
        });
    }

    #[test]
    fn unknown_key_in_mux_zellij_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [mux.zellij]
                statuss_plugin_path = "x"   # typo
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(res.is_err(), "typo in [mux.zellij] must error");
            Ok(())
        });
    }

    #[test]
    fn unknown_key_in_diff_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [diff]
                debouce_ms = 500   # typo
                "#,
            )?;
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract();
            assert!(res.is_err(), "typo in [diff] must error");
            Ok(())
        });
    }

    #[test]
    fn engine_claude_code_inject_hooks_overridable() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                r#"
                [engine.claude_code]
                inject_hooks = ["Stop"]
                "#,
            )?;
            let cfg: Config = Figment::new()
                .merge(Toml::file(jail.directory().join("c.toml")))
                .extract()
                .expect("override inject_hooks");
            assert_eq!(
                cfg.engine.claude_code.inject_hooks,
                vec!["Stop".to_string()]
            );
            Ok(())
        });
    }
}
