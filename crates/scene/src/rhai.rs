//! Rhai expression engine wrapper (T-019..T-021).
//!
//! **STUB NOTE (T-022..T-025 packet)**: This file is a minimal stub
//! authored alongside the T-022/T-023/T-024/T-025 packet so that
//! `interp.rs`, `compile.rs`, and `context.rs` can import
//! [`Engine`], [`Program`], [`RhaiScope`], and [`compile_in_scope`]
//! without a circular blocking dependency on the parallel T-019 packet.
//!
//! The real T-019..T-021 implementation supersedes this file when it
//! lands; the merge should favour the parallel packet (richer API:
//! helper functions, scope-mismatch diagnostics, engine symbol
//! disables) while preserving the names imported by downstream
//! modules:
//!
//! - [`Engine`] — opaque handle around `rhai::Engine`.
//! - [`Program`] — a compiled Rhai AST + scope-kind tag.
//! - [`RhaiScope`] — the two-scope enum (`Spawn` / `Event`, R8).
//! - [`compile_in_scope`] — parse a source string into a [`Program`].
//!
//! See `context/plans/build-site-scene.md` T-019 for the full spec.

use crate::error::SceneError;
use miette::{NamedSource, SourceSpan};

/// Engine configuration kinds available at the scene level.
///
/// Scenes wire every Rhai program to one of these scopes. Layout
/// values (`cwd`, `name`, `when=` on tabs / panes / rows / cols) are
/// attached to [`RhaiScope::Spawn`]; reaction predicates and op
/// bodies (`on <Kind> { … }`, `bind "<chord>" { … }`) are attached
/// to [`RhaiScope::Event`]. Evaluation at runtime rejects mismatched
/// scopes with `error[scene/rhai-scope-mismatch]` (T-020).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RhaiScope {
    /// Layout-time values: `cwd`, `id`, `name`, `env`.
    Spawn,
    /// Event-time values: `event`, `payload`, `agent`, `session`,
    /// plus selector-captured locals.
    Event,
}

/// Thin wrapper over a shared, resource-limited `rhai::Engine`.
///
/// Constructed via [`Engine::new`] (stub: default engine). The real
/// T-019 engine is built via `Engine::new_raw()` with symbols
/// (`fn`/`while`/`for`/`loop`/`return`/`break`/`continue`/`=`/…)
/// disabled and limits set per R8.
#[derive(Debug)]
pub struct Engine {
    inner: rhai::Engine,
}

impl Engine {
    /// Build a new engine. Stub: delegates to `rhai::Engine::new()`.
    /// T-019 replaces this with an `Engine::new_raw()`-based build
    /// that drops the full stdlib and applies R8 limits.
    pub fn new() -> Self {
        let mut inner = rhai::Engine::new();
        // Conservative limits mirroring R8 so the stub doesn't let
        // hostile scenes DoS the test harness.
        inner.set_max_expr_depths(32, 32);
        inner.set_max_operations(10_000);
        inner.set_max_string_size(4096);
        inner.set_max_array_size(256);
        inner.set_max_map_size(256);
        Self { inner }
    }

    /// Borrow the underlying engine for direct `rhai::Engine` calls.
    pub fn inner(&self) -> &rhai::Engine {
        &self.inner
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// Compiled Rhai program — AST + scope-kind tag.
///
/// Cloneable so the same compiled program can be stashed in a
/// [`crate::compile::CompiledScene`] collection and rerendered
/// on each reaction fire / reconciler pass without re-parsing.
#[derive(Debug, Clone)]
pub struct Program {
    /// Raw source text (preserved for diagnostics).
    pub src: String,
    /// Scope this program is legal in.
    pub scope: RhaiScope,
    /// Compiled AST.
    pub ast: rhai::AST,
}

/// Compile `src` in the given [`RhaiScope`] as a Rhai expression.
///
/// Parse failures surface as [`SceneError::RhaiParse`] with an empty
/// source-attribution stub (real attribution lives in the caller via
/// the KDL attribute span). Callers that want a source-located
/// diagnostic should map the returned error's `message` onto their
/// own span (`compile.rs` does this).
#[allow(clippy::result_large_err)]
pub fn compile_in_scope(
    engine: &Engine,
    src: &str,
    scope: RhaiScope,
) -> Result<Program, SceneError> {
    match engine.inner.compile_expression(src) {
        Ok(ast) => Ok(Program {
            src: src.to_string(),
            scope,
            ast,
        }),
        Err(err) => Err(SceneError::RhaiParse {
            message: err.to_string(),
            src: NamedSource::new("<expr>", src.to_string()),
            span: SourceSpan::new(0.into(), src.len().min(1)),
        }),
    }
}

/// Evaluate a compiled program for a `bool` result.
///
/// Used by `when=` predicates (T-023, T-060). Runtime errors surface
/// as [`SceneError::RhaiEval`]; operation-limit overrun as
/// [`SceneError::RhaiOom`].
#[allow(clippy::result_large_err)]
pub fn eval_bool(
    engine: &Engine,
    program: &Program,
    scope: &mut rhai::Scope,
) -> Result<bool, SceneError> {
    engine
        .inner
        .eval_ast_with_scope::<bool>(scope, &program.ast)
        .map_err(|err| SceneError::RhaiEval {
            message: err.to_string(),
        })
}

/// Evaluate a compiled program for a dynamic result.
///
/// Used by `{Rhai}` hole expansion (T-022). Runtime errors surface
/// as [`SceneError::RhaiEval`].
#[allow(clippy::result_large_err)]
pub fn eval_value(
    engine: &Engine,
    program: &Program,
    scope: &mut rhai::Scope,
) -> Result<rhai::Dynamic, SceneError> {
    engine
        .inner
        .eval_ast_with_scope::<rhai::Dynamic>(scope, &program.ast)
        .map_err(|err| SceneError::RhaiEval {
            message: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_builds() {
        let _engine = Engine::new();
    }

    #[test]
    fn compile_simple_expr() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 + 1", RhaiScope::Spawn)
            .expect("constant expression should compile");
        assert_eq!(p.scope, RhaiScope::Spawn);
        assert_eq!(p.src, "1 + 1");
    }

    #[test]
    fn compile_rejects_garbage() {
        let engine = Engine::new();
        let err = compile_in_scope(&engine, "1 +", RhaiScope::Event)
            .expect_err("truncated expression must reject");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn eval_bool_true_false() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "true", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut scope).unwrap());

        let p = compile_in_scope(&engine, "1 == 2", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        assert!(!eval_bool(&engine, &p, &mut scope).unwrap());
    }

    #[test]
    fn eval_value_preserves_int() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "40 + 2", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        let v = eval_value(&engine, &p, &mut scope).unwrap();
        assert_eq!(v.as_int().unwrap(), 42);
    }
}
