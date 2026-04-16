//! `{Rhai}` brace-hole interpolation for scene string values (T-022 / R8).
//!
//! Every string in a scene node (`cwd="…"`, `name="…"`, op string args)
//! admits Rhai holes delimited by `{` … `}`. [`parse_interp`] tokenizes
//! a raw value into a sequence of [`InterpSegment`]s; [`render_interp`]
//! (+ the typed sibling [`render_interp_typed`]) evaluates the holes
//! against a live `rhai::Scope` and stitches the result back into a
//! string (or preserves the typed value when the whole string is one
//! hole).
//!
//! # Grammar
//!
//! - `{`  opens a hole; scan to the next `}`.
//! - `{{` escapes to a literal `{`.
//! - `}}` escapes to a literal `}`.
//! - `}` outside a hole (and not doubled) is an error.
//! - Empty hole `{}` is an error.
//!
//! Hole bodies are Rhai expressions; they are compiled via
//! [`compile_in_scope`] at parse time so [`InterpSegment::Hole`]
//! carries a ready-to-eval [`Program`].
//!
//! # Zero / single / multi hole semantics
//!
//! - **Zero holes** → single [`InterpSegment::Literal`]; rendering is a
//!   verbatim copy (no Rhai engine invocation at render time).
//! - **Single-hole whole-value** (the raw string is exactly one
//!   `{expr}` with no surrounding literal) → single [`InterpSegment::Hole`];
//!   [`render_interp_typed`] returns the raw `Dynamic` so callers that
//!   want to preserve `i64` / `bool` / `f64` for typed attrs can bypass
//!   the stringify step.
//! - **Mixed / multi-hole** → sequence of literals + holes; each hole
//!   is coerced to a string via Rhai's `to_string` and concatenated.

use crate::error::SceneError;
use crate::rhai::{Engine, Program, RhaiScope, compile_in_scope};
use miette::{NamedSource, SourceSpan};

/// One segment of a parsed interpolation string.
#[derive(Debug, Clone)]
pub enum InterpSegment {
    /// A verbatim literal chunk (escaped `{{` / `}}` already resolved).
    Literal(String),
    /// A compiled Rhai expression that produces the segment's value
    /// at render time.
    Hole(Program),
}

/// Tokenize `raw` into a sequence of [`InterpSegment`]s.
///
/// Each hole is compiled against the given [`RhaiScope`] via
/// [`compile_in_scope`], so parse errors surface early at scene-compile
/// time rather than at first evaluation.
#[allow(clippy::result_large_err)]
pub fn parse_interp(
    engine: &Engine,
    raw: &str,
    scope: RhaiScope,
) -> Result<Vec<InterpSegment>, SceneError> {
    let mut out: Vec<InterpSegment> = Vec::new();
    let mut literal = String::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'{' {
            // Escaped `{{` -> literal `{`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                literal.push('{');
                i += 2;
                continue;
            }
            // Flush pending literal.
            if !literal.is_empty() {
                out.push(InterpSegment::Literal(std::mem::take(&mut literal)));
            }
            // Scan until matching `}` (single-level; Rhai expressions
            // don't carry unbalanced braces in normal use).
            let hole_start = i + 1;
            let mut j = hole_start;
            let mut closed = false;
            while j < bytes.len() {
                if bytes[j] == b'}' {
                    closed = true;
                    break;
                }
                j += 1;
            }
            if !closed {
                return Err(SceneError::RhaiParse {
                    message: "unterminated `{Rhai}` interpolation hole (missing `}`)".into(),
                    src: NamedSource::new("<interp>", raw.to_string()),
                    span: SourceSpan::new(i.into(), bytes.len() - i),
                });
            }
            let body = &raw[hole_start..j];
            if body.is_empty() {
                return Err(SceneError::RhaiParse {
                    message: "empty Rhai hole `{}`".into(),
                    src: NamedSource::new("<interp>", raw.to_string()),
                    span: SourceSpan::new(i.into(), (j - i + 1).max(2)),
                });
            }
            let program = compile_in_scope(engine, body, scope)?;
            out.push(InterpSegment::Hole(program));
            i = j + 1;
        } else if c == b'}' {
            // Escaped `}}` -> literal `}`.
            if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                literal.push('}');
                i += 2;
                continue;
            }
            return Err(SceneError::RhaiParse {
                message: "unexpected `}` outside an interpolation hole".into(),
                src: NamedSource::new("<interp>", raw.to_string()),
                span: SourceSpan::new(i.into(), 1),
            });
        } else {
            // Push the full UTF-8 codepoint, not just one byte.
            let ch_start = i;
            // Determine UTF-8 char length (1..=4).
            let ch_len = utf8_char_len(c);
            let end = (ch_start + ch_len).min(bytes.len());
            // Safety: `raw` is valid UTF-8 and we advance by full
            // codepoint widths. Use the slice directly.
            literal.push_str(&raw[ch_start..end]);
            i = end;
        }
    }
    if !literal.is_empty() {
        out.push(InterpSegment::Literal(literal));
    }
    // Zero-segment input (empty string) -> single empty literal.
    if out.is_empty() {
        out.push(InterpSegment::Literal(String::new()));
    }
    Ok(out)
}

/// Width (bytes) of the UTF-8 codepoint that starts with `first`.
/// Returns 1 for ASCII and invalid leading bytes (the outer loop will
/// clip at `bytes.len()` anyway).
fn utf8_char_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xC0 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

/// Render a segment list into a `String`.
///
/// Literal segments pass through verbatim; hole segments evaluate
/// against `live_scope` and are coerced to a string via Rhai's
/// `Dynamic::to_string`.
#[allow(clippy::result_large_err)]
pub fn render_interp(
    segments: &[InterpSegment],
    engine: &Engine,
    live_scope: &mut rhai::Scope,
) -> Result<String, SceneError> {
    let mut out = String::new();
    for seg in segments {
        match seg {
            InterpSegment::Literal(s) => out.push_str(s),
            InterpSegment::Hole(prog) => {
                let v = crate::rhai::eval_value(engine, prog, live_scope)?;
                out.push_str(&v.to_string());
            }
        }
    }
    Ok(out)
}

/// Render a segment list, preserving the native Rhai type when the
/// whole input was a single hole (`"{expr}"`).
///
/// Single-hole whole-value passthrough is the rule per R8: typed op
/// attrs (e.g. `ttl_ms="{payload.n}"` on `set_status`) should keep
/// `i64` / `bool` / `f64` rather than being coerced to a string. For
/// every other shape (literal-only, mixed literal+hole, multi-hole),
/// the result is a stringified [`Dynamic::from`].
#[allow(clippy::result_large_err)]
pub fn render_interp_typed(
    segments: &[InterpSegment],
    engine: &Engine,
    live_scope: &mut rhai::Scope,
) -> Result<rhai::Dynamic, SceneError> {
    match segments {
        [InterpSegment::Hole(prog)] => crate::rhai::eval_value(engine, prog, live_scope),
        _ => {
            let s = render_interp(segments, engine, live_scope)?;
            Ok(rhai::Dynamic::from(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        Engine::new()
    }

    #[test]
    fn zero_holes_single_literal() {
        let segs = parse_interp(&engine(), "plain text", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            InterpSegment::Literal(s) => assert_eq!(s, "plain text"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn single_hole_whole_value() {
        let segs = parse_interp(&engine(), "{id}", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0], InterpSegment::Hole(_)));
    }

    #[test]
    fn multi_hole_mixed() {
        let segs = parse_interp(&engine(), "before {id} middle {name} after", RhaiScope::Spawn)
            .unwrap();
        // Expect: Literal, Hole, Literal, Hole, Literal
        assert_eq!(segs.len(), 5);
        assert!(matches!(segs[0], InterpSegment::Literal(ref s) if s == "before "));
        assert!(matches!(segs[1], InterpSegment::Hole(_)));
        assert!(matches!(segs[2], InterpSegment::Literal(ref s) if s == " middle "));
        assert!(matches!(segs[3], InterpSegment::Hole(_)));
        assert!(matches!(segs[4], InterpSegment::Literal(ref s) if s == " after"));
    }

    #[test]
    fn escaped_open_brace() {
        let segs = parse_interp(&engine(), "a {{ b", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            InterpSegment::Literal(s) => assert_eq!(s, "a { b"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn escaped_close_brace() {
        let segs = parse_interp(&engine(), "a }} b", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            InterpSegment::Literal(s) => assert_eq!(s, "a } b"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn escape_with_interpolation() {
        let segs = parse_interp(&engine(), "{{{id}}}", RhaiScope::Spawn).unwrap();
        // `{{` -> `{`, `{id}` -> Hole, `}}` -> `}`
        assert_eq!(segs.len(), 3);
        assert!(matches!(segs[0], InterpSegment::Literal(ref s) if s == "{"));
        assert!(matches!(segs[1], InterpSegment::Hole(_)));
        assert!(matches!(segs[2], InterpSegment::Literal(ref s) if s == "}"));
    }

    #[test]
    fn empty_hole_errors() {
        let err = parse_interp(&engine(), "a {} b", RhaiScope::Spawn)
            .expect_err("empty hole must error");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn unbalanced_close_brace_errors() {
        let err = parse_interp(&engine(), "a } b", RhaiScope::Spawn)
            .expect_err("stray `}` must error");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn unterminated_hole_errors() {
        let err = parse_interp(&engine(), "a {id b", RhaiScope::Spawn)
            .expect_err("unterminated hole must error");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn empty_string_is_empty_literal() {
        let segs = parse_interp(&engine(), "", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            InterpSegment::Literal(s) => assert!(s.is_empty()),
            _ => panic!("expected empty literal"),
        }
    }

    #[test]
    fn render_literal_only_passes_through() {
        let engine = engine();
        let segs = parse_interp(&engine, "hello world", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        let s = render_interp(&segs, &engine, &mut scope).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn render_single_hole_to_string() {
        let engine = engine();
        let segs = parse_interp(&engine, "{id}", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        scope.push("id", "abc123".to_string());
        let s = render_interp(&segs, &engine, &mut scope).unwrap();
        assert_eq!(s, "abc123");
    }

    #[test]
    fn render_mixed_holes_concat() {
        let engine = engine();
        let segs = parse_interp(&engine, "pre {id} mid {name} post", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        scope.push("id", "42".to_string());
        scope.push("name", "work".to_string());
        let s = render_interp(&segs, &engine, &mut scope).unwrap();
        assert_eq!(s, "pre 42 mid work post");
    }

    #[test]
    fn render_typed_preserves_int_for_single_hole() {
        let engine = engine();
        let segs = parse_interp(&engine, "{n}", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        scope.push("n", 42_i64);
        let v = render_interp_typed(&segs, &engine, &mut scope).unwrap();
        assert_eq!(v.as_int().unwrap(), 42);
    }

    #[test]
    fn render_typed_preserves_bool_for_single_hole() {
        let engine = engine();
        let segs = parse_interp(&engine, "{b}", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        scope.push("b", true);
        let v = render_interp_typed(&segs, &engine, &mut scope).unwrap();
        assert!(v.as_bool().unwrap());
    }

    #[test]
    fn render_typed_stringifies_mixed() {
        let engine = engine();
        let segs = parse_interp(&engine, "x={n}", RhaiScope::Spawn).unwrap();
        let mut scope = rhai::Scope::new();
        scope.push("n", 42_i64);
        let v = render_interp_typed(&segs, &engine, &mut scope).unwrap();
        assert_eq!(v.into_string().unwrap(), "x=42");
    }

    #[test]
    fn parse_compiles_each_hole() {
        // Programs are compiled at parse time; broken Rhai surfaces as parse error.
        let err = parse_interp(&engine(), "broken {1 +} end", RhaiScope::Spawn)
            .expect_err("broken Rhai must reject at parse");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn utf8_literals_pass_through() {
        let segs = parse_interp(&engine(), "héllo ✓ wörld", RhaiScope::Spawn).unwrap();
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            InterpSegment::Literal(s) => assert_eq!(s, "héllo ✓ wörld"),
            _ => panic!("expected literal"),
        }
    }
}
