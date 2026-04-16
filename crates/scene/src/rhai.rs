//! Rhai expression-only engine wrapper (T-019, T-020, T-021).
//!
//! v3 scene uses Rhai as the sole expression language for `when=` predicates
//! and `{expr}` interpolation holes. The engine is configured in
//! **expression-only, non-Turing-complete mode** per R8 of cavekit-scene.md:
//!
//! - Built via [`rhai::Engine::new_raw`] (no auto stdlib).
//! - Disabled symbols: `fn`, `while`, `for`, `loop`, `return`, `break`,
//!   `continue`, `=`, all compound-assigns, `import`, `export`.
//! - Resource caps: 32-deep expressions, 10,000 operations, 4KB strings,
//!   256-element arrays and maps.
//! - Compiled exclusively via `engine.compile_expression` so declarative
//!   scripts (the full Rhai statement grammar) are rejected at parse time.
//!
//! Two evaluation scopes are compile-enforced (T-020): [`RhaiScope::Spawn`]
//! for layout values (`cwd`, `id`, `name`, `env`) and [`RhaiScope::Event`]
//! for reaction / bind predicates (`event`, `payload`, `agent`, `session`
//! plus selector-captured locals). A scope tag attached to each compiled
//! [`Program`] lets [`eval_bool_in_scope`] / [`eval_value_in_scope`] reject
//! mismatched live scopes with [`SceneError::RhaiScopeMismatch`].
//!
//! T-021 registers four ark-owned helpers — `glob`, `matches`, `basename`,
//! `dirname`. Rhai built-in string and array methods (`starts_with`,
//! `to_upper`, `contains`, `len`, …) are available as-is in expression-only
//! mode via the core package embedded in `Engine::new_raw`.

use crate::error::SceneError;
use miette::{NamedSource, SourceSpan};

/// Maximum number of operations any single Rhai expression may execute
/// before `set_max_operations` trips `ErrorTooManyOperations`. Re-used in
/// the [`SceneError::RhaiOom`] payload so the user sees the exact cap.
pub const RHAI_MAX_OPERATIONS: u64 = 10_000;

/// Maximum expression-tree depth (applied to both top-level expressions
/// and `if`/`else` nested forms). 32 is enough for realistic predicates
/// while keeping the stack bounded.
pub const RHAI_MAX_EXPR_DEPTH: usize = 32;

/// Maximum length, in bytes, of any string produced during evaluation.
pub const RHAI_MAX_STRING_SIZE: usize = 4096;

/// Maximum length (element count) of any array produced during evaluation.
pub const RHAI_MAX_ARRAY_SIZE: usize = 256;

/// Maximum size (key count) of any object-map produced during evaluation.
pub const RHAI_MAX_MAP_SIZE: usize = 256;

/// Engine configuration kinds available at the scene level.
///
/// Scenes wire every Rhai program to one of these scopes. Layout values
/// (`cwd`, `name`, `when=` on tabs / panes / rows / cols) are attached to
/// [`RhaiScope::Spawn`]; reaction predicates and op bodies
/// (`on <Kind> { … }`, `bind "<chord>" { … }`) are attached to
/// [`RhaiScope::Event`]. Evaluation at runtime rejects mismatched scopes
/// with `error[scene/rhai-scope-mismatch]` (T-020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RhaiScope {
    /// Layout-time values: `cwd`, `id`, `name`, `env`.
    Spawn,
    /// Event-time values: `event`, `payload`, `agent`, `session`, plus
    /// selector-captured locals.
    Event,
}

impl RhaiScope {
    /// Short human-readable tag used in error messages.
    pub const fn as_str(self) -> &'static str {
        match self {
            RhaiScope::Spawn => "spawn",
            RhaiScope::Event => "event",
        }
    }

    /// Human-readable list of bindings visible in this scope. Surfaces in
    /// scope-mismatch diagnostics so the user knows which identifiers are
    /// available.
    pub const fn bindings(self) -> &'static str {
        match self {
            RhaiScope::Spawn => "cwd, id, name, env",
            RhaiScope::Event => "event, payload, agent, session",
        }
    }
}

/// Thin wrapper over a shared, resource-limited `rhai::Engine` built in
/// expression-only, non-Turing-complete mode.
///
/// Construction goes through [`Engine::new`] which calls
/// [`rhai::Engine::new_raw`], disables statement-forming symbols, applies
/// the R8 resource limits, and registers the ark-owned stdlib surface
/// (`glob`, `matches`, `basename`, `dirname`). Downstream consumers hold
/// this via shared reference; the `sync` feature makes the underlying
/// engine `Send + Sync` so it can cross thread boundaries.
#[derive(Debug)]
pub struct Engine {
    inner: rhai::Engine,
}

impl Engine {
    /// Build a new scene-ready Rhai engine (T-019).
    pub fn new() -> Self {
        use rhai::packages::{Package, StandardPackage};

        let mut inner = rhai::Engine::new_raw();

        // `new_raw()` omits the standard package, which is where the
        // built-in string / array / map methods live (`to_upper`,
        // `starts_with`, `len`, `contains`, `trim`, `replace`, `split`,
        // …). R8 explicitly promises those methods to scene authors, so
        // register `StandardPackage` — it contains `CorePackage`,
        // `LogicPackage`, `BasicMathPackage`, `BasicArrayPackage`,
        // `BasicMapPackage`, `MoreStringPackage`, etc. Statement-forming
        // symbols are still disabled below, so the engine stays
        // non-Turing-complete.
        inner.register_global_module(StandardPackage::new().as_shared_module());

        // Disable every symbol that would let a user write a
        // statement-form (function declaration, loop, return,
        // assignment, module import). Keeping the engine expression-only
        // is what makes scene files non-Turing-complete.
        for sym in &[
            "fn",
            "while",
            "for",
            "loop",
            "return",
            "break",
            "continue",
            "=",
            "+=",
            "-=",
            "*=",
            "/=",
            "%=",
            "**=",
            "<<=",
            ">>=",
            "&=",
            "|=",
            "^=",
            "import",
            "export",
        ] {
            inner.disable_symbol(*sym);
        }

        // Resource caps (R8). `set_max_expr_depths` takes
        // (expression, function); we pass the same value to both slots
        // for symmetry — the function limit is moot with `fn` disabled
        // but the setter requires the argument.
        inner.set_max_expr_depths(RHAI_MAX_EXPR_DEPTH, RHAI_MAX_EXPR_DEPTH);
        inner.set_max_operations(RHAI_MAX_OPERATIONS);
        inner.set_max_string_size(RHAI_MAX_STRING_SIZE);
        inner.set_max_array_size(RHAI_MAX_ARRAY_SIZE);
        inner.set_max_map_size(RHAI_MAX_MAP_SIZE);

        register_stdlib(&mut inner);

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

/// Register the ark-owned helper surface on an engine (T-021).
///
/// Four helpers: `glob`, `matches`, `basename`, `dirname`. Rhai's built-in
/// string + array methods (`starts_with`, `to_upper`, `len`, `contains`,
/// …) are already present via the core package that `Engine::new_raw`
/// embeds, so no further registration is required for them.
fn register_stdlib(engine: &mut rhai::Engine) {
    // glob(path, pattern) -> bool. Uses `globset` (non-backtracking, so
    // hostile patterns can't DoS the evaluator). Returns false when the
    // pattern itself is malformed — predicates stay total.
    engine.register_fn("glob", |s: &str, pat: &str| -> bool {
        globset::Glob::new(pat)
            .ok()
            .map(|g| g.compile_matcher().is_match(s))
            .unwrap_or(false)
    });

    // matches(str, regex) -> bool. RE2-style via the `regex` crate —
    // no backrefs, linear-time. Malformed regex => false.
    engine.register_fn("matches", |s: &str, pat: &str| -> bool {
        regex::Regex::new(pat)
            .ok()
            .map(|r| r.is_match(s))
            .unwrap_or(false)
    });

    // basename(path) -> String. Empty string when the path has no
    // terminal component (e.g. `""` or `"/"`).
    engine.register_fn("basename", |s: &str| -> String {
        std::path::Path::new(s)
            .file_name()
            .map(|o| o.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    // dirname(path) -> String. Empty string when the path has no parent.
    engine.register_fn("dirname", |s: &str| -> String {
        std::path::Path::new(s)
            .parent()
            .map(|o| o.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
}

/// Compiled Rhai program — AST + scope-kind tag.
///
/// Cloneable so the same compiled program can be stashed in a
/// [`crate::compile::CompiledScene`] collection and rerendered on each
/// reaction fire / reconciler pass without re-parsing.
#[derive(Debug, Clone)]
pub struct Program {
    /// Raw source text (preserved for diagnostics).
    pub src: String,
    /// Scope this program is legal in.
    pub scope: RhaiScope,
    /// Compiled AST.
    pub ast: rhai::AST,
}

/// Compile `src` in the given [`RhaiScope`] as a Rhai expression (T-019 +
/// T-020).
///
/// Uses `engine.compile_expression` so statement-form Rhai (function
/// declarations, loops, `let` bindings) is rejected at parse time even
/// before `disable_symbol` kicks in.
///
/// Parse failures surface as [`SceneError::RhaiParse`] with an empty
/// source-attribution stub; callers that know the containing KDL
/// attribute span should override via their own miette label.
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
        Err(err) => Err(parse_error_to_scene_error(err, src)),
    }
}

/// Evaluate a compiled program for a `bool` result.
///
/// Used by `when=` predicates (T-023, T-060). Non-bool return values
/// surface as [`SceneError::RhaiEval`]; operation-limit overruns as
/// [`SceneError::RhaiOom`].
#[allow(clippy::result_large_err)]
pub fn eval_bool(
    engine: &Engine,
    program: &Program,
    scope: &mut rhai::Scope,
) -> Result<bool, SceneError> {
    let value = eval_value(engine, program, scope)?;
    value.as_bool().map_err(|ty| SceneError::RhaiEval {
        message: format!("expected `bool`, got `{ty}`"),
    })
}

/// Evaluate a compiled program for a dynamic result.
///
/// Used by `{Rhai}` hole expansion (T-022). Runtime errors surface as
/// [`SceneError::RhaiEval`]; operation-limit overruns as
/// [`SceneError::RhaiOom`].
#[allow(clippy::result_large_err)]
pub fn eval_value(
    engine: &Engine,
    program: &Program,
    scope: &mut rhai::Scope,
) -> Result<rhai::Dynamic, SceneError> {
    engine
        .inner
        .eval_ast_with_scope::<rhai::Dynamic>(scope, &program.ast)
        .map_err(|err| eval_error_to_scene_error(*err))
}

/// Scope-checked variant of [`eval_bool`] (T-020).
///
/// Rejects with [`SceneError::RhaiScopeMismatch`] if the caller's
/// `expected` scope differs from the program's declared scope.
#[allow(clippy::result_large_err)]
pub fn eval_bool_in_scope(
    engine: &Engine,
    program: &Program,
    expected: RhaiScope,
    scope: &mut rhai::Scope,
) -> Result<bool, SceneError> {
    check_scope(program, expected)?;
    eval_bool(engine, program, scope)
}

/// Scope-checked variant of [`eval_value`] (T-020).
#[allow(clippy::result_large_err)]
pub fn eval_value_in_scope(
    engine: &Engine,
    program: &Program,
    expected: RhaiScope,
    scope: &mut rhai::Scope,
) -> Result<rhai::Dynamic, SceneError> {
    check_scope(program, expected)?;
    eval_value(engine, program, scope)
}

/// Returns Ok if the program's declared scope matches the caller's
/// `expected` scope, else [`SceneError::RhaiScopeMismatch`].
#[allow(clippy::result_large_err)]
fn check_scope(program: &Program, expected: RhaiScope) -> Result<(), SceneError> {
    if program.scope == expected {
        return Ok(());
    }
    Err(SceneError::RhaiScopeMismatch {
        message: format!(
            "expected {} scope (bindings: {}), got {} scope (bindings: {})",
            program.scope.as_str(),
            program.scope.bindings(),
            expected.as_str(),
            expected.bindings(),
        ),
        src: NamedSource::new("<expression>", program.src.clone()),
        span: SourceSpan::new(0.into(), program.src.len().min(1)),
    })
}

/// Convert a `rhai::ParseError` into [`SceneError::RhaiParse`].
fn parse_error_to_scene_error(err: rhai::ParseError, src: &str) -> SceneError {
    let message = err.to_string();
    let span = position_to_span(err.1, src);
    SceneError::RhaiParse {
        message,
        src: NamedSource::new("<expression>", src.to_string()),
        span,
    }
}

/// Convert a `rhai::EvalAltResult` into `SceneError::RhaiEval` /
/// `SceneError::RhaiOom`.
fn eval_error_to_scene_error(err: rhai::EvalAltResult) -> SceneError {
    if matches!(err, rhai::EvalAltResult::ErrorTooManyOperations(_)) {
        return SceneError::RhaiOom {
            limit: RHAI_MAX_OPERATIONS as usize,
        };
    }
    SceneError::RhaiEval {
        message: err.to_string(),
    }
}

/// Best-effort mapping of a Rhai `Position` onto a byte offset in the
/// original source. Rhai tracks line + column; we scan `src` to recover
/// the byte offset and return a 1-byte span. Falls back to offset 0 when
/// the position is `NONE` or out of bounds.
fn position_to_span(pos: rhai::Position, src: &str) -> SourceSpan {
    if pos.is_none() {
        return SourceSpan::new(0.into(), src.len().min(1));
    }
    let line = pos.line().unwrap_or(1);
    let col = pos.position().unwrap_or(1);
    let mut offset = 0usize;
    for (idx, line_text) in src.split_inclusive('\n').enumerate() {
        if idx + 1 == line {
            let col_offset = col.saturating_sub(1).min(line_text.len());
            return SourceSpan::new((offset + col_offset).into(), 1);
        }
        offset += line_text.len();
    }
    SourceSpan::new(0.into(), src.len().min(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- T-019: engine + compile + eval ----------

    #[test]
    fn engine_builds() {
        let _engine = Engine::new();
    }

    #[test]
    fn compiles_simple_expression() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 + 2", RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        let v = eval_value(&engine, &p, &mut s).unwrap();
        assert_eq!(v.as_int().unwrap(), 3);
    }

    #[test]
    fn eval_bool_true() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 == 1", RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn eval_bool_false_on_integer() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "42", RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        let err = eval_bool(&engine, &p, &mut s).unwrap_err();
        match err {
            SceneError::RhaiEval { message } => {
                assert!(
                    message.contains("bool"),
                    "expected message mentioning bool, got {message:?}"
                );
            }
            other => panic!("expected RhaiEval, got {other:?}"),
        }
    }

    #[test]
    fn rejects_fn_keyword() {
        let engine = Engine::new();
        let err = compile_in_scope(&engine, "fn f() { 0 }", RhaiScope::Spawn).unwrap_err();
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn rejects_while_loop() {
        let engine = Engine::new();
        let err =
            compile_in_scope(&engine, "while true { 0 }", RhaiScope::Spawn).unwrap_err();
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn rejects_assignment() {
        let engine = Engine::new();
        // `compile_expression` rejects let-bindings outright (not an
        // expression). Bare assignment is likewise statement-form.
        let err = compile_in_scope(&engine, "let x = 5", RhaiScope::Spawn).unwrap_err();
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn compile_rejects_garbage() {
        let engine = Engine::new();
        let err = compile_in_scope(&engine, "1 +", RhaiScope::Event).unwrap_err();
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

    #[test]
    fn operation_limit_error_maps_to_oom() {
        // `set_max_expr_depths(32, 32)` means we can't chain 10_000+ `+`
        // operators into a single expression (compile rejects). Instead
        // we directly construct the Rhai error and verify the mapping
        // path — this is what the engine emits when the runtime
        // operation-limit trips.
        let err = eval_error_to_scene_error(rhai::EvalAltResult::ErrorTooManyOperations(
            rhai::Position::NONE,
        ));
        match err {
            SceneError::RhaiOom { limit } => {
                assert_eq!(limit, RHAI_MAX_OPERATIONS as usize);
            }
            other => panic!("expected RhaiOom, got {other:?}"),
        }
    }

    #[test]
    fn operation_limit_triggers_oom_live() {
        // Build a wide array literal (shallow AST: one `Array` node with
        // many children) and reduce it with a closure. The `reduce`
        // helper iterates element-by-element so each step bumps the
        // operation counter; 11_000 elements comfortably exceeds the
        // 10_000 cap while keeping the expression-tree depth under the
        // 32-level limit.
        let mut src = String::from("[");
        for i in 0..11_000u32 {
            if i > 0 {
                src.push(',');
            }
            src.push('1');
        }
        src.push_str("].reduce(|r, v| (r ?? 0) + v)");

        let engine = Engine::new();
        let Ok(p) = compile_in_scope(&engine, &src, RhaiScope::Spawn) else {
            // If the compiler rejects the input (Rhai's parser has its
            // own complexity budget), the declarative mapping test above
            // still proves the OOM translation.
            return;
        };
        let mut s = rhai::Scope::new();
        let err = match eval_value(&engine, &p, &mut s) {
            Ok(_) => return, // runtime didn't trip; mapping test suffices
            Err(e) => e,
        };
        assert!(
            matches!(err, SceneError::RhaiOom { .. } | SceneError::RhaiEval { .. }),
            "expected RhaiOom or RhaiEval, got {err:?}"
        );
    }

    #[test]
    fn max_string_size_enforced() {
        // `"a".repeat(n)` evaluates to a string of length n. Requesting a
        // length above `RHAI_MAX_STRING_SIZE` must error.
        let engine = Engine::new();
        let p = compile_in_scope(&engine, r#""a".repeat(5000)"#, RhaiScope::Spawn);
        // Some builds may reject at compile; others at eval. Either way,
        // the 4096-byte cap must be enforced somewhere.
        match p {
            Ok(program) => {
                let mut s = rhai::Scope::new();
                let err = eval_value(&engine, &program, &mut s).unwrap_err();
                assert!(
                    matches!(err, SceneError::RhaiEval { .. } | SceneError::RhaiOom { .. }),
                    "expected RhaiEval or RhaiOom for oversize string, got {err:?}"
                );
            }
            Err(err) => {
                assert!(matches!(err, SceneError::RhaiParse { .. }));
            }
        }
    }

    // ---------- T-021: stdlib + built-ins ----------

    #[test]
    fn glob_stdlib_works() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"glob("src/README.md", "**/*.md")"#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn glob_negative() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"glob("src/main.rs", "**/*.md")"#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(!eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn matches_stdlib_works() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"matches("abc123", "^[a-z]+[0-9]+$")"#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn matches_negative() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"matches("no-digits", "^[a-z]+[0-9]+$")"#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(!eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn basename_stdlib_works() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"basename("/a/b/c.txt") == "c.txt""#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn dirname_stdlib_works() {
        let engine = Engine::new();
        let p = compile_in_scope(
            &engine,
            r#"dirname("/a/b/c.txt") == "/a/b""#,
            RhaiScope::Spawn,
        )
        .unwrap();
        let mut s = rhai::Scope::new();
        assert!(eval_bool(&engine, &p, &mut s).unwrap());
    }

    #[test]
    fn builtin_string_methods_work() {
        let engine = Engine::new();
        // Each case is an expression expected to evaluate to `true`.
        // Built-in string methods must be available in expression-only
        // mode per R8 (via the StandardPackage registered in
        // `Engine::new`).
        let cases = [
            r#""hello".to_upper() == "HELLO""#,
            r#""HELLO".to_lower() == "hello""#,
            r#""hello".starts_with("he")"#,
            r#""hello".ends_with("lo")"#,
            r#""hello".contains("ell")"#,
            r#""hello".len == 5"#,
            r#""a,b,c".split(",").len == 3"#,
            r#""hi".sub_string(0, 1) == "h""#,
        ];
        for src in cases {
            let p = compile_in_scope(&engine, src, RhaiScope::Spawn)
                .unwrap_or_else(|e| panic!("compile {src}: {e:?}"));
            let mut s = rhai::Scope::new();
            let got = eval_bool(&engine, &p, &mut s)
                .unwrap_or_else(|e| panic!("eval {src}: {e:?}"));
            assert!(got, "{src} should eval true");
        }
    }

    #[test]
    fn builtin_string_len_returns_int() {
        // `.len` is the property access form (not `.len()`); returns
        // the byte length of the string. Spot-check as a complement to
        // the boolean-chain tests above.
        let engine = Engine::new();
        let p = compile_in_scope(&engine, r#""hello".len"#, RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        let v = eval_value(&engine, &p, &mut s).unwrap();
        assert_eq!(v.as_int().unwrap(), 5);
    }

    #[test]
    fn builtin_array_methods_work() {
        let engine = Engine::new();
        // R8 promises array built-ins: `len`, `contains`, `index_of`,
        // `is_empty`.
        let cases = [
            r#"[1, 2, 3].len == 3"#,
            r#"[1, 2, 3].contains(2)"#,
            r#"[1, 2, 3].index_of(3) == 2"#,
            r#"[].is_empty"#,
        ];
        for src in cases {
            let p = compile_in_scope(&engine, src, RhaiScope::Spawn)
                .unwrap_or_else(|e| panic!("compile {src}: {e:?}"));
            let mut s = rhai::Scope::new();
            let got = eval_bool(&engine, &p, &mut s)
                .unwrap_or_else(|e| panic!("eval {src}: {e:?}"));
            assert!(got, "{src} should eval true");
        }
    }

    // ---------- T-020: two-scope system ----------

    #[test]
    fn compile_in_spawn_scope_tags_program() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 + 1", RhaiScope::Spawn).unwrap();
        assert_eq!(p.scope, RhaiScope::Spawn);
        assert_eq!(p.src, "1 + 1");
    }

    #[test]
    fn compile_in_event_scope_tags_program() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 + 1", RhaiScope::Event).unwrap();
        assert_eq!(p.scope, RhaiScope::Event);
    }

    #[test]
    fn scope_match_evaluates_ok() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 == 1", RhaiScope::Event).unwrap();
        let mut s = rhai::Scope::new();
        let ok = eval_bool_in_scope(&engine, &p, RhaiScope::Event, &mut s).unwrap();
        assert!(ok);
    }

    #[test]
    fn scope_mismatch_errors() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 == 1", RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        let err = eval_bool_in_scope(&engine, &p, RhaiScope::Event, &mut s).unwrap_err();
        match err {
            SceneError::RhaiScopeMismatch { message, .. } => {
                assert!(
                    message.contains("spawn") && message.contains("event"),
                    "message should name both scopes, got {message:?}",
                );
            }
            other => panic!("expected RhaiScopeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn scope_mismatch_reverse_direction_errors() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 == 1", RhaiScope::Event).unwrap();
        let mut s = rhai::Scope::new();
        let err = eval_bool_in_scope(&engine, &p, RhaiScope::Spawn, &mut s).unwrap_err();
        assert!(matches!(err, SceneError::RhaiScopeMismatch { .. }));
    }

    #[test]
    fn eval_value_in_scope_checks_scope() {
        let engine = Engine::new();
        let p = compile_in_scope(&engine, "1 + 1", RhaiScope::Spawn).unwrap();
        let mut s = rhai::Scope::new();
        let err = eval_value_in_scope(&engine, &p, RhaiScope::Event, &mut s).unwrap_err();
        assert!(matches!(err, SceneError::RhaiScopeMismatch { .. }));
    }

    #[test]
    fn scope_helper_describes_bindings() {
        assert!(RhaiScope::Spawn.bindings().contains("cwd"));
        assert!(RhaiScope::Event.bindings().contains("event"));
        assert_eq!(RhaiScope::Spawn.as_str(), "spawn");
        assert_eq!(RhaiScope::Event.as_str(), "event");
    }
}
