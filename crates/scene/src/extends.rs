//! `extends "<parent-scene>"` resolution (T-9.1, R11).
//!
//! The `extends` keyword inherits a base scene by NAME. Resolution walks
//! a dedicated **scene-search-path** that is distinct from the
//! extension-search-path driving `use "<name>"` (T-10.4). The rungs are,
//! in precedence order:
//!
//! 1. `<cwd>/.ark/scenes/<name>.kdl` — project-local base scenes.
//! 2. `$XDG_CONFIG_HOME/<appname>/scenes/<name>.kdl` — user-global base
//!    scenes. `<appname>` defaults to `ark`, overridable via
//!    `$ARK_APPNAME` (mirrors [`crate::path::resolve_scene_path`]).
//! 3. Built-in shipped scenes — a small registry of static `(name,
//!    kdl_source)` pairs baked into the binary. Empty at the current
//!    tier; future built-ins plug in by extending
//!    [`SceneSearchCtx::builtins`].
//!
//! [`resolve_extends_path`] is the pure function that probes each rung
//! deterministically. [`load_extends`] is the convenience wrapper that
//! reads + parses the parent scene file and returns the resulting
//! [`SceneDoc`]. Both surfaces are side-effect-free beyond the direct
//! filesystem read.
//!
//! The grammar-level "one `extends` per scene" constraint is enforced
//! by [`ensure_single_extends`], which walks the raw KDL document
//! (`facet-kdl` silently accepts a second `extends` through the
//! `#[facet(kdl::child)]` field because the repeat-check is done at
//! semantic time — see [`crate::ast::SceneNode`]).

use std::path::{Path, PathBuf};

use kdl::KdlDocument;
use miette::NamedSource;

use crate::ast::SceneDoc;
use crate::error::SceneError;
use crate::parse::parse_scene;
use crate::path::DEFAULT_APPNAME;

/// Context passed to [`resolve_extends_path`] — mirrors the inputs
/// consumed by [`crate::path::resolve_scene_path`] but trimmed to the
/// fields the `extends` resolver actually reads.
///
/// Keeping this as a separate struct avoids a giant positional arg list
/// at every call site and makes future extension (`system_dirs`,
/// profile-scoped overlays) a non-breaking addition.
#[derive(Debug, Clone)]
pub struct SceneSearchCtx {
    /// Session CWD — rung 1 is rooted at `<cwd>/.ark/scenes/`.
    pub cwd: PathBuf,

    /// `$XDG_CONFIG_HOME` when set (caller injects). `None` skips rung
    /// 2 entirely — the resolver does NOT fall back to `$HOME/.config`
    /// on its own; that expansion belongs to the env-reading
    /// convenience wrapper in [`crate::path`].
    pub xdg_config_home: Option<PathBuf>,

    /// `<appname>` slug under `$XDG_CONFIG_HOME`. Defaults to
    /// [`DEFAULT_APPNAME`] when the wrapping CLI does not override it.
    pub appname: String,

    /// Built-in shipped scenes. Each entry is `(scene-name,
    /// kdl-source)`. Empty in v0.1 — future tiers can plug defaults in
    /// here without touching the resolver logic.
    pub builtins: Vec<(&'static str, &'static str)>,
}

impl SceneSearchCtx {
    /// Convenience constructor: no builtins, appname = `"ark"`, no
    /// XDG path. Callers that already have explicit values should
    /// build the struct directly.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            xdg_config_home: None,
            appname: DEFAULT_APPNAME.to_string(),
            builtins: Vec::new(),
        }
    }
}

/// Outcome of [`resolve_extends_path`].
///
/// Mirrors [`crate::path::ResolvedScene`] but scoped to the
/// extends-resolver's output: the caller either got a concrete file
/// path (rungs 1 + 2) or a built-in (rung 3). There is no
/// `ResolvedScene::Named` equivalent here — `extends` is always a name
/// lookup, and rungs 1 + 2 always produce a file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedExtends {
    /// Concrete file path on disk (rungs 1 + 2). Caller should
    /// `std::fs::read_to_string(&path)` then feed through
    /// [`crate::parse::parse_scene`].
    Path(PathBuf),

    /// Built-in scene whose KDL source is baked into the binary
    /// (rung 3).
    BuiltIn {
        /// Scene name as registered in [`SceneSearchCtx::builtins`].
        name: &'static str,
        /// KDL source text of the built-in scene.
        source: &'static str,
    },
}

/// Resolve `extends "<parent_name>"` through the scene-search-path.
///
/// Pure function: no env reads, no `std::env::current_dir`. Every rung
/// that requires filesystem state takes it as an argument, so unit
/// tests exercise every branch with tempdirs and the resolver itself
/// has no I/O contract beyond `Path::is_file`.
///
/// Errors with [`SceneError::ExtendsNotFound`] when no rung resolves,
/// populating the `searched` vector with every candidate path that
/// was probed so the diagnostic points the user at the exact failure.
#[allow(clippy::result_large_err)] // SceneError is intentionally fat for diagnostics.
pub fn resolve_extends_path(
    parent_name: &str,
    ctx: &SceneSearchCtx,
) -> Result<ResolvedExtends, SceneError> {
    let mut searched: Vec<String> = Vec::with_capacity(3);

    // Rung 1: project-local `./.ark/scenes/<name>.kdl`.
    let project_local = ctx
        .cwd
        .join(".ark")
        .join("scenes")
        .join(format!("{parent_name}.kdl"));
    searched.push(project_local.display().to_string());
    if project_local.is_file() {
        return Ok(ResolvedExtends::Path(project_local));
    }

    // Rung 2: `$XDG_CONFIG_HOME/<appname>/scenes/<name>.kdl`.
    if let Some(xdg) = ctx.xdg_config_home.as_ref() {
        let user_global = xdg
            .join(&ctx.appname)
            .join("scenes")
            .join(format!("{parent_name}.kdl"));
        searched.push(user_global.display().to_string());
        if user_global.is_file() {
            return Ok(ResolvedExtends::Path(user_global));
        }
    }

    // Rung 3: built-in registry.
    for (name, source) in &ctx.builtins {
        if *name == parent_name {
            return Ok(ResolvedExtends::BuiltIn {
                name,
                source,
            });
        }
    }
    searched.push(format!("<built-in:{parent_name}>"));

    Err(SceneError::ExtendsNotFound {
        name: parent_name.to_string(),
        searched,
    })
}

/// Load, read, and parse the parent scene referenced by `doc`'s
/// `extends` clause (if any).
///
/// Returns `Ok(None)` when `doc` does not declare an `extends`. Returns
/// `Ok(Some((parent_doc, parent_path)))` on a successful resolve + parse;
/// `parent_path` is the filesystem path the parent came from (or a
/// synthetic `<built-in:...>` path for rung 3). Errors surface as
/// [`SceneError::ExtendsNotFound`] (no rung matched), [`SceneError::Parse`]
/// (parent file exists but is malformed), or [`SceneError::Grammar`]
/// (I/O / UTF-8 failures — same mapping the compile pipeline already
/// uses for read errors).
#[allow(clippy::result_large_err)]
pub fn load_extends(
    doc: &SceneDoc,
    ctx: &SceneSearchCtx,
) -> Result<Option<(SceneDoc, PathBuf)>, SceneError> {
    let Some(extends) = doc.scene.extends.as_ref() else {
        return Ok(None);
    };
    let resolved = resolve_extends_path(&extends.parent, ctx)?;

    match resolved {
        ResolvedExtends::Path(path) => {
            let src = read_scene_file(&path)?;
            let parsed = parse_scene(&src, &path)?;
            Ok(Some((parsed, path)))
        }
        ResolvedExtends::BuiltIn { name, source } => {
            // Synthetic path keeps downstream diagnostics' span
            // surface consistent — `parse_scene` uses the path in its
            // `NamedSource` constructor regardless of whether the
            // bytes came from disk or from memory.
            let synthetic = PathBuf::from(format!("<built-in:{name}>"));
            let parsed = parse_scene(source, &synthetic)?;
            Ok(Some((parsed, synthetic)))
        }
    }
}

/// Read a scene file off disk, returning [`SceneError::Grammar`] on
/// I/O failure or UTF-8 conversion failure.
///
/// Mirrors the mapping used by
/// [`crate::compile::compile_scene_file`] — keeps the error shape
/// consistent across the pipeline so callers only have to match on
/// one variant.
#[allow(clippy::result_large_err)]
fn read_scene_file(path: &Path) -> Result<String, SceneError> {
    let bytes = std::fs::read(path).map_err(|e| SceneError::Grammar {
        message: format!("read scene `{}`: {e}", path.display()),
        src: NamedSource::new(path.display().to_string(), String::new()),
        at: (0, 0).into(),
    })?;
    String::from_utf8(bytes).map_err(|e| SceneError::Grammar {
        message: format!("scene `{}` is not valid utf-8: {e}", path.display()),
        src: NamedSource::new(path.display().to_string(), String::new()),
        at: (0, 0).into(),
    })
}

/// Walk the raw KDL document and surface [`SceneError::MultipleExtends`]
/// when more than one `extends` child is present under the `scene { … }`
/// body.
///
/// facet-kdl silently retains only the LAST `extends` for the typed
/// AST's single-child slot (see [`crate::ast::SceneNode::extends`]), so
/// a scene with two `extends` clauses would otherwise parse cleanly
/// and silently lose the first parent. The R11 spec explicitly allows
/// only one `extends` per scene, so we detect the duplicate on the raw
/// KDL tree before the compile pipeline dispatches resolution.
///
/// Invoke with the verbatim source text + the file path — matches
/// [`crate::scope::check_scope`]'s signature so wiring into the
/// compile pipeline is mechanical.
#[allow(clippy::result_large_err)]
pub fn ensure_single_extends(src: &str, path: &Path) -> Result<(), SceneError> {
    // Best-effort: rely on the KDL 2.0 parser. If the document
    // doesn't parse at all we let the main parse path surface the
    // error; this function is only called after `parse_scene`
    // succeeds, so the re-parse is effectively guaranteed to work.
    let Ok(doc) = KdlDocument::parse(src) else {
        return Ok(());
    };

    // Find the `scene` root.
    let Some(scene) = doc.nodes().iter().find(|n| n.name().value() == "scene") else {
        return Ok(());
    };

    let Some(body) = scene.children() else {
        return Ok(());
    };

    let extends_nodes: Vec<_> = body
        .nodes()
        .iter()
        .filter(|n| n.name().value() == "extends")
        .collect();

    if extends_nodes.len() > 1 {
        let first = extends_nodes[0].name().span();
        let second = extends_nodes[1].name().span();
        return Err(SceneError::MultipleExtends {
            src: NamedSource::new(path.display().to_string(), src.to_string()),
            first,
            second,
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn ctx(cwd: &Path) -> SceneSearchCtx {
        SceneSearchCtx::new(cwd)
    }

    /// Rung 1: project-local `./.ark/scenes/<name>.kdl` wins when
    /// present.
    #[test]
    fn rung1_project_local_wins() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let scenes_dir = cwd.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        let parent = scenes_dir.join("base.kdl");
        fs::write(&parent, r#"scene "base""#).unwrap();

        let resolved = resolve_extends_path("base", &ctx(cwd)).expect("resolve rung 1");
        assert_eq!(resolved, ResolvedExtends::Path(parent));
    }

    /// Rung 2: XDG fallback — fires when rung 1 has no match.
    #[test]
    fn rung2_xdg_fallback_when_project_empty() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();
        let xdg_scene = xdg.path().join("ark/scenes/base.kdl");
        fs::create_dir_all(xdg_scene.parent().unwrap()).unwrap();
        fs::write(&xdg_scene, r#"scene "base""#).unwrap();

        let mut c = SceneSearchCtx::new(cwd);
        c.xdg_config_home = Some(xdg.path().to_path_buf());

        let resolved = resolve_extends_path("base", &c).expect("resolve rung 2");
        assert_eq!(resolved, ResolvedExtends::Path(xdg_scene));
    }

    /// Rung 2 honours the `appname` override so multiple flavours of
    /// the host binary can share `$XDG_CONFIG_HOME`.
    #[test]
    fn rung2_honours_appname_override() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();
        let xdg_scene = xdg.path().join("custom/scenes/base.kdl");
        fs::create_dir_all(xdg_scene.parent().unwrap()).unwrap();
        fs::write(&xdg_scene, r#"scene "base""#).unwrap();

        let c = SceneSearchCtx {
            cwd: cwd.to_path_buf(),
            xdg_config_home: Some(xdg.path().to_path_buf()),
            appname: "custom".to_string(),
            builtins: Vec::new(),
        };

        let resolved = resolve_extends_path("base", &c).expect("resolve rung 2");
        assert_eq!(resolved, ResolvedExtends::Path(xdg_scene));
    }

    /// Rung 3: built-in registry fires when disk rungs come up empty.
    #[test]
    fn rung3_builtin_fallback() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let mut c = SceneSearchCtx::new(cwd);
        c.builtins = vec![("shipped", r#"scene "shipped""#)];

        let resolved = resolve_extends_path("shipped", &c).expect("resolve rung 3");
        assert_eq!(
            resolved,
            ResolvedExtends::BuiltIn {
                name: "shipped",
                source: r#"scene "shipped""#,
            }
        );
    }

    /// No rung matches → `SceneError::ExtendsNotFound` with `searched`
    /// populated by every probed candidate.
    #[test]
    fn unresolved_extends_reports_all_searched_paths() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let xdg = TempDir::new().unwrap();

        let mut c = SceneSearchCtx::new(cwd);
        c.xdg_config_home = Some(xdg.path().to_path_buf());

        let err = resolve_extends_path("missing", &c).expect_err("must not resolve");
        match err {
            SceneError::ExtendsNotFound { name, searched } => {
                assert_eq!(name, "missing");
                assert_eq!(searched.len(), 3, "probed rungs 1+2+3: {searched:?}");
                assert!(searched[0].contains(".ark/scenes/missing.kdl"));
                assert!(searched[1].contains("ark/scenes/missing.kdl"));
                assert!(searched[2].contains("<built-in:missing>"));
            }
            other => panic!("expected ExtendsNotFound, got {other:?}"),
        }
    }

    /// Rung 1 wins over rung 2 when both contain the same name.
    #[test]
    fn rung1_wins_over_rung2() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let local = cwd.join(".ark/scenes/base.kdl");
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&local, r#"scene "local""#).unwrap();

        let xdg = TempDir::new().unwrap();
        let xdg_scene = xdg.path().join("ark/scenes/base.kdl");
        fs::create_dir_all(xdg_scene.parent().unwrap()).unwrap();
        fs::write(&xdg_scene, r#"scene "xdg""#).unwrap();

        let mut c = SceneSearchCtx::new(cwd);
        c.xdg_config_home = Some(xdg.path().to_path_buf());

        let resolved = resolve_extends_path("base", &c).expect("resolve");
        assert_eq!(resolved, ResolvedExtends::Path(local));
    }

    /// `load_extends` returns `None` when the child has no `extends`.
    #[test]
    fn load_extends_skips_scene_without_parent() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let src = r#"scene "only-child""#;
        let doc = parse_scene(src, &cwd.join("child.kdl")).unwrap();
        let result = load_extends(&doc, &ctx(cwd)).expect("no-op returns None");
        assert!(result.is_none());
    }

    /// `load_extends` reads + parses the resolved parent file.
    #[test]
    fn load_extends_reads_and_parses_rung1_parent() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let scenes_dir = cwd.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(scenes_dir.join("base.kdl"), r#"scene "base""#).unwrap();

        let child_src = r#"
scene "child" {
    extends "base"
}
"#;
        let child = parse_scene(child_src, &cwd.join("child.kdl")).unwrap();
        let (parent_doc, parent_path) = load_extends(&child, &ctx(cwd))
            .expect("load parent")
            .expect("parent present");
        assert_eq!(parent_doc.scene.name, "base");
        assert!(parent_path.ends_with("base.kdl"));
    }

    /// `load_extends` surfaces `ExtendsNotFound` when the parent name
    /// doesn't resolve.
    #[test]
    fn load_extends_surfaces_not_found() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let child_src = r#"
scene "child" {
    extends "ghost"
}
"#;
        let child = parse_scene(child_src, &cwd.join("child.kdl")).unwrap();
        let err = load_extends(&child, &ctx(cwd)).expect_err("must fail");
        assert!(matches!(err, SceneError::ExtendsNotFound { .. }));
    }

    /// `ensure_single_extends` accepts zero or one `extends` clauses.
    #[test]
    fn ensure_single_extends_accepts_zero_or_one() {
        let zero = r#"scene "x""#;
        let one = r#"
scene "x" {
    extends "base"
}
"#;
        assert!(ensure_single_extends(zero, Path::new("x.kdl")).is_ok());
        assert!(ensure_single_extends(one, Path::new("x.kdl")).is_ok());
    }

    /// `ensure_single_extends` rejects two or more `extends` clauses.
    #[test]
    fn ensure_single_extends_rejects_duplicates() {
        let src = r#"
scene "x" {
    extends "base"
    extends "other"
}
"#;
        let err = ensure_single_extends(src, Path::new("x.kdl"))
            .expect_err("duplicate extends must error");
        assert!(matches!(err, SceneError::MultipleExtends { .. }));
    }
}
