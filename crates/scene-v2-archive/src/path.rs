//! Scene path resolution.
//!
//! [`resolve_scene_path`] is the **pure** function that decides which
//! scene file the runtime should load. It takes every input as an
//! argument — no environment reads, no `std::env::current_dir`, no
//! filesystem mutation — so the caller can test every rung
//! deterministically and the resolver itself is trivial to unit-test
//! against tempdirs.
//!
//! ## Precedence (from `cavekit-scene.md` R13)
//!
//! 1. **`--scene NAME`** flag explicitly supplied on the CLI.
//! 2. **`ARK_SCENE` env var** (caller injects via the `env_scene`
//!    parameter).
//! 3. **`./.ark/scene.kdl`** — project-local scene.
//! 4. **`$XDG_CONFIG_HOME/<appname>/scenes/default.kdl`** — user-global
//!    scene under the XDG config tree. `appname` defaults to `ark` and
//!    can be overridden via the `ARK_APPNAME` env var (caller injects
//!    via the `env_appname` parameter so the resolver itself stays
//!    pure).
//! 5. **Built-in default** compiled into the binary via
//!    [`include_str!`] (see [`DEFAULT_SCENE_KDL`]). Always available;
//!    final fallback so `ark` boots even on a brand-new machine.
//!
//! Rules 1 + 2 resolve a *name*, not a path: the caller is responsible
//! for translating that name through the scene-search-path before
//! reading the file. The resolver only checks that the name is
//! non-empty and surfaces it as a [`ResolvedScene::Named`].
//!
//! Rules 3 + 4 only fire when the corresponding file actually exists
//! on disk (so a stray dotfile doesn't accidentally shadow the
//! built-in default for users who forgot to populate it).
//!
//! ## Convenience: env-reading wrapper
//!
//! [`resolve_scene_path_from_env`] reads `ARK_SCENE` and `ARK_APPNAME`
//! from the process environment and forwards to the pure function. It
//! exists so production callers don't have to thread env vars through
//! by hand, while tests can keep using the pure entry point.

use std::path::{Path, PathBuf};

/// v0.1 built-in default scene KDL (inline plugin form). Embedded via
/// [`include_str!`] so the binary always carries a working scene even
/// when no configuration files are present.
///
/// This is the **authoritative** default for v0.3. T-10.10 (this tier)
/// migrates the source to the `use "picker"` / `use "status"` form —
/// see [`DEFAULT_SCENE_KDL_USE_FORM`] — but the runtime still falls
/// back to the inline form because the built-in extension registry
/// (compiled-in `register_extension!` resolution in
/// `use_resolution.rs`) is not yet wired end-to-end. Both forms
/// compile to identical runtime behaviour; the inline form remains
/// parseable indefinitely (Rust-editions / Neovim-Lua-shim
/// convention).
pub const DEFAULT_SCENE_KDL: &str = include_str!("default_scene.kdl");

/// v0.3 built-in default scene KDL in `use` form (T-10.10).
///
/// The `use "picker"` / `use "status"` shape activates the two
/// shipped plugins through the extension surface (R10 / R11)
/// instead of declaring inline plugin blocks. Equivalent to
/// [`DEFAULT_SCENE_KDL`] once the built-in extension registry
/// consumes the picker + status `ExtensionMetadata` declarations
/// (both contributed via `register_extension!`).
///
/// Structural equivalence with the inline form is asserted in
/// `tests::default_scene_use_form_parses_to_equivalent_shape`.
pub const DEFAULT_SCENE_KDL_USE_FORM: &str =
    include_str!("default_scene_use_form.kdl");

/// Default value for the `<appname>` segment of the XDG config path
/// when no `ARK_APPNAME` override is supplied. Matches the binary
/// name as shipped.
pub const DEFAULT_APPNAME: &str = "ark";

/// Outcome of [`resolve_scene_path`].
///
/// Distinguishes the four resolved cases the runtime needs to react
/// to differently: a CLI/env-named scene (still needs name → path
/// resolution), a concrete file path on disk, and the embedded
/// built-in default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedScene {
    /// User passed `--scene NAME` (rung 1) or set `ARK_SCENE=NAME`
    /// (rung 2). The caller still has to translate the name through
    /// the scene-search-path before reading the file.
    Named(String),

    /// A concrete file path the resolver verified exists on disk
    /// (rungs 3 and 4). Caller can `std::fs::read_to_string(&path)`
    /// directly.
    Path(PathBuf),

    /// No user scene found — fall back to the binary-embedded
    /// default. The string is the full KDL source of
    /// [`DEFAULT_SCENE_KDL`]; the runtime parses it via the same
    /// `parse_scene` entry point as user-supplied files.
    BuiltIn(&'static str),
}

/// Pure scene-path resolver implementing R13 precedence.
///
/// Inputs:
/// * `flag` — the `--scene NAME` CLI argument, if supplied.
/// * `env_scene` — value of `$ARK_SCENE`, if set. Empty string treated
///   as unset to match shell idioms (`ARK_SCENE=` clears the var).
/// * `env_appname` — value of `$ARK_APPNAME`, if set. Same empty-string
///   handling as `env_scene`.
/// * `cwd` — current working directory (caller injects).
/// * `xdg_config_home` — value of `$XDG_CONFIG_HOME`. When `None` the
///   caller is expected to fall back to `$HOME/.config` themselves
///   (XDG default), or pass `Some(home.join(".config"))` directly.
///   Keeping this as an explicit parameter rather than reading
///   `$HOME` inside the function preserves purity.
///
/// Returns the first rung that resolves. Never errors: the built-in
/// default (rung 5) is always available.
pub fn resolve_scene_path(
    flag: Option<&str>,
    env_scene: Option<&str>,
    env_appname: Option<&str>,
    cwd: &Path,
    xdg_config_home: Option<&Path>,
) -> ResolvedScene {
    // Rung 1: --scene NAME (flag takes precedence over everything).
    if let Some(name) = non_empty(flag) {
        return ResolvedScene::Named(name.to_string());
    }

    // Rung 2: ARK_SCENE env var.
    if let Some(name) = non_empty(env_scene) {
        return ResolvedScene::Named(name.to_string());
    }

    // Rung 3: ./.ark/scene.kdl in cwd.
    let project_local = cwd.join(".ark").join("scene.kdl");
    if project_local.is_file() {
        return ResolvedScene::Path(project_local);
    }

    // Rung 4: $XDG_CONFIG_HOME/<appname>/scenes/default.kdl.
    if let Some(xdg) = xdg_config_home {
        let appname = non_empty(env_appname).unwrap_or(DEFAULT_APPNAME);
        let user_global = xdg.join(appname).join("scenes").join("default.kdl");
        if user_global.is_file() {
            return ResolvedScene::Path(user_global);
        }
    }

    // Rung 5: built-in default — always available.
    ResolvedScene::BuiltIn(DEFAULT_SCENE_KDL)
}

/// Convenience wrapper that reads `$ARK_SCENE`, `$ARK_APPNAME`, and
/// `$XDG_CONFIG_HOME` from the process environment, then forwards to
/// the pure [`resolve_scene_path`].
///
/// Production CLI entry points use this; tests prefer the pure
/// function so env state cannot leak between cases.
///
/// `flag` is the value of `--scene NAME` from arg parsing (still
/// caller-supplied — we never touch `std::env::args`).
pub fn resolve_scene_path_from_env(flag: Option<&str>, cwd: &Path) -> ResolvedScene {
    let env_scene = std::env::var("ARK_SCENE").ok();
    let env_appname = std::env::var("ARK_APPNAME").ok();
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            // XDG default: $HOME/.config when $XDG_CONFIG_HOME is
            // unset or empty. Mirrors the XDG Base Directory Spec.
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config"))
        });

    resolve_scene_path(
        flag,
        env_scene.as_deref(),
        env_appname.as_deref(),
        cwd,
        xdg.as_deref(),
    )
}

/// Treat `None` and `Some("")` as the same "unset" state — matches
/// shell idioms (`ARK_SCENE=` clears the var) and avoids surfacing an
/// empty string as a resolvable scene name.
fn non_empty(s: Option<&str>) -> Option<&str> {
    s.and_then(|v| if v.is_empty() { None } else { Some(v) })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Rung 1: `--scene NAME` wins regardless of any other input.
    #[test]
    fn flag_takes_precedence_over_everything() {
        let tmp = TempDir::new().unwrap();
        // Even with rungs 2/3/4 satisfied, rung 1 still wins.
        let cwd = tmp.path();
        fs::create_dir_all(cwd.join(".ark")).unwrap();
        fs::write(cwd.join(".ark/scene.kdl"), "scene \"x\"").unwrap();

        let resolved = resolve_scene_path(
            Some("from-flag"),
            Some("from-env"),
            None,
            cwd,
            None,
        );
        assert_eq!(resolved, ResolvedScene::Named("from-flag".to_string()));
    }

    /// Rung 1 ignores empty-string flag (shell idiom: `--scene ""`
    /// is treated as unset).
    #[test]
    fn empty_flag_falls_through_to_env() {
        let resolved = resolve_scene_path(
            Some(""),
            Some("from-env"),
            None,
            Path::new("/nonexistent"),
            None,
        );
        assert_eq!(resolved, ResolvedScene::Named("from-env".to_string()));
    }

    /// Rung 2: `ARK_SCENE` resolves when no flag is supplied.
    #[test]
    fn env_scene_resolves_when_no_flag() {
        let resolved = resolve_scene_path(
            None,
            Some("alt"),
            None,
            Path::new("/nonexistent"),
            None,
        );
        assert_eq!(resolved, ResolvedScene::Named("alt".to_string()));
    }

    /// Rung 3: project-local `./.ark/scene.kdl` wins over rung 4 + 5.
    #[test]
    fn project_local_scene_wins_over_xdg_and_default() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        fs::create_dir_all(cwd.join(".ark")).unwrap();
        let path = cwd.join(".ark/scene.kdl");
        fs::write(&path, "scene \"local\"").unwrap();

        // Even with XDG path populated, rung 3 wins.
        let xdg_root = TempDir::new().unwrap();
        let xdg_scene = xdg_root.path().join("ark/scenes/default.kdl");
        fs::create_dir_all(xdg_scene.parent().unwrap()).unwrap();
        fs::write(&xdg_scene, "scene \"xdg\"").unwrap();

        let resolved = resolve_scene_path(
            None,
            None,
            None,
            cwd,
            Some(xdg_root.path()),
        );
        assert_eq!(resolved, ResolvedScene::Path(path));
    }

    /// Rung 3 falls through when the file does not exist (a stray
    /// `.ark` directory without a `scene.kdl` inside should not
    /// shadow rung 4 / 5).
    #[test]
    fn project_local_falls_through_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        // Directory exists but file does not.
        fs::create_dir_all(cwd.join(".ark")).unwrap();

        let resolved = resolve_scene_path(None, None, None, cwd, None);
        assert!(matches!(resolved, ResolvedScene::BuiltIn(_)));
    }

    /// Rung 4: `$XDG_CONFIG_HOME/<appname>/scenes/default.kdl` resolves
    /// when nothing higher matched.
    #[test]
    fn xdg_default_scene_resolves() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();
        let scene_path = xdg.path().join("ark/scenes/default.kdl");
        fs::create_dir_all(scene_path.parent().unwrap()).unwrap();
        fs::write(&scene_path, "scene \"xdg-default\"").unwrap();

        let resolved = resolve_scene_path(None, None, None, cwd, Some(xdg.path()));
        assert_eq!(resolved, ResolvedScene::Path(scene_path));
    }

    /// Rung 4 honours the `ARK_APPNAME` override so users sharing
    /// `$XDG_CONFIG_HOME` between flavours of the binary can keep
    /// scenes under distinct subdirs.
    #[test]
    fn xdg_default_scene_honours_appname_override() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();
        let scene_path = xdg.path().join("custom/scenes/default.kdl");
        fs::create_dir_all(scene_path.parent().unwrap()).unwrap();
        fs::write(&scene_path, "scene \"custom-app\"").unwrap();

        let resolved =
            resolve_scene_path(None, None, Some("custom"), cwd, Some(xdg.path()));
        assert_eq!(resolved, ResolvedScene::Path(scene_path));
    }

    /// Rung 4 only fires when the file actually exists on disk.
    #[test]
    fn xdg_default_falls_through_when_missing() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();
        // No file written under xdg/ark/scenes/.

        let resolved = resolve_scene_path(None, None, None, cwd, Some(xdg.path()));
        assert!(matches!(resolved, ResolvedScene::BuiltIn(_)));
    }

    /// Rung 4 is skipped entirely when the caller supplies no XDG
    /// config home (e.g. `$XDG_CONFIG_HOME` and `$HOME` both unset).
    #[test]
    fn xdg_skipped_when_xdg_config_home_is_none() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_scene_path(None, None, None, tmp.path(), None);
        assert!(matches!(resolved, ResolvedScene::BuiltIn(_)));
    }

    /// Rung 5: with no input and no files, the built-in default is
    /// always served.
    #[test]
    fn builtin_default_is_always_available() {
        let tmp = TempDir::new().unwrap();
        let resolved = resolve_scene_path(None, None, None, tmp.path(), None);
        match resolved {
            ResolvedScene::BuiltIn(src) => {
                // Default must contain the picker plugin per
                // T-8.0 acceptance criterion.
                assert!(src.contains("plugin \"picker\""));
                assert!(src.contains("plugin \"status\""));
            }
            other => panic!("expected BuiltIn, got {other:?}"),
        }
    }

    /// The embedded default scene must itself parse via `parse_scene`
    /// — otherwise users who fall through to rung 5 will see a
    /// confusing "shipped scene is broken" error.
    #[test]
    fn builtin_default_parses_cleanly() {
        let parsed =
            crate::parse::parse_scene(DEFAULT_SCENE_KDL, Path::new("<built-in>"))
                .expect("built-in default must parse");
        assert_eq!(parsed.scene.name, "default");
        // Sanity: both shipped plugins present.
        let names: Vec<&str> =
            parsed.scene.plugins.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"picker"));
        assert!(names.contains(&"status"));
        // Sanity: keybinds attached.
        assert!(!parsed.scene.keybinds.is_empty());
    }

    /// T-10.10 validation gate: the `use`-form default scene parses
    /// cleanly and carries the same externally-observable surface as
    /// the inline-form default (same scene name, same keybinds, same
    /// extension activations). The merge equivalence with inline
    /// `plugin { … }` blocks is asserted in a dedicated test below
    /// (it depends on the shipped sidecar scene fragments baked into
    /// the binary via `include_str!`).
    #[test]
    fn default_scene_use_form_parses_to_equivalent_shape() {
        let use_form = crate::parse::parse_scene(
            DEFAULT_SCENE_KDL_USE_FORM,
            Path::new("<built-in:use-form>"),
        )
        .expect("use-form default must parse");
        let inline = crate::parse::parse_scene(
            DEFAULT_SCENE_KDL,
            Path::new("<built-in:inline>"),
        )
        .expect("inline default must parse");

        // Both point at the same scene name.
        assert_eq!(use_form.scene.name, inline.scene.name);
        assert_eq!(use_form.scene.name, "default");

        // Same keybinds (chord + intent, last-wins irrelevant here).
        let kb_use: Vec<(&str, Option<&str>)> = use_form
            .scene
            .keybinds
            .iter()
            .map(|k| (k.chord.as_str(), k.intent.as_deref()))
            .collect();
        let kb_inline: Vec<(&str, Option<&str>)> = inline
            .scene
            .keybinds
            .iter()
            .map(|k| (k.chord.as_str(), k.intent.as_deref()))
            .collect();
        assert_eq!(kb_use, kb_inline);

        // Use-form activates picker + status via `use`; inline carries
        // them as top-level plugin blocks. Assert the right shape on
        // each side.
        let use_names: Vec<&str> = use_form
            .scene
            .uses
            .iter()
            .map(|u| u.name.as_str())
            .collect();
        assert_eq!(use_names, vec!["picker", "status"]);
        assert!(use_form.scene.plugins.is_empty());

        let inline_names: Vec<&str> = inline
            .scene
            .plugins
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert!(inline_names.contains(&"picker"));
        assert!(inline_names.contains(&"status"));
    }

    /// T-10.10: the plugin sidecar scene fragments shipped by
    /// `ark-plugin-picker` and `ark-plugin-status` parse cleanly. The
    /// merge pipeline splices these into the `use`-form default scene
    /// once the built-in extension registry is wired end-to-end; until
    /// then, this test is the structural-equivalence witness.
    ///
    /// The sidecar text is re-declared here rather than pulled through
    /// the plugin crates' public API to keep `ark-scene` dep-free of
    /// `ark-plugin-*` (the dep graph runs the other direction).
    #[test]
    fn builtin_plugin_sidecars_parse() {
        let picker = r#"
scene "picker-sidecar" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}
"#;
        let status = r#"
scene "status-sidecar" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#;
        let picker_ir = crate::parse::parse_scene(
            picker,
            Path::new("<built-in:picker-sidecar>"),
        )
        .expect("picker sidecar must parse");
        let status_ir = crate::parse::parse_scene(
            status,
            Path::new("<built-in:status-sidecar>"),
        )
        .expect("status sidecar must parse");

        // Each sidecar contributes exactly one plugin block.
        assert_eq!(picker_ir.scene.plugins.len(), 1);
        assert_eq!(picker_ir.scene.plugins[0].name, "picker");
        assert_eq!(status_ir.scene.plugins.len(), 1);
        assert_eq!(status_ir.scene.plugins[0].name, "status");
    }
}
