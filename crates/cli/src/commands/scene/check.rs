//! `ark scene check` — full parse + resolve + validate + CEL-compile.
//!
//! T-12.2 (cavekit-scene R13). Exit 0 on green; non-zero with
//! diagnostics on any error. Emits every error, not just first.

use std::path::{Path, PathBuf};

use clap::Args;

use ark_scene::error::SceneError;
use ark_scene::parse::parse_scene;
use ark_scene::path::{ResolvedScene, resolve_scene_path_from_env};
use ark_scene::v1_strict::v1_strict_validate;
use ark_scene::validate::validate_scene;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Path to a scene file. Validates the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Enforce v1.0 contract (T-15.3).
    #[arg(long)]
    pub v1_strict: bool,
}

pub fn run(args: CheckArgs, ctx: &Ctx) -> Result<(), CliError> {
    let (src, display_path) = load_scene_source(&args, ctx)?;
    let mut all_errors: Vec<SceneError> = Vec::new();

    // Phase 1: parse.
    let doc = match parse_scene(&src, Path::new(&display_path)) {
        Ok(doc) => Some(doc),
        Err(e) => {
            all_errors.push(e);
            None
        }
    };

    // Phase 2: validate (CEL predicates, templates, chord strings).
    // Only runs when parsing succeeded — otherwise the AST is absent.
    if let Some(ref doc) = doc {
        if let Err(mut errs) = validate_scene(doc) {
            all_errors.append(&mut errs);
        }
    }

    // Phase 3 (T-15.3): v1-strict gate. Only runs when --v1-strict is
    // set AND parsing succeeded — strict mode layers on top of the
    // normal check; a file that doesn't even parse has no shape to
    // enforce the contract against. Walks the raw KDL because op
    // names live on the KDL nodes rather than the typed AST (see
    // `ark_scene::v1_strict` module docs for the rationale).
    if args.v1_strict {
        if let Ok(kdl_doc) = src.parse::<kdl::KdlDocument>() {
            if let Err(mut errs) = v1_strict_validate(&src, Path::new(&display_path), &kdl_doc) {
                all_errors.append(&mut errs);
            }
        }
    }

    if all_errors.is_empty() {
        let suffix = if args.v1_strict { " (v1-strict)" } else { "" };
        eprintln!("scene check: {} ok{}", display_path, suffix);
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
/// we run the scene-path resolver (R13 precedence) to find the default
/// scene and read it — or use the built-in default if nothing on disk
/// matches.
fn load_scene_source(args: &CheckArgs, _ctx: &Ctx) -> Result<(String, String), CliError> {
    if let Some(ref path) = args.path {
        let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
            reason: format!("cannot read {}: {e}", path.display()),
        })?;
        Ok((src, path.display().to_string()))
    } else {
        let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
            reason: format!("cannot determine cwd: {e}"),
        })?;
        match resolve_scene_path_from_env(None, &cwd) {
            ResolvedScene::Named(name) => Err(CliError::Generic {
                reason: format!(
                    "scene `{name}` resolved by name; pass an explicit path to check"
                ),
            }),
            ResolvedScene::Path(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                Ok((src, p.display().to_string()))
            }
            ResolvedScene::BuiltIn(src) => Ok((src.to_string(), "<built-in>".to_string())),
        }
    }
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
        // miette's `ReportHandler::display` wants `fmt::Formatter`,
        // which we get by implementing `Display` on a thin wrapper.
        struct DiagFmt<'a> {
            err: &'a SceneError,
            handler: &'a dyn miette::ReportHandler,
        }
        impl std::fmt::Display for DiagFmt<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.handler.display(self.err, f)
            }
        }
        let d = DiagFmt { err, handler: &*handler };
        let mut buf = String::new();
        if write!(&mut buf, "{d}").is_ok() {
            eprintln!("{buf}");
        } else {
            eprintln!("error: {err}");
        }
    }
}
