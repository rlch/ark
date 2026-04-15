//! Thin wrapper over `cel-interpreter` for scene `when=` / `if=`
//! predicates.
//!
//! Scene reactions, keybinds, conditional ops (`if=`), and engine
//! gating all consume CEL expressions (see `cavekit-scene.md` R4 +
//! R8). This module centralises the compile / eval API so every
//! caller surfaces the same `cel/*` miette diagnostics.
//!
//! # Surface
//!
//! - [`Program`] — opaque alias for `cel_interpreter::Program`.
//! - [`Context`] — re-export of `cel_interpreter::Context` built in
//!   [`crate::context::build_context`] (T-2.2). Carries the
//!   `'a` lifetime because `Child` contexts borrow their parent
//!   scope.
//! - [`compile`] — parse and cache an expression, translating
//!   `ParseErrors` into [`SceneError::CelParse`].
//! - [`eval`] — evaluate a compiled program against a context,
//!   translating `ExecutionError` into [`SceneError::CelEvaluate`].
//! - [`eval_bool`] — convenience for predicate call sites: evaluates
//!   and enforces a boolean result (a non-bool yields
//!   [`SceneError::CelEvaluate`]).
//!
//! The wrapper is intentionally stateless. `Program`s are cheap to
//! clone (they're reference-counted under the hood), and callers
//! typically build one per `when=`/`if=` string at scene-check time
//! (T-2.6) and reuse it across every matching event.
//!
//! # Diagnostics
//!
//! Compile failures attach the original source and a span covering
//! the expression, so `ark scene check` renders a caret under the
//! offending token. Evaluation failures ride a plain string — the
//! event context is reconstructed by the caller when needed, so this
//! layer stays free of event-shape assumptions.

use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;

/// Compiled CEL expression. Re-exported from `cel-interpreter` so
/// the rest of the scene crate only needs to import from this
/// module.
pub type Program = cel_interpreter::Program;

/// Evaluation context alias. `'a` lets callers nest `Child`
/// contexts (see `cel_interpreter::Context::new_inner_scope`).
pub type Context<'a> = cel_interpreter::Context<'a>;

/// Resolved CEL value, re-exported for symmetry with `Program` and
/// `Context` so callers can spell their types as `cel::Value`.
pub type Value = cel_interpreter::Value;

/// Compile a CEL expression source into a reusable [`Program`].
///
/// On parser failure, returns [`SceneError::CelParse`] carrying the
/// rendered parser message and a byte-range span covering the full
/// expression. Callers that want a more precise span (e.g. picking
/// out a `when=` attribute inside a scene file) should wrap the
/// error and add a `#[related]` site via `miette::Diagnostic`.
///
/// # Example
/// ```
/// # use ark_scene::cel::compile;
/// let prog = compile("1 + 1", "<inline>", 0).unwrap();
/// assert!(format!("{prog:?}").contains("Program"));
/// ```
pub fn compile(expr: &str, src_name: &str, start_offset: usize) -> Result<Program, SceneError> {
    // `cel-interpreter` 0.10 wraps an ANTLR-generated parser that
    // can panic on certain malformed inputs (e.g. stray `@` sigils,
    // unmatched `(`). We catch the unwind and surface it as a
    // normal `cel/parse` diagnostic so scene compilation never
    // aborts the process.
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Program::compile(expr)));
    match result {
        Ok(Ok(prog)) => Ok(prog),
        Ok(Err(errs)) => Err(SceneError::CelParse {
            message: errs.to_string(),
            src: NamedSource::new(src_name, expr.to_string()),
            at: span_for(start_offset, expr.len()),
        }),
        Err(panic) => {
            let payload = panic
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "CEL parser panicked (upstream bug)".to_string());
            Err(SceneError::CelParse {
                message: format!("CEL parser panicked: {payload}"),
                src: NamedSource::new(src_name, expr.to_string()),
                at: span_for(start_offset, expr.len()),
            })
        }
    }
}

/// Build a `SourceSpan` that survives a zero-length expression
/// (miette's renderer treats `(offset, 0)` as a caret, not a
/// range, which still produces a sensible diagnostic).
fn span_for(offset: usize, len: usize) -> SourceSpan {
    if len == 0 {
        (offset, 0).into()
    } else {
        (offset, len).into()
    }
}

/// Evaluate a compiled [`Program`] against `ctx` and return the
/// resulting [`Value`].
///
/// On failure, returns [`SceneError::CelEvaluate`] with the
/// `ExecutionError`'s rendered message. Callers are expected to
/// render that through miette directly — the variant has no span
/// because it's associated with a runtime event, not a source
/// location.
pub fn eval(program: &Program, ctx: &Context<'_>) -> Result<Value, SceneError> {
    program
        .execute(ctx)
        .map_err(|err| SceneError::CelEvaluate {
            message: err.to_string(),
        })
}

/// Evaluate a predicate (`when=` / `if=`) and enforce that the
/// result is a `bool`. Non-bool results surface as
/// [`SceneError::CelEvaluate`] so the caller sees a uniform error
/// surface regardless of whether the expression was type-correct.
///
/// # Example
/// ```
/// # use ark_scene::cel::{compile, eval_bool};
/// # use cel_interpreter::Context;
/// let prog = compile("1 < 2", "<inline>", 0).unwrap();
/// assert_eq!(eval_bool(&prog, &Context::default()).unwrap(), true);
/// ```
pub fn eval_bool(program: &Program, ctx: &Context<'_>) -> Result<bool, SceneError> {
    match eval(program, ctx)? {
        Value::Bool(b) => Ok(b),
        other => Err(SceneError::CelEvaluate {
            message: format!(
                "expected boolean result, got {other:?} — `when=` / `if=` predicates must evaluate to a bool"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    /// Baseline: the simplest compilable expression.
    #[test]
    fn compile_trivial() {
        let _p = compile("1 + 1", "expr", 0).expect("should compile");
    }

    /// A syntactically invalid expression produces a `cel/parse` diag.
    ///
    /// Note: `cel-interpreter`'s parser can panic via the underlying
    /// ANTLR runtime on certain malformed inputs (e.g. trailing
    /// operators). We pick a shape that returns an error cleanly —
    /// the upstream panic is tracked on the CEL crate's issue tracker
    /// and is outside this wrapper's scope.
    #[test]
    fn compile_syntax_error_maps_to_cel_parse() {
        let err = compile("(", "expr", 0).expect_err("expected parse error");
        assert_eq!(err.code_enum(), ErrorCode::CelParse);
        let msg = format!("{err}");
        assert!(msg.contains("CEL expression failed to parse"), "msg: {msg}");
    }

    /// Evaluation of a well-formed expression returns a Value we can
    /// match on.
    #[test]
    fn eval_returns_expected_value() {
        let prog = compile("1 + 2", "expr", 0).unwrap();
        let ctx = Context::default();
        let v = eval(&prog, &ctx).unwrap();
        match v {
            Value::Int(n) => assert_eq!(n, 3),
            other => panic!("expected Int(3), got {other:?}"),
        }
    }

    /// Undeclared reference during eval maps to `cel/evaluate`.
    #[test]
    fn eval_undeclared_reference_maps_to_cel_evaluate() {
        let prog = compile("nope == 1", "expr", 0).unwrap();
        let ctx = Context::default();
        let err = eval(&prog, &ctx).expect_err("expected eval error");
        assert_eq!(err.code_enum(), ErrorCode::CelEvaluate);
    }

    /// `eval_bool` unwraps boolean results.
    #[test]
    fn eval_bool_true_branch() {
        let prog = compile("1 < 2", "expr", 0).unwrap();
        let ctx = Context::default();
        assert!(eval_bool(&prog, &ctx).unwrap());
    }

    /// `eval_bool` on a non-bool expression surfaces a `cel/evaluate`
    /// error rather than silently coercing.
    #[test]
    fn eval_bool_rejects_non_bool() {
        let prog = compile("1 + 1", "expr", 0).unwrap();
        let ctx = Context::default();
        let err = eval_bool(&prog, &ctx).expect_err("should reject int result");
        assert_eq!(err.code_enum(), ErrorCode::CelEvaluate);
        let rendered = format!("{err}");
        assert!(rendered.contains("boolean"), "msg: {rendered}");
    }
}
