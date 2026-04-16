//! Compile-pass validation of CEL predicates and templates in a
//! `SceneDoc`. Lives under T-2.6.
//!
//! `ark scene check` walks the parsed scene AST once and calls
//! [`validate_scene`] to surface every CEL/template error up-front
//! (instead of waiting for the reaction graph to fire at runtime).
//!
//! Two classes of checks:
//!
//! 1. **Compilability.** Every `when=` on a `tab` / `pane` and every
//!    `if=` on an `on { }` reaction is fed through
//!    [`crate::cel::compile`]. Parse errors map to
//!    [`SceneError::CelParse`] diagnostics.
//!
//! 2. **Static guards.** CEL is non-Turing-complete, so there's no
//!    need for a runtime budget, but adversarial predicates can
//!    still be pathologically slow if an attacker crafts very long
//!    inputs. Two static caps defend against that without any
//!    runtime cost:
//!
//!    - `max_expression_length = 4096` bytes (per expression).
//!    - `max_ast_depth = 64` (nested-paren + select chain depth,
//!      measured on the expression source as a cheap AST-depth
//!      proxy — cel-interpreter 0.10 doesn't expose its internal
//!      `Expression` struct, and approximating via parens captures
//!      the shape of adversarial deep recursions equally well).
//!
//! All errors are collected; the walk never short-circuits on the
//! first failure, so `ark scene check` can render multiple
//! diagnostics in one go.

use crate::ast::{KeybindNode, OnNode, PaneNode, SceneDoc, SceneNode, TabNode};
use crate::cel;
use crate::chord::{ChordError, validate_chord};
use crate::error::SceneError;
use crate::template;
use miette::NamedSource;

/// Upper bound on CEL expression length (bytes). Exceeding this
/// yields [`SceneError::CelExpressionTooLong`].
pub const MAX_EXPRESSION_LENGTH: usize = 4096;

/// Upper bound on CEL AST nesting depth (approximated by
/// parenthesis + dot-chain depth in the source). Exceeding yields
/// [`SceneError::CelAstTooDeep`].
pub const MAX_AST_DEPTH: usize = 64;

/// Validate every CEL predicate and template in a scene document.
///
/// Returns `Ok(())` when the entire walk succeeds. Otherwise the
/// `Err` carries every failure found during the single pass — the
/// caller (typically `ark scene check`) renders them all through
/// miette.
///
/// # Coverage
///
/// - `SceneNode.layout.tabs[*].when` → CEL predicate.
/// - `SceneNode.layout.tabs[*].panes[*].when` (recursive) → CEL predicate.
/// - `SceneNode.ons[*].if_` → CEL predicate.
/// - Templates: v1 has no typed template fields in the AST yet, but
///   [`validate_template`] exposes the check for call sites that
///   have already resolved a template string (e.g. future `emit=`
///   args). When the AST grows template fields (T-3.x), they will
///   plug into this function.
pub fn validate_scene(doc: &SceneDoc) -> Result<(), Vec<SceneError>> {
    let mut errors = Vec::new();
    walk_scene(&doc.scene, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate a single CEL expression in isolation.
///
/// Use this for call sites that don't have a full scene document
/// but still want to enforce the same guards (e.g. REPL-style
/// `ark scene eval`).
pub fn validate_cel(expr: &str, src_name: &str, start_offset: usize) -> Result<(), SceneError> {
    if expr.len() > MAX_EXPRESSION_LENGTH {
        return Err(SceneError::CelExpressionTooLong {
            len: expr.len(),
            max: MAX_EXPRESSION_LENGTH,
        });
    }
    let depth = approximate_ast_depth(expr);
    if depth > MAX_AST_DEPTH {
        return Err(SceneError::CelAstTooDeep {
            depth,
            max: MAX_AST_DEPTH,
        });
    }
    cel::compile(expr, src_name, start_offset).map(|_| ())
}

/// Validate a single template string. Currently a thin alias for
/// [`crate::template::compile_only`] — call sites requiring
/// variable-resolution checks use
/// [`crate::template::compile_time_render`] directly with a fully
/// populated `LayoutVars`.
pub fn validate_template(template_src: &str) -> Result<(), SceneError> {
    template::compile_only(template_src)
}

fn walk_scene(scene: &SceneNode, errors: &mut Vec<SceneError>) {
    if let Some(layout) = &scene.layout {
        for tab in &layout.tabs {
            walk_tab(tab, errors);
        }
        for pane in &layout.panes {
            walk_pane(pane, errors);
        }
    }
    for on in &scene.ons {
        walk_on(on, errors);
    }
    for kb in &scene.keybinds {
        walk_keybind(kb, errors);
    }
}

/// T-6.6: validate the keybind chord string against the loose grammar
/// `(Mod )*KEY`. Stricter validation surfaces at first session-spawn
/// via zellij's own lexer.
///
/// We synthesise a `NamedSource` from `<keybind>` here because the
/// per-node span isn't tracked on `KeybindNode` today (facet-kdl spans
/// are dropped at the typed-AST layer — see crate::ast module docs).
/// When `KeybindNode` grows a span field in a later tier, the
/// `NamedSource` and `SourceSpan` here will point into the original
/// scene file.
fn walk_keybind(kb: &KeybindNode, errors: &mut Vec<SceneError>) {
    if let Err(e) = validate_chord(&kb.chord) {
        errors.push(chord_error_to_scene_error(&kb.chord, e));
    }
}

/// Wrap a [`ChordError`] in the miette-shaped
/// [`SceneError::InvalidChord`] variant. Span is `(0, chord.len())`
/// against a synthesised `<keybind>` source — sufficient for the
/// `ark scene check` surface today; will sharpen once
/// `KeybindNode` carries a real span (TODO upstream).
fn chord_error_to_scene_error(chord: &str, e: ChordError) -> SceneError {
    SceneError::InvalidChord {
        chord: chord.to_string(),
        reason: e.to_string(),
        src: NamedSource::new("<keybind>", chord.to_string()),
        at: (0, chord.len()).into(),
    }
}

fn walk_tab(tab: &TabNode, errors: &mut Vec<SceneError>) {
    if let Some(when) = tab.when.as_deref() {
        collect(validate_cel(when, "tab.when", 0), errors);
    }
    for pane in &tab.panes {
        walk_pane(pane, errors);
    }
}

fn walk_pane(pane: &PaneNode, errors: &mut Vec<SceneError>) {
    if let Some(when) = pane.when.as_deref() {
        collect(validate_cel(when, "pane.when", 0), errors);
    }
    for inner in &pane.panes {
        walk_pane(inner, errors);
    }
}

fn walk_on(on: &OnNode, errors: &mut Vec<SceneError>) {
    if let Some(if_) = on.if_.as_deref() {
        collect(validate_cel(if_, "on.if", 0), errors);
    }
}

fn collect(result: Result<(), SceneError>, errors: &mut Vec<SceneError>) {
    if let Err(e) = result {
        errors.push(e);
    }
}

/// Cheap source-level proxy for CEL AST depth. Counts the deepest
/// run of nested `(` + dot-chain depth, ignoring contents inside
/// string literals (so strings like `"((("` don't inflate the count).
///
/// Not a perfect AST-depth computation — cel-interpreter 0.10 hides
/// its `Expression` type — but captures the adversarial shape we
/// care about: deeply nested call expressions and long
/// field-access chains both increment this counter.
fn approximate_ast_depth(src: &str) -> usize {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut chain: usize = 0;
    let mut max_chain: usize = 0;
    let mut in_str = false;
    let mut prev_was_ident_end = false;
    let mut escape = false;
    for ch in src.chars() {
        if escape {
            escape = false;
            continue;
        }
        if in_str {
            match ch {
                '\\' => escape = true,
                '"' | '\'' => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_str = true,
            '(' | '[' | '{' => {
                depth += 1;
                if depth > max_depth {
                    max_depth = depth;
                }
                prev_was_ident_end = false;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                prev_was_ident_end = true;
            }
            '.' => {
                if prev_was_ident_end {
                    chain += 1;
                    if chain > max_chain {
                        max_chain = chain;
                    }
                } else {
                    chain = 0;
                }
                prev_was_ident_end = false;
            }
            c if c.is_alphanumeric() || c == '_' => {
                prev_was_ident_end = true;
            }
            _ => {
                // whitespace / operator — chain doesn't reset on
                // whitespace (`foo . bar` is legal CEL), but any
                // non-`.` ident-break resets.
                if !ch.is_whitespace() {
                    chain = 0;
                }
                prev_was_ident_end = false;
            }
        }
    }
    max_depth.max(max_chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    // --- validate_cel direct tests ---

    #[test]
    fn valid_cel_passes_validation() {
        validate_cel("event.kind == \"progress\"", "t", 0).expect("should pass");
    }

    /// Rule: oversize expression → SceneError::CelExpressionTooLong
    /// with `cel/expression-too-long` miette code.
    #[test]
    fn oversize_expression_rejected() {
        let big = "1".repeat(MAX_EXPRESSION_LENGTH + 1);
        let err = validate_cel(&big, "t", 0).expect_err("should reject");
        assert_eq!(err.code_enum(), ErrorCode::CelExpressionTooLong);
    }

    /// Oversize by exactly one byte — boundary check.
    #[test]
    fn boundary_length_cases() {
        // Exactly MAX is allowed (compile may still fail — just a
        // 4096-byte numeric literal, which CEL may accept).
        let at_limit = "1".repeat(MAX_EXPRESSION_LENGTH);
        match validate_cel(&at_limit, "t", 0) {
            Ok(()) => {}
            Err(e) => {
                // Accept compile failure (the huge number may not
                // parse), but NOT CelExpressionTooLong.
                assert_ne!(e.code_enum(), ErrorCode::CelExpressionTooLong);
            }
        }
        // One byte over → always CelExpressionTooLong.
        let over = "1".repeat(MAX_EXPRESSION_LENGTH + 1);
        let err = validate_cel(&over, "t", 0).expect_err("over limit");
        assert_eq!(err.code_enum(), ErrorCode::CelExpressionTooLong);
    }

    /// Rule: deep AST → SceneError::CelAstTooDeep with
    /// `cel/ast-too-deep` miette code.
    #[test]
    fn deep_ast_rejected() {
        // Build a source with >64 nested parens.
        let depth = MAX_AST_DEPTH + 5;
        let mut expr = String::new();
        for _ in 0..depth {
            expr.push('(');
        }
        expr.push('1');
        for _ in 0..depth {
            expr.push(')');
        }
        let err = validate_cel(&expr, "t", 0).expect_err("should reject");
        assert_eq!(err.code_enum(), ErrorCode::CelAstTooDeep);
    }

    /// Deeply nested but still under the limit should not trip the
    /// depth guard (may still fail at CEL parse for other reasons,
    /// but NOT CelAstTooDeep).
    #[test]
    fn depth_under_limit_passes() {
        let depth = 10; // well under MAX_AST_DEPTH
        let mut expr = String::new();
        for _ in 0..depth {
            expr.push('(');
        }
        expr.push('1');
        for _ in 0..depth {
            expr.push(')');
        }
        // `(((((1)))))` — legal CEL.
        validate_cel(&expr, "t", 0).expect("should pass");
    }

    /// Parens inside string literals don't count toward depth.
    #[test]
    fn parens_in_string_ignored() {
        let expr = format!(r#""{}""#, "(".repeat(MAX_AST_DEPTH + 10));
        // Should NOT be flagged as CelAstTooDeep — the parens are
        // inside a string literal.
        let result = validate_cel(&expr, "t", 0);
        match result {
            Ok(()) => {}
            Err(e) => {
                assert_ne!(e.code_enum(), ErrorCode::CelAstTooDeep);
            }
        }
    }

    /// CEL parse errors propagate through validation as CelParse.
    #[test]
    fn parse_error_propagates() {
        let err = validate_cel("(", "t", 0).expect_err("syntax error");
        assert_eq!(err.code_enum(), ErrorCode::CelParse);
    }

    // --- validate_template tests ---

    /// Well-formed template passes.
    #[test]
    fn valid_template_passes() {
        validate_template("{{ cwd }} {{ id }}").expect("pass");
    }

    /// Rule: missing template var → SceneError (TemplateRender).
    /// Note: `validate_template` only syntax-checks; the full
    /// strict-rendering check runs through `compile_time_render`
    /// with a concrete `LayoutVars`. This test exercises that
    /// path since it's the one `ark scene check` ultimately wires.
    #[test]
    fn missing_template_var_surfaces_render_error() {
        use crate::template::{compile_time_render, LayoutVars};
        let vars = LayoutVars {
            cwd: "/tmp".into(),
            agent_cmd: "x".into(),
            agent_args: vec![],
            id: "id".into(),
            name: "n".into(),
        };
        let err =
            compile_time_render("{{ nope }}", &vars).expect_err("missing var should fail");
        assert_eq!(err.code_enum(), ErrorCode::TemplateRender);
    }

    /// Template syntax error propagates as TemplateCompile.
    #[test]
    fn template_syntax_error_propagates() {
        let err = validate_template("{{ unclosed").expect_err("bad syntax");
        assert_eq!(err.code_enum(), ErrorCode::TemplateCompile);
    }

    // --- validate_scene full-walk tests ---

    fn parse_scene(src: &str) -> SceneDoc {
        facet_kdl::from_str(src).expect("parse scene")
    }

    /// A clean scene walks cleanly.
    #[test]
    fn clean_scene_passes_validation() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "work" when="event.kind == \"started\"" {
            pane command="bash"
        }
    }
    on "AgentReady" if="agent.name == \"builder\"" { }
}
"#,
        );
        validate_scene(&doc).expect("clean scene");
    }

    /// A scene with an invalid `when` CEL on a tab surfaces a
    /// CelParse diagnostic through validate_scene.
    #[test]
    fn scene_with_invalid_tab_when_is_cel_parse() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "w" when="(" { }
    }
}
"#,
        );
        let errs = validate_scene(&doc).expect_err("should fail");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::CelParse);
    }

    /// Multiple errors accumulate — the walker doesn't short-circuit.
    #[test]
    fn multiple_errors_accumulate() {
        let doc = parse_scene(
            r#"
scene "s" {
    layout {
        tab "w" when="(" { }
    }
    on "AgentReady" if="(" { }
}
"#,
        );
        let errs = validate_scene(&doc).expect_err("should fail");
        assert_eq!(errs.len(), 2);
        for e in &errs {
            assert_eq!(e.code_enum(), ErrorCode::CelParse);
        }
    }

    /// Oversize `when=` on a pane surfaces CelExpressionTooLong via
    // --- T-6.6 chord validation tests ---

    /// Valid chords (`Alt p`, `Ctrl Shift t`, `F4`) pass the
    /// validate_scene walk.
    #[test]
    fn scene_with_valid_keybind_chords_passes() {
        let doc = parse_scene(
            r#"
scene "s" {
    keybind "Alt p" intent="picker.show"
    keybind "Ctrl Shift t" intent="tab.new"
    keybind "F4" intent="quit"
}
"#,
        );
        validate_scene(&doc).expect("all chords legal");
    }

    /// Invalid chords surface as `scene/invalid-chord`.
    #[test]
    fn scene_with_invalid_chord_rejected() {
        let doc = parse_scene(
            r#"
scene "s" {
    keybind "Hyper p" intent="picker.show"
}
"#,
        );
        let errs = validate_scene(&doc).expect_err("must reject");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::InvalidChord);
    }

    /// Empty chord surfaces as `scene/invalid-chord` (not Grammar).
    #[test]
    fn scene_with_empty_chord_rejected_as_invalid_chord() {
        let doc = parse_scene(
            r#"
scene "s" {
    keybind "" intent="x"
}
"#,
        );
        let errs = validate_scene(&doc).expect_err("must reject");
        assert!(
            errs.iter().any(|e| e.code_enum() == ErrorCode::InvalidChord),
            "expected at least one InvalidChord, got {errs:?}"
        );
    }

    /// Multiple bad chords accumulate (the validator never
    /// short-circuits).
    #[test]
    fn multiple_invalid_chords_accumulate() {
        let doc = parse_scene(
            r#"
scene "s" {
    keybind "Hyper p" intent="a"
    keybind "Alt &!" intent="b"
}
"#,
        );
        let errs = validate_scene(&doc).expect_err("must reject");
        assert_eq!(errs.len(), 2);
        assert!(
            errs.iter().all(|e| e.code_enum() == ErrorCode::InvalidChord),
            "expected all InvalidChord, got {errs:?}"
        );
    }

    /// validate_scene.
    #[test]
    fn scene_with_oversize_when_rejected() {
        let big = "1".repeat(MAX_EXPRESSION_LENGTH + 1);
        let src = format!(
            r#"
scene "s" {{
    layout {{
        tab "w" {{
            pane when="{big}"
        }}
    }}
}}
"#
        );
        let doc = parse_scene(&src);
        let errs = validate_scene(&doc).expect_err("too long");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::CelExpressionTooLong);
    }
}
