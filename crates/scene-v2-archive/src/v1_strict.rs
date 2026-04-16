//! v1.0-strict validation (T-15.3).
//!
//! `ark scene check --v1-strict` flips this pass on. It enforces the
//! contract frozen in `context/refs/intent-api-v1.md` and
//! `context/refs/wasm-metadata-v1.md`: only the 17 `ark.core.*` ops
//! are allowed, only the v0.4 capability vocabulary is allowed, and
//! extension-contributed ops are rejected.
//!
//! Used as the CI gate for shipped scenes. User scenes do NOT have
//! to pass this — the default (non-strict) `ark scene check` is
//! intentionally more permissive.
//!
//! # Walker choice: raw KDL, not the typed AST
//!
//! The typed AST encodes ops as an opaque `OpNode` bag (see
//! TODO(T-3.2) in `ast.rs`) — the op's name lives only on the KDL
//! node itself, not on the typed struct. We walk the raw
//! [`kdl::KdlDocument`] directly, mirroring the pattern already
//! used by [`crate::ops::validate`] (T-4.3 op cross-ref checker).
//! When the AST grows a typed op enum at T-3.2, this file switches
//! over; the public surface stays stable.
//!
//! # What v1-strict enforces
//!
//! 1. **Allowed op names.** Inside `on { }` and `keybind { }` bodies,
//!    every op node name MUST be a member of [`ALLOWED_OP_VERBS`] —
//!    the 17 short KDL verbs corresponding to the `ark.core.*`
//!    namespace. Extension-contributed op verbs (even legally-named
//!    ones) are rejected at strict time because a shipped scene MUST
//!    NOT assume any ext-contributed surface.
//! 2. **`keybind intent=` form.** When a `keybind` uses the
//!    `intent="<name>"` shorthand (R5), the intent name MUST be one
//!    of [`ALLOWED_INTENT_NAMES`] (the fully-qualified forms).
//! 3. **No deprecated ops.** v1.0 ships with zero deprecations; this
//!    pass is a placeholder for the
//!    `warning[scene/deprecated-op]` upgrade once an op enters the
//!    deprecation window (per `intent-api-v1.md` §"Deprecation
//!    policy").
//!
//! # What v1-strict does NOT enforce (by design)
//!
//! - **Extension metadata capabilities.** The capability-vocabulary
//!   upgrade (warning → error) lives in the `ark ext inspect` path;
//!   scene check can surface it transitively when a scene `use`s
//!   an extension with unknown caps, but the first-class check is
//!   over there.
//! - **Engine wiring.** `engine { command "…" }` points at an ACP
//!   agent that may or may not be wired. T-15.3 catches the literal
//!   contract (v1 engines = claude/codex/gemini-cli via
//!   `use "engine-*"` or by ACP-compliant `command`), not runtime
//!   reachability.
//!
//! # Public surface
//!
//! - [`v1_strict_validate`] is the single entry point. It takes the
//!   scene source + parsed [`kdl::KdlDocument`] + the display path,
//!   and returns either `Ok(())` or the full bag of
//!   [`SceneError::V1Strict`] diagnostics found in one pass (no
//!   short-circuit).
//! - [`ALLOWED_OP_VERBS`] and [`ALLOWED_INTENT_NAMES`] are exported
//!   so docs / schema-dump paths can enumerate the frozen surface
//!   without reaching into the module internals.

use std::path::Path;

use kdl::{KdlDocument, KdlNode};
use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;

/// Short KDL verbs frozen under v1.0 (one per `ark.core.*` op).
///
/// Kept in sync with [`crate::ops::CORE_OP_NAMES`] — the verbs here
/// strip the `ark.core.` prefix from that list. A mismatch between
/// the two is a drift bug; the test
/// [`tests::allowed_op_verbs_match_core_op_names`] pins them
/// together.
pub const ALLOWED_OP_VERBS: &[&str] = &[
    // tabs
    "open_tab",
    "close_tab",
    "rename_tab",
    "focus_tab",
    // panes
    "split_pane",
    "close_pane",
    // plugins
    "mount_plugin",
    "unmount_plugin",
    // messaging
    "pipe",
    "emit",
    "set_status",
    // control
    "exec",
    "reload_scene",
    // acp
    "prompt",
    "acp_cancel",
    "acp_permit",
    "set_mode",
];

/// Fully-qualified intent names frozen under v1.0. Exactly
/// [`ALLOWED_OP_VERBS`] with the `ark.core.` prefix reattached —
/// kept as a separate const so the `keybind intent="…"` check
/// matches on the form scene authors actually write (which the
/// `intent=` shorthand requires).
///
/// Same invariant pinned by
/// [`tests::allowed_intent_names_match_core_op_names`].
pub const ALLOWED_INTENT_NAMES: &[&str] = &[
    "ark.core.open_tab",
    "ark.core.close_tab",
    "ark.core.rename_tab",
    "ark.core.focus_tab",
    "ark.core.split_pane",
    "ark.core.close_pane",
    "ark.core.mount_plugin",
    "ark.core.unmount_plugin",
    "ark.core.pipe",
    "ark.core.emit",
    "ark.core.set_status",
    "ark.core.exec",
    "ark.core.reload_scene",
    "ark.core.prompt",
    "ark.core.acp_cancel",
    "ark.core.acp_permit",
    "ark.core.set_mode",
];

/// v1.0-strict validation over a parsed scene.
///
/// Walks the raw KDL document and returns every contract violation
/// encountered in one pass. Returns `Ok(())` when the scene passes
/// strict mode; `Err(Vec<SceneError>)` otherwise.
///
/// Callers: `ark scene check --v1-strict` and the CI lane for
/// shipped scenes. User scenes typically run the non-strict default
/// which tolerates extension-contributed ops.
///
/// # Arguments
///
/// - `source` — scene source text. Held verbatim inside the
///   returned diagnostics' `NamedSource` so miette can render the
///   caret.
/// - `src_name` — display path (or synthetic label like
///   `"<built-in>"`) used in the rendered diagnostic header.
/// - `doc` — parsed KDL document. Callers already have this in
///   hand from [`crate::parse::parse_scene`]; passing it in avoids
///   re-parsing.
pub fn v1_strict_validate(
    source: &str,
    src_name: &Path,
    doc: &KdlDocument,
) -> Result<(), Vec<SceneError>> {
    let mut errors = Vec::new();
    let Some(scene_body) = scene_body(doc) else {
        // No `scene { }` wrapper — the non-strict check will have
        // already surfaced `scene/empty-or-unknown` /
        // `scene/ambiguous-file-shape`. No work for v1-strict.
        return Ok(());
    };
    for node in scene_body.nodes() {
        match node.name().value() {
            "on" => {
                walk_op_body(node, source, src_name, &mut errors);
            }
            "keybind" => {
                check_keybind_intent(node, source, src_name, &mut errors);
                walk_op_body(node, source, src_name, &mut errors);
            }
            _ => {}
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Enter the `scene { … }` body. Mirrors
/// [`crate::ops::validate::scene_body`] but private to this module
/// so the two validators can diverge without entangling signatures.
fn scene_body(doc: &KdlDocument) -> Option<&KdlDocument> {
    let scene = doc.nodes().iter().find(|n| n.name().value() == "scene")?;
    scene.children()
}

/// Walk an `on { }` / `keybind { }` body flagging any op node
/// whose name is not in [`ALLOWED_OP_VERBS`].
fn walk_op_body(node: &KdlNode, source: &str, src_name: &Path, errors: &mut Vec<SceneError>) {
    let Some(body) = node.children() else {
        return;
    };
    for op in body.nodes() {
        let verb = op.name().value();
        if ALLOWED_OP_VERBS.contains(&verb) {
            continue;
        }
        let span = kdl_span(op.name().span());
        let named = NamedSource::new(src_name.display().to_string(), source.to_string());
        errors.push(SceneError::v1_strict(
            format!(
                "op `{verb}` is outside the frozen `ark.core.*` vocabulary"
            ),
            format!(
                "v1-strict mode only accepts the 17 frozen ops. Allowed verbs: {}. \
                 See context/refs/intent-api-v1.md for the full contract.",
                ALLOWED_OP_VERBS.join(", ")
            ),
            named,
            span,
        ));
    }
}

/// Flag a `keybind "<chord>" intent="<name>"` shorthand whose
/// `intent=` value is not in [`ALLOWED_INTENT_NAMES`].
fn check_keybind_intent(
    node: &KdlNode,
    source: &str,
    src_name: &Path,
    errors: &mut Vec<SceneError>,
) {
    let Some(entry) = node
        .entries()
        .iter()
        .find(|e| e.name().map(|n| n.value() == "intent").unwrap_or(false))
    else {
        return;
    };
    let Some(value) = entry.value().as_string() else {
        return;
    };
    if ALLOWED_INTENT_NAMES.contains(&value) {
        return;
    }
    let span = kdl_span(entry.span());
    let named = NamedSource::new(src_name.display().to_string(), source.to_string());
    errors.push(SceneError::v1_strict(
        format!(
            "keybind intent `{value}` is outside the frozen `ark.core.*` vocabulary"
        ),
        format!(
            "v1-strict mode only accepts intents from the 17 frozen ops. Allowed names: {}.",
            ALLOWED_INTENT_NAMES.join(", ")
        ),
        named,
        span,
    ));
}

/// Translate a `miette::SourceSpan`-compatible range out of a
/// `kdl::KdlSpan`. KDL 2.0's `KdlNode::span()` /
/// `KdlEntry::span()` return `SourceSpan` directly — we just
/// copy the components so the SceneError carries a fresh `SourceSpan`.
fn kdl_span(span: SourceSpan) -> SourceSpan {
    SourceSpan::new(span.offset().into(), span.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::ops::CORE_OP_NAMES;
    use std::path::PathBuf;

    fn parse(src: &str) -> KdlDocument {
        src.parse().expect("test fixture is valid KDL")
    }

    // --- invariants: const tables track the real registry --------------

    /// [`ALLOWED_OP_VERBS`] MUST equal [`CORE_OP_NAMES`] stripped of
    /// the `ark.core.` prefix. Drift here breaks the v1 contract.
    #[test]
    fn allowed_op_verbs_match_core_op_names() {
        let expected: Vec<&str> = CORE_OP_NAMES
            .iter()
            .map(|n| n.strip_prefix("ark.core.").unwrap_or(n))
            .collect();
        let actual: Vec<&str> = ALLOWED_OP_VERBS.iter().copied().collect();
        assert_eq!(actual, expected);
        assert_eq!(ALLOWED_OP_VERBS.len(), 17, "v1.0 freezes 17 ops");
    }

    /// [`ALLOWED_INTENT_NAMES`] MUST equal [`CORE_OP_NAMES`] exactly.
    #[test]
    fn allowed_intent_names_match_core_op_names() {
        let expected: Vec<&str> = CORE_OP_NAMES.iter().copied().collect();
        let actual: Vec<&str> = ALLOWED_INTENT_NAMES.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    // --- happy path ----------------------------------------------------

    /// A scene using only `ark.core.*` ops passes strict mode.
    #[test]
    fn all_core_ops_pass_strict() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        open_tab name="work"
        split_pane into="work" side="right"
        emit "user.tick"
        exec script="echo hi"
        reload_scene
    }
    keybind "Alt p" intent="ark.core.prompt"
}
"#;
        let doc = parse(src);
        v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect("core ops must pass strict");
    }

    /// A scene with no `scene { }` wrapper is a v1-strict no-op
    /// (non-strict check has already surfaced the shape error).
    #[test]
    fn missing_scene_wrapper_is_noop() {
        let src = r#"// stray comment"#;
        let doc = parse(src);
        v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect("no-op on missing wrapper");
    }

    // --- op-vocabulary rejections --------------------------------------

    /// An ext-contributed op inside `on { }` is rejected with
    /// `scene/v1-strict`.
    #[test]
    fn ext_op_in_on_body_rejected() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        my_ext.do_thing arg="value"
    }
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("ext op must fail strict");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::V1Strict);
        match &errs[0] {
            SceneError::V1Strict { reason, .. } => {
                assert!(
                    reason.contains("my_ext.do_thing"),
                    "reason should name the op: {reason:?}"
                );
            }
            other => panic!("expected V1Strict, got {other:?}"),
        }
    }

    /// An ext-contributed op inside a `keybind { }` body is also rejected.
    #[test]
    fn ext_op_in_keybind_body_rejected() {
        let src = r#"
scene "s" {
    keybind "Alt p" {
        unknown_verb x=1
    }
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("ext op must fail strict");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::V1Strict);
    }

    /// A typo'd op name (e.g. `open_ab`) is rejected. Strict doesn't
    /// try to do typo suggestions — the non-strict unknown-node
    /// suggestion surface is the right place for that.
    #[test]
    fn typo_op_name_rejected() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        open_ab name="work"
    }
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("typo must fail strict");
        assert_eq!(errs.len(), 1);
    }

    // --- keybind intent= form -------------------------------------------

    /// `keybind intent="ark.core.*"` passes strict.
    #[test]
    fn keybind_intent_core_passes() {
        let src = r#"
scene "s" {
    keybind "Alt p" intent="ark.core.prompt"
    keybind "Alt q" intent="ark.core.acp_cancel"
}
"#;
        let doc = parse(src);
        v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect("core intents pass");
    }

    /// `keybind intent="my_ext.do_thing"` fails strict.
    #[test]
    fn keybind_intent_ext_rejected() {
        let src = r#"
scene "s" {
    keybind "Alt p" intent="my_ext.do_thing"
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("ext intent must fail");
        assert_eq!(errs.len(), 1);
        match &errs[0] {
            SceneError::V1Strict { reason, .. } => {
                assert!(reason.contains("my_ext.do_thing"));
            }
            other => panic!("expected V1Strict, got {other:?}"),
        }
    }

    /// `keybind intent="open_tab"` (short verb, not namespaced) fails
    /// strict. v1 freezes on the fully-qualified form.
    #[test]
    fn keybind_intent_short_verb_rejected() {
        let src = r#"
scene "s" {
    keybind "Alt p" intent="open_tab"
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("short-verb intent must fail strict");
        assert_eq!(errs.len(), 1);
    }

    // --- multiple violations accumulate --------------------------------

    /// Multiple violations surface in one pass — the walker never
    /// short-circuits.
    #[test]
    fn multiple_violations_accumulate() {
        let src = r#"
scene "s" {
    on "AgentReady" {
        unknown_a
        unknown_b
    }
    keybind "Alt p" intent="unknown.c"
}
"#;
        let doc = parse(src);
        let errs = v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect_err("three violations");
        assert_eq!(errs.len(), 3);
        for e in &errs {
            assert_eq!(e.code_enum(), ErrorCode::V1Strict);
        }
    }

    /// Strict mode ignores nodes outside `on { }` / `keybind { }` —
    /// e.g. `plugin { source … }` can carry whatever it wants.
    #[test]
    fn non_op_nodes_are_ignored() {
        let src = r#"
scene "s" {
    plugin "my-picker" { source "ext:my-ext" }
    layout {
        tab name="work" { pane }
    }
}
"#;
        let doc = parse(src);
        v1_strict_validate(src, &PathBuf::from("scene.kdl"), &doc)
            .expect("non-op nodes ignored");
    }
}
