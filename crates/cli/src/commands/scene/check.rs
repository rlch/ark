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

use ark_scene::ast::layout::{ColNode, LayoutChild, RowNode, StackNode, TabNode};
use ark_scene::ast::{LayoutNode, ModeNode, SceneBodyNode};
use ark_scene::compile::{CompiledScene, compile_scene, compile_scene_with_registry};
use ark_scene::compose::compose_scene;
use ark_scene::default_scene::DEFAULT_SCENE_KDL;
use ark_scene::error::SceneError;
use ark_scene::parse::{SceneIR, parse_scene};
use ark_scene::resolve_path::{SceneSource, resolve_scene_path};
use ark_scene::rhai::Engine;
use ark_scene::shape::detect_and_normalize;
use ark_scene::validate::{handles, op_refs, pane_views, scope, validate_view_types};
use ark_scene::view::ViewRegistry;

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
    //
    // Under `--v1-strict` we route through `compile_scene_with_registry`
    // so phase 6 can consume the resulting `CompiledScene` to cross-check
    // view-types. In the non-strict path we keep the lighter
    // `compile_scene` wrapper (identical semantics; primitives-only
    // registry) to preserve the existing code path.
    let mut compiled: Option<CompiledScene> = None;
    // `strict_ir` holds onto the parsed IR ONLY when compile was skipped
    // so phase 6 can still run structural checks on the raw AST. When
    // compile succeeded we reach the IR via `compiled.ir` instead.
    let mut strict_ir: Option<SceneIR> = None;
    if let Some(ir) = ir {
        if all_errors.is_empty() {
            let engine = Engine::new();
            if args.v1_strict {
                let registry = ViewRegistry::with_primitives();
                match compile_scene_with_registry(&engine, ir, &registry) {
                    Ok(cs) => compiled = Some(cs),
                    Err(e) => all_errors.push(e),
                }
            } else {
                match compile_scene(&engine, ir) {
                    Ok(cs) => compiled = Some(cs),
                    Err(e) => all_errors.push(e),
                }
            }
        } else {
            // Compile skipped: retain the IR so phase 6's structural
            // checks can still run against the partial parse result.
            strict_ir = Some(ir);
        }
    }

    // Phase 6: v1-strict validation (T-135, scene-v3 S-C).
    //
    // Only fires under `--v1-strict`. Checks that go beyond the default
    // validator passes — publishable-scene gates that would be too
    // strict for authoring-time `ark scene check`:
    //
    // 1. scene/strict/empty-layout — scene has no tabs (no `layout` block
    //    or a `layout { }` with zero tabs). Signals a no-op scene.
    // 2. scene/strict/empty-tab — a tab whose body has zero children.
    //    Signals an unfinished layout.
    // 3. scene/strict/empty-container — a `row { }` or `col { }` with no
    //    children. Empty `stack { }` is intentionally allowed (dynamic
    //    population via `spawn_into`, scene-2026-04-18 T-022).
    // 4. scene/view-type-mismatch — runs `validate_view_types` against
    //    the compiled scene's view table (R-8 homogeneous-only stack
    //    check). Currently not wired into the default pipeline; strict
    //    mode surfaces it.
    //
    // Each fired rule adds one diagnostic to `strict_errors`. The final
    // count is logged so authors see how many default warnings the
    // strict gate elevated.
    let mut strict_errors: Vec<StrictDiag> = Vec::new();
    if args.v1_strict {
        if let Some(ref cs) = compiled {
            strict_layout_pass(&cs.ir, &mut strict_errors);
            let registry = ViewRegistry::with_primitives();
            for err in validate_view_types(cs, &registry) {
                strict_errors.push(StrictDiag::ViewTypes(err));
            }
        } else if let Some(ref ir) = strict_ir {
            // Compile was skipped upstream (prior errors); still run the
            // structural checks against the raw IR so authors see ALL
            // strict violations in one pass.
            strict_layout_pass(ir, &mut strict_errors);
        }
    }

    // Render parse/validate/compile errors first, then strict-mode
    // errors. Strict-mode errors get a visible header so authors know
    // which diagnostics the `--v1-strict` gate is responsible for.
    if !all_errors.is_empty() {
        render_diagnostics(&all_errors, ctx.no_color);
    }
    if !strict_errors.is_empty() {
        eprintln!(
            "scene check: {} warning(s) elevated to errors under --v1-strict",
            strict_errors.len()
        );
        render_strict_diagnostics(&strict_errors, ctx.no_color);
    }

    let total = all_errors.len() + strict_errors.len();
    if total == 0 {
        eprintln!("scene check: {} ok", display_path);
        Ok(())
    } else {
        Err(CliError::Generic {
            reason: format!("scene check: {} error(s) in {}", total, display_path),
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

// ---------------------------------------------------------------------------
// T-135 — v1-strict mode (scene-v3 S-C)
// ---------------------------------------------------------------------------

/// A single strict-mode violation. Either a reused crate-level
/// [`SceneError`] (currently only [`SceneError::ViewTypeMismatch`] from
/// `validate_view_types`) or a strict-only sanity-check carrying a
/// stable diagnostic code + human message.
///
/// Split into two variants because the crate-level errors carry
/// `miette::Diagnostic` derives with source spans, while the
/// strict-only checks today operate on AST shape with no single source
/// span to pin a caret to (they describe structural gaps, not token
/// positions).
#[derive(Debug)]
enum StrictDiag {
    /// Strict-mode empty-layout / empty-tab / empty-row / empty-col
    /// check. Carries the stable `scene/strict/<kind>` diagnostic code
    /// and a human message.
    Structural {
        /// Stable diagnostic code, e.g. `scene/strict/empty-layout`.
        code: &'static str,
        /// Human-readable message suffix.
        message: String,
    },
    /// Re-surfaced [`SceneError`] from a validator that only runs in
    /// strict mode (today: `validate_view_types`).
    ViewTypes(SceneError),
}

/// Walk the scene body and emit structural strict-only diagnostics.
///
/// Today's coverage:
///
/// 1. `scene/strict/empty-layout` — no `layout` block OR the block has
///    zero tabs. A publishable scene should always render at least one
///    tab; zero-tab scenes are almost always unfinished drafts.
/// 2. `scene/strict/empty-tab` — a tab whose body contains zero
///    layout children. Same rationale — an empty tab is an
///    unfinished draft, not a publishable shape.
/// 3. `scene/strict/empty-container` — a nested `row` / `col` / `stack`
///    with zero children. Same rationale.
///
/// Modes (`mode "<name>" { … }`) are walked with the same rules so
/// alternate whole-tab layouts face the same gates.
fn strict_layout_pass(ir: &SceneIR, errors: &mut Vec<StrictDiag>) {
    let mut has_layout = false;
    let mut layout_tab_count: usize = 0;

    for node in &ir.scene.body {
        match node {
            SceneBodyNode::Layout(layout) => {
                has_layout = true;
                layout_tab_count += layout.tabs.len();
                walk_layout(layout, errors);
            }
            SceneBodyNode::Mode(mode) => {
                walk_mode(mode, errors);
            }
            _ => {}
        }
    }

    // Rule 1: empty-layout.
    if !has_layout || layout_tab_count == 0 {
        errors.push(StrictDiag::Structural {
            code: "scene/strict/empty-layout",
            message: "scene declares no tabs in its `layout { }` block".to_string(),
        });
    }
}

fn walk_layout(layout: &LayoutNode, errors: &mut Vec<StrictDiag>) {
    for tab in &layout.tabs {
        walk_tab(tab, errors);
    }
}

fn walk_mode(mode: &ModeNode, errors: &mut Vec<StrictDiag>) {
    for tab in &mode.tabs {
        walk_tab(tab, errors);
    }
}

fn walk_tab(tab: &TabNode, errors: &mut Vec<StrictDiag>) {
    if tab.body.is_empty() {
        errors.push(StrictDiag::Structural {
            code: "scene/strict/empty-tab",
            message: format!("tab `{}` has no layout children", tab.handle),
        });
        return;
    }
    for child in &tab.body {
        walk_layout_child(child, errors);
    }
}

fn walk_layout_child(child: &LayoutChild, errors: &mut Vec<StrictDiag>) {
    match child {
        LayoutChild::Row(row) => walk_row(row, errors),
        LayoutChild::Col(col) => walk_col(col, errors),
        LayoutChild::Stack(stack) => walk_stack(stack, errors),
        // Panes are leaves; their emptiness is fine (a pane renders a
        // view, not child panes). Other variants (extensions etc.)
        // carry their own children semantics we don't gate here.
        _ => {}
    }
}

fn walk_row(row: &RowNode, errors: &mut Vec<StrictDiag>) {
    if row.body.is_empty() {
        errors.push(StrictDiag::Structural {
            code: "scene/strict/empty-container",
            message: "`row { }` has no children".to_string(),
        });
        return;
    }
    for child in &row.body {
        walk_layout_child(child, errors);
    }
}

fn walk_col(col: &ColNode, errors: &mut Vec<StrictDiag>) {
    if col.body.is_empty() {
        errors.push(StrictDiag::Structural {
            code: "scene/strict/empty-container",
            message: "`col { }` has no children".to_string(),
        });
        return;
    }
    for child in &col.body {
        walk_layout_child(child, errors);
    }
}

fn walk_stack(stack: &StackNode, errors: &mut Vec<StrictDiag>) {
    // A `stack { }` with zero child panes is a valid shape at the AST
    // level — it represents an empty-body source stack whose first
    // `spawn_into` populates the runtime list (scene-2026-04-18 T-022).
    // Do NOT emit `scene/strict/empty-container` for stacks; that would
    // reject the canonical empty-source-stack pattern.
    for child in &stack.body {
        walk_layout_child(child, errors);
    }
}

/// Render strict-mode diagnostics to stderr.
///
/// Structural checks carry no source span, so we emit a plain
/// `error[<code>]: <message>` line. View-type mismatches go through
/// the same miette pipeline as regular [`SceneError`] renders so the
/// caret-and-help UI is preserved.
fn render_strict_diagnostics(errors: &[StrictDiag], no_color: bool) {
    use std::fmt::Write;

    let handler: Box<dyn miette::ReportHandler> = if no_color {
        Box::new(miette::NarratableReportHandler::new())
    } else {
        Box::new(miette::GraphicalReportHandler::new())
    };

    for err in errors {
        match err {
            StrictDiag::Structural { code, message } => {
                eprintln!("error[{code}]: {message}");
            }
            StrictDiag::ViewTypes(inner) => {
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
                    err: inner,
                    handler: &*handler,
                };
                let mut buf = String::new();
                if write!(&mut buf, "{d}").is_ok() {
                    eprintln!("{buf}");
                } else {
                    eprintln!("error: {inner}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — T-135 strict-mode sanity checks.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ark_scene::parse::parse_scene;

    fn ir_from(src: &str) -> SceneIR {
        parse_scene(src, "<test>").expect("parse ok")
    }

    #[test]
    fn strict_passes_on_default_scene() {
        let ir = ir_from(DEFAULT_SCENE_KDL);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        assert!(
            errors.is_empty(),
            "default scene must pass strict: {:?}",
            errors.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>()
        );
    }

    #[test]
    fn strict_rejects_no_layout_block() {
        let src = r#"scene "empty" { }"#;
        let ir = ir_from(src);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        assert_eq!(errors.len(), 1);
        let StrictDiag::Structural { code, .. } = &errors[0] else {
            panic!("expected Structural");
        };
        assert_eq!(*code, "scene/strict/empty-layout");
    }

    #[test]
    fn strict_rejects_empty_layout_block() {
        let src = r#"scene "empty" { layout { } }"#;
        let ir = ir_from(src);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        assert_eq!(errors.len(), 1);
        let StrictDiag::Structural { code, .. } = &errors[0] else {
            panic!("expected Structural");
        };
        assert_eq!(*code, "scene/strict/empty-layout");
    }

    #[test]
    fn strict_rejects_empty_tab_body() {
        let src = r#"
            scene "empty-tab" {
                layout {
                    tab "@main" { }
                }
            }
        "#;
        let ir = ir_from(src);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        assert_eq!(errors.len(), 1);
        let StrictDiag::Structural { code, message } = &errors[0] else {
            panic!("expected Structural");
        };
        assert_eq!(*code, "scene/strict/empty-tab");
        assert!(message.contains("@main"));
    }

    #[test]
    fn strict_rejects_empty_row_container() {
        let src = r#"
            scene "empty-row" {
                layout {
                    tab "@main" {
                        row { }
                    }
                }
            }
        "#;
        let ir = ir_from(src);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        // One empty-container diagnostic for the row.
        let codes: Vec<&str> = errors
            .iter()
            .filter_map(|e| match e {
                StrictDiag::Structural { code, .. } => Some(*code),
                _ => None,
            })
            .collect();
        assert!(
            codes.contains(&"scene/strict/empty-container"),
            "expected empty-container, got: {codes:?}"
        );
    }

    #[test]
    fn strict_accepts_empty_source_stack() {
        // Empty `stack { }` is the canonical empty-source-stack shape
        // (scene-2026-04-18 T-022). Strict mode must NOT reject it.
        let src = r#"
            scene "stack-ok" {
                layout {
                    tab "@main" {
                        stack "@subs" { }
                    }
                }
            }
        "#;
        let ir = ir_from(src);
        let mut errors: Vec<StrictDiag> = Vec::new();
        strict_layout_pass(&ir, &mut errors);
        assert!(
            errors.is_empty(),
            "empty-source stack must pass strict: {errors:?}"
        );
    }
}
