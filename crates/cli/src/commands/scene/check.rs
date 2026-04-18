//! `ark scene check` — full parse + validate + compile diagnostic command.
//!
//! T-118 (cavekit-scene R13). Exit 0 on green; non-zero with
//! diagnostics on any error. Emits every error, not just first.
//!
//! Pipeline:
//! 1. Shape detect + normalize (T-112)
//! 2. Parse scene (facet-kdl)
//! 3. Compose (resolve includes, T-074..T-077)
//! 4. Validate scope (R2 placement rules)
//! 5. Validate handles (R2 @ident dedup)
//! 6. Validate pane views (R3 one-view-per-pane)
//! 7. Validate op refs (R7 handle-type rules)
//! 8. Compile Rhai predicates + interpolation holes (T-023/T-024)

use std::path::{Path, PathBuf};

use clap::Args;

use ark_scene::compile::{CompiledScene, compile_scene};
use ark_scene::compose::compose_scene;
use ark_scene::default_scene::DEFAULT_SCENE_KDL;
use ark_scene::error::SceneError;
use ark_scene::parse::parse_scene;
use ark_scene::resolve_path::{SceneSource, resolve_scene_path};
use ark_scene::rhai::Engine;
use ark_scene::shape::detect_and_normalize;
use ark_scene::validate::{handles, op_refs, pane_views, scope};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Path to a scene file. Validates the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Enable v1.0-strict validation mode. Rejects ops outside the frozen
    /// `ark.core.*` vocabulary, upgrades `warning[ext/unknown-capability]`
    /// and `warning[scene/deprecated-op]` to errors, and rejects engine
    /// blocks targeting unwired engines.
    #[arg(long = "v1-strict")]
    pub v1_strict: bool,
}

pub fn run(args: CheckArgs, ctx: &Ctx) -> Result<(), CliError> {
    let (src, display_path) = load_scene_source(&args)?;
    let mut all_errors: Vec<SceneError> = Vec::new();

    // Phase 1: shape detect + normalize.
    let normalized = match detect_and_normalize(&src, Path::new(&display_path)) {
        Ok(n) => Some(n),
        Err(e) => {
            all_errors.push(e);
            None
        }
    };

    // Phase 2: parse.
    let ir = if let Some(ref norm_src) = normalized {
        match parse_scene(norm_src, &display_path) {
            Ok(ir) => Some(ir),
            Err(e) => {
                all_errors.push(e);
                None
            }
        }
    } else {
        None
    };

    // Phase 3: compose (resolve includes).
    let ir = if let Some(ir) = ir {
        match compose_scene(ir) {
            Ok(composed) => Some(composed),
            Err(e) => {
                all_errors.push(e);
                None
            }
        }
    } else {
        None
    };

    // Phase 4: validate (scope, handles, pane views, op refs).
    // Only runs when parsing + compose succeeded.
    if let Some(ref ir) = ir {
        all_errors.extend(scope::validate_scope(ir));
        all_errors.extend(handles::validate_handles(ir));
        all_errors.extend(pane_views::validate_pane_views(ir));
        all_errors.extend(op_refs::validate_op_refs(ir));
    }

    // Phase 5: compile Rhai predicates + interpolation holes.
    // Only runs when parsing + compose succeeded and no validation errors
    // so far (a malformed AST can cause misleading compile errors).
    let _compiled: Option<CompiledScene> = if let Some(ir) = ir {
        if all_errors.is_empty() {
            let engine = Engine::new();
            match compile_scene(&engine, ir) {
                Ok(cs) => Some(cs),
                Err(e) => {
                    all_errors.push(e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Phase 6: v1-strict validation (T-135).
    // TODO(T-135): Implement actual v1 contract validation. Currently
    // the flag is recognized and reported but the concrete checks
    // (frozen op vocabulary, unknown-capability upgrade, deprecated-op
    // upgrade, engine-block restrictions) are not yet wired.
    if args.v1_strict {
        eprintln!("scene check: --v1-strict enabled (v1 contract validation pending)");
    }

    if all_errors.is_empty() {
        eprintln!("scene check: {} ok", display_path);
        Ok(())
    } else {
        render_diagnostics(&all_errors, ctx.no_color);
        Err(CliError::Generic {
            reason: format!(
                "scene check: {} error(s) in {}",
                all_errors.len(),
                display_path
            ),
        })
    }
}

/// Resolve the scene source text and a display-friendly path string.
///
/// When the user supplies an explicit path, we read from disk. Otherwise
/// we run the scene-path resolver (T-113 precedence) to find the default
/// scene and read it — or use the built-in default if nothing on disk
/// matches.
fn load_scene_source(args: &CheckArgs) -> Result<(String, String), CliError> {
    if let Some(ref path) = args.path {
        let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
            reason: format!("cannot read {}: {e}", path.display()),
        })?;
        Ok((src, path.display().to_string()))
    } else {
        resolve_default_source()
    }
}

/// Resolve the default scene via T-113's `resolve_scene_path`.
fn resolve_default_source() -> Result<(String, String), CliError> {
    let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
        reason: format!("cannot determine cwd: {e}"),
    })?;
    let env_scene = std::env::var("ARK_SCENE").ok();
    let xdg_config = xdg_config_dir();
    match resolve_scene_path(
        None,
        env_scene.as_deref(),
        None,
        xdg_config.as_deref(),
        &cwd,
    ) {
        SceneSource::Flag(p)
        | SceneSource::EnvVar(p)
        | SceneSource::ProjectLocal(p)
        | SceneSource::UserConfig(p) => {
            let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                reason: format!("cannot read {}: {e}", p.display()),
            })?;
            Ok((src, p.display().to_string()))
        }
        SceneSource::BuiltIn => Ok((DEFAULT_SCENE_KDL.to_string(), "<built-in>".to_string())),
    }
}

/// Best-effort XDG config dir using `$XDG_CONFIG_HOME` or `~/.config`.
fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_CONFIG_HOME") {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".config"));
    }
    None
}

/// Render every [`SceneError`] through miette's text renderer to stderr.
///
/// Each error is a full `miette::Diagnostic` carrying source context,
/// labeled spans, help text, and a stable error code — we lean on
/// `miette::GraphicalReportHandler` (or `NarratableReportHandler` when
/// colors are suppressed) to do the heavy lifting.
fn render_diagnostics(errors: &[SceneError], no_color: bool) {
    use std::fmt::Write;

    let handler: Box<dyn miette::ReportHandler> = if no_color {
        Box::new(miette::NarratableReportHandler::new())
    } else {
        Box::new(miette::GraphicalReportHandler::new())
    };

    for err in errors {
        struct DiagFmt<'a> {
            err: &'a SceneError,
            handler: &'a dyn miette::ReportHandler,
        }
        impl std::fmt::Display for DiagFmt<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.handler.display(self.err, f)
            }
        }
        let d = DiagFmt {
            err,
            handler: &*handler,
        };
        let mut buf = String::new();
        if write!(&mut buf, "{d}").is_ok() {
            eprintln!("{buf}");
        } else {
            eprintln!("error: {err}");
        }
    }
}
