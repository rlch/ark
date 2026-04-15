//! Scene compile pipeline run by the supervisor at boot (T-8.1).
//!
//! Wires the canonical `cavekit-supervisor.md` R3 step 7 sequence:
//! parse the scene file referenced by `AgentSpec.scene_path` (or the
//! embedded built-in default when the spec carries no scene), validate
//! every CEL predicate + template, build a [`ReactionRegistry`] from
//! the scene's `on { }` and `keybind { }` nodes, and expose the lowered
//! plugin decls for the always-on mount pass.
//!
//! The CLI already compiles the layout for zellij before the
//! supervisor forks (`compile_scene_file` writes the rendered
//! `.kdl` to `${XDG_RUNTIME_DIR}/ark/layouts/{scene-hash}-scene.kdl`).
//! The work in this module is deliberately narrower: we only need the
//! compile artefacts that DRIVE the supervisor's own runtime
//! consumers — the reaction dispatcher, the plugin lifecycle manager,
//! and the control-socket intent bridge. Re-running the layout writer
//! here would either duplicate work or race against the CLI's already
//! rendered file.
//!
//! # Failure handling
//!
//! Any hard failure (I/O, UTF-8 decode, facet-kdl parse, validate) is
//! surfaced as an `anyhow::Error` carrying a pre-rendered miette
//! diagnostic. The supervisor caller logs the diagnostic, drops the
//! lock / socket already taken, and exits via [`Outcome::Crashed`] so
//! the parent CLI surfaces a non-zero exit code — matching the
//! cavekit-supervisor R3 step 7 contract ("Compile error = abort spawn
//! with miette diagnostic").

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use ark_scene::ast::SceneDoc;
use ark_scene::engine::EngineLaunch;
use ark_scene::hook_compat::{HookEntry as SceneHookEntry, extend_registry_with_hooks};
use ark_scene::id::SceneId;
use ark_scene::path::DEFAULT_SCENE_KDL;
use ark_scene::plugin::{PluginDecl, lower_plugin};
use ark_scene::reactions::{ReactionRegistry, populate_registry, scene_max_cascade_depth};
use ark_scene::validate::validate_scene;
use tracing::{debug, warn};

/// Source of the scene KDL the supervisor compiled.
///
/// Distinguishes "user passed a scene path on `spec.scene_path`" from
/// "no scene configured, fell back to the binary-embedded default".
/// The supervisor surfaces this via tracing so operators can tell from
/// a single log line why a given agent is running against a particular
/// reaction graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneSource {
    /// `spec.scene_path` pointed at an on-disk file that the supervisor
    /// read + parsed.
    Path(PathBuf),
    /// `spec.scene_path` was `None` (or the file was missing) — the
    /// embedded [`DEFAULT_SCENE_KDL`] served instead.
    BuiltIn,
}

impl SceneSource {
    /// Human-readable label for tracing / diagnostics.
    pub fn display(&self) -> String {
        match self {
            SceneSource::Path(p) => p.display().to_string(),
            SceneSource::BuiltIn => "<built-in default>".to_string(),
        }
    }
}

/// Fully compiled scene artefacts the supervisor threads into its
/// consumers.
///
/// Held by `run_supervisor_with` for the lifetime of the agent. The
/// [`SceneDoc`] is retained so later consumers (plugin lifecycle, ark
/// scene graph) can walk typed AST rather than re-parse from disk.
#[derive(Debug)]
pub struct CompiledScene {
    /// Where the scene KDL came from (file path or built-in).
    pub source: SceneSource,
    /// Parsed scene AST. Kept alive for downstream borrows
    /// ([`plugin_decls`](Self::plugin_decls) references it).
    pub doc: SceneDoc,
    /// Stable identity for cascade telemetry + scene graph attribution.
    pub scene_id: SceneId,
    /// Reaction registry populated from the scene's `on { }` and
    /// `keybind { }` nodes, with legacy `[[hooks]]` entries extended
    /// on top.
    pub registry: Arc<ReactionRegistry>,
    /// Resolved `max-cascade-depth` for this scene (R4). Defaults to
    /// [`ark_scene::intent::DEFAULT_MAX_CASCADE_DEPTH`] when absent.
    pub max_cascade_depth: u32,
    /// Resolved ACP engine launch spec for this agent
    /// (T-ACP.4a/4b). `None` when the supervisor didn't thread a
    /// runtime config through — legacy test paths fall back to
    /// spawning via the old engine trait. Populated by
    /// [`crate::engine_resolution::resolve_engine`] during boot.
    pub engine_launch: Option<EngineLaunch>,
}

impl CompiledScene {
    /// Lift every `plugin { }` declaration in the scene into a typed
    /// [`PluginDecl`], skipping any whose lifecycle lowering errored
    /// (ambiguous `summon` + `on` pairings — caller's `ark scene check`
    /// already surfaced those; at supervisor boot we degrade gracefully).
    ///
    /// Returns borrowed decls — the lifetime is tied to this
    /// [`CompiledScene`]'s `doc` field.
    pub fn plugin_decls(&self) -> Vec<PluginDecl<'_>> {
        let mut out = Vec::new();
        for plugin in &self.doc.scene.plugins {
            match lower_plugin(plugin) {
                Ok(decl) => out.push(decl),
                Err(err) => {
                    warn!(
                        plugin = %plugin.name,
                        error = %err,
                        "plugin lowering failed; skipping at supervisor boot"
                    );
                }
            }
        }
        out
    }
}

/// Compile the scene referenced by `scene_path`, or the built-in
/// default when `scene_path` is `None` / points at a missing file.
///
/// Runs the full R3 step 7 sequence: parse → validate → populate
/// reaction registry → merge legacy hook entries. Does NOT render the
/// zellij layout KDL: that step lives in the CLI (`compile_and_write_scene`)
/// and runs before the supervisor forks so the layout file exists by
/// the time zellij is launched.
///
/// On success every returned artefact is `Arc`-wrapped where cloning
/// is expected (registry) so downstream consumers can share without
/// extra allocations. On any failure the `Err` surfaces an
/// `anyhow::Error` that already wraps a miette diagnostic-style
/// message for logging.
pub fn compile_scene_for_runtime(
    scene_path: Option<&Path>,
    hooks: &[SceneHookEntry],
) -> Result<CompiledScene> {
    let (src, source) = load_scene_source(scene_path)?;

    let doc = parse_scene_src(&src, &source)?;

    // T-2.6: CEL + template validation. Collect every diagnostic the
    // single-pass walk produces and render them as a single error
    // message so the operator sees the full picture.
    if let Err(errs) = validate_scene(&doc) {
        let joined = errs
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow::anyhow!(
            "scene `{}` failed validation:\n{joined}",
            source.display()
        ));
    }

    // T-5.2 / T-5.3: build the primary reaction registry from the scene
    // AST. `populate_registry` walks every `on { }` and `keybind { }`
    // node, parses selectors, and compiles each `if=` predicate.
    let mut registry = populate_registry(&doc).map_err(|errs| {
        let joined = errs
            .iter()
            .map(|e| format!("- {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::anyhow!(
            "scene `{}` reaction compile failed:\n{joined}",
            source.display()
        )
    })?;

    // T-5.7: legacy TOML `[[hooks]]` merge on top of the scene registry.
    // Hook entries fire after scene reactions — matches the historical
    // fire order (hooks were the very last consumer to subscribe).
    if !hooks.is_empty() {
        extend_registry_with_hooks(&mut registry, hooks);
    }

    let max_cascade_depth = scene_max_cascade_depth(&doc);

    let scene_id = match &source {
        SceneSource::Path(p) => SceneId::from_bytes(p.clone(), src.as_bytes()),
        SceneSource::BuiltIn => SceneId::from_bytes(
            PathBuf::from("<built-in>"),
            DEFAULT_SCENE_KDL.as_bytes(),
        ),
    };

    debug!(
        source = %source.display(),
        reactions = registry.len(),
        plugins = doc.scene.plugins.len(),
        max_cascade_depth,
        "scene compiled at supervisor boot (R3 step 7)"
    );

    Ok(CompiledScene {
        source,
        doc,
        scene_id,
        registry: Arc::new(registry),
        max_cascade_depth,
        // T-ACP.4a: the boot path fills this in post-compile via
        // [`CompiledScene::with_engine_launch`] once the
        // [`ark_config::Config`] + CLI flag are in hand. Leave `None`
        // here so the legacy scene-compile-only test callers still
        // round-trip without new inputs.
        engine_launch: None,
    })
}

impl CompiledScene {
    /// T-ACP.4a: install a resolved [`EngineLaunch`] on the compiled
    /// scene. Builder-style so the boot path can chain
    /// `compile_scene_for_runtime(...).with_engine_launch(launch)`.
    pub fn with_engine_launch(mut self, launch: EngineLaunch) -> Self {
        self.engine_launch = Some(launch);
        self
    }
}

/// Load the KDL source for the scene the supervisor should compile.
///
/// Returns `(src, SceneSource)`. When `scene_path` is `Some(p)` and `p`
/// exists on disk, reads + decodes it. When `scene_path` is `None` or
/// the file is missing, falls back to [`DEFAULT_SCENE_KDL`].
fn load_scene_source(scene_path: Option<&Path>) -> Result<(String, SceneSource)> {
    match scene_path {
        Some(p) if p.is_file() => {
            let bytes = std::fs::read(p)
                .with_context(|| format!("read scene `{}`", p.display()))?;
            let src = String::from_utf8(bytes)
                .with_context(|| format!("scene `{}` is not valid utf-8", p.display()))?;
            Ok((src, SceneSource::Path(p.to_path_buf())))
        }
        Some(p) => {
            // Spec carried a path but the file is missing. Per R3 step
            // 7 we want a clean diagnostic rather than silently falling
            // back — the operator configured a scene on purpose.
            Err(anyhow::anyhow!(
                "scene `{}` does not exist or is not a regular file",
                p.display()
            ))
        }
        None => Ok((DEFAULT_SCENE_KDL.to_string(), SceneSource::BuiltIn)),
    }
}

/// Parse the scene KDL source via facet-kdl, mapping any parse error
/// back onto the original source path for a readable diagnostic.
fn parse_scene_src(src: &str, source: &SceneSource) -> Result<SceneDoc> {
    let path: PathBuf = match source {
        SceneSource::Path(p) => p.clone(),
        SceneSource::BuiltIn => PathBuf::from("<built-in>"),
    };
    ark_scene::parse::parse_scene(src, &path)
        .map_err(|e| anyhow::anyhow!("scene `{}` parse failed: {e}", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> TempDir {
        tempfile::Builder::new()
            .prefix("scene-runtime")
            .tempdir_in("/tmp")
            .expect("tempdir")
    }

    /// `scene_path = None` falls back to the binary-embedded default
    /// and successfully compiles (the shipped default must always
    /// parse + validate).
    #[test]
    fn built_in_default_compiles_successfully() {
        let compiled = compile_scene_for_runtime(None, &[]).expect("built-in compiles");
        assert_eq!(compiled.source, SceneSource::BuiltIn);
        // Default scene ships with the picker + status plugins.
        let decl_names: Vec<&str> =
            compiled.doc.scene.plugins.iter().map(|p| p.name.as_str()).collect();
        assert!(decl_names.contains(&"picker"));
        assert!(decl_names.contains(&"status"));
    }

    /// `scene_path = Some(valid file)` reads + parses the file and
    /// returns `SceneSource::Path(p)`.
    #[test]
    fn user_scene_file_path_round_trips() {
        let tmp = tempdir();
        let path = tmp.path().join("custom.kdl");
        std::fs::write(
            &path,
            r#"scene "custom" {
    plugin "status-bar" {
        source "shipped:status"
        mount "status-bar"
    }
    on "Started" {
        set_status text="ready"
    }
}
"#,
        )
        .unwrap();

        let compiled = compile_scene_for_runtime(Some(&path), &[])
            .expect("custom scene compiles");
        assert_eq!(compiled.source, SceneSource::Path(path));
        assert_eq!(compiled.doc.scene.plugins.len(), 1);
        assert_eq!(compiled.doc.scene.ons.len(), 1);
        // Registry has one reaction registered against Started.
        assert!(!compiled.registry.is_empty());
    }

    /// Missing file on `scene_path` is a hard error — the supervisor
    /// aborts spawn rather than silently falling back to the built-in.
    #[test]
    fn missing_scene_file_errors() {
        let tmp = tempdir();
        let missing = tmp.path().join("does-not-exist.kdl");
        let err =
            compile_scene_for_runtime(Some(&missing), &[]).expect_err("missing file errors");
        let msg = err.to_string();
        assert!(msg.contains("does not exist"), "got: {msg}");
    }

    /// Syntactically invalid scene surfaces as a parse error.
    #[test]
    fn parse_error_surfaces_as_anyhow_error() {
        let tmp = tempdir();
        let path = tmp.path().join("bad.kdl");
        // Missing closing brace on `scene { }`
        std::fs::write(&path, r#"scene "bad" { not-a-valid-node !"#).unwrap();

        let err = compile_scene_for_runtime(Some(&path), &[]).expect_err("parse error");
        let msg = err.to_string();
        assert!(
            msg.contains("parse failed") || msg.contains("parse"),
            "expected parse failure message, got: {msg}"
        );
    }

    /// Legacy hook entries merge into the scene registry — a scene with
    /// zero reactions plus a non-empty hook list surfaces the hook
    /// reactions in the final registry.
    #[test]
    fn legacy_hooks_merge_into_scene_registry() {
        let tmp = tempdir();
        let path = tmp.path().join("empty.kdl");
        std::fs::write(
            &path,
            r#"scene "empty" {
    plugin "status-bar" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();

        let hooks = vec![SceneHookEntry::new(
            "echo hello",
            Vec::new(),
            Vec::new(), // on_event = empty => match every kind
            Vec::new(),
            Vec::new(),
        )];

        let compiled =
            compile_scene_for_runtime(Some(&path), &hooks).expect("scene + hooks compiles");
        // Scene has zero reactions but the hook `*` selector
        // synthesises one per `EventKind`; registry must be non-empty.
        assert!(
            !compiled.registry.is_empty(),
            "expected hook-derived reactions in registry"
        );
    }

    /// `plugin_decls()` lifts every typed plugin node into a lowered
    /// decl in source order.
    #[test]
    fn plugin_decls_preserve_source_order() {
        let tmp = tempdir();
        let path = tmp.path().join("ordered.kdl");
        std::fs::write(
            &path,
            r#"scene "ordered" {
    plugin "first" {
        source "shipped:status"
        mount "status-bar"
    }
    plugin "second" {
        source "shipped:picker"
        mount "floating"
        summon "UserEvent:picker.show"
    }
}
"#,
        )
        .unwrap();

        let compiled = compile_scene_for_runtime(Some(&path), &[]).expect("compiles");
        let decls = compiled.plugin_decls();
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].name, "first");
        assert_eq!(decls[1].name, "second");
    }
}
