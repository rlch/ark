//! Emit-variant restriction + compile-time cycle detection (T-5.5).
//!
//! R4 splits `AgentEvent` into two halves:
//!
//! * **Core events** (`Started`, `PhaseTransition`, `ToolUse`, …) come
//!   from the supervisor, agent, or plugin layers. Scenes react to
//!   them but MUST NOT emit them.
//! * **User events** (`UserEvent:<namespaced.name>`) are the scene
//!   author's domain. Scenes can both react to and emit them.
//!
//! The restriction is two-fold:
//!
//! 1. `emit "Started"`, `emit "Failed"`, etc. surface as
//!    [`SceneError::EmitNonUserEvent`] at `ark scene check` time.
//! 2. With emits restricted to user events, the reaction graph has a
//!    tractable structure — user-event emit targets fan into
//!    `on "UserEvent:<name>"` selectors. A depth-first walk detects
//!    static cycles ([`SceneError::EmitCycle`]) before they hit the
//!    runtime cascade bound (T-5.4).
//!
//! A third helper, [`validate_emit_sources`], enforces the R4
//! canonical `UserEvent.source` attribution set when a scene author
//! explicitly sets `source="<X>"` on an `emit` op.
//!
//! # Walker choice
//!
//! Like `validate_op_refs` (T-4.3), this pass walks the raw
//! `kdl::KdlDocument` because the typed AST's `OpNode` is an opaque
//! positional-arg bag today (see `ast.rs` TODO(T-3.2)). When the AST
//! grows a typed op enum, the public API (`validate_emit_ops`,
//! `detect_emit_cycles`) stays stable; internals flip from the KDL
//! walker to typed-AST iteration.

use std::collections::{BTreeMap, BTreeSet};

use kdl::KdlDocument;

use crate::error::SceneError;
use crate::reactions::EventKind;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the full T-5.5 validation pass: emit-variant restriction,
/// cycle detection, and source-attribution sanity.
///
/// Returns every error found in one pass (no short-circuit), so
/// `ark scene check` renders the full diagnostic set.
pub fn validate_emit_ops(doc: &KdlDocument) -> Result<(), Vec<SceneError>> {
    let mut errors = Vec::new();

    // Pass 1: variant restriction + source attribution (per-emit
    // checks, local to the op).
    check_emit_targets(doc, &mut errors);
    check_emit_sources(doc, &mut errors);

    // Pass 2: cycle detection (global, graph-level).
    if let Err(mut cycle_errors) = detect_emit_cycles(doc) {
        errors.append(&mut cycle_errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Pass 1: variant + source
// ---------------------------------------------------------------------------

/// Walk every `on { … }` / `keybind { … }` body and surface any `emit`
/// op whose target is not a `UserEvent:<name>`.
///
/// Scene authors write `emit "user.picker.accept"` (unprefixed,
/// rewritten to `UserEvent:user.picker.accept` by the R11
/// context-sensitive unprefixed-name rewrite — for this pass we
/// treat unprefixed names as user events by default, which is the
/// rewrite's effect). The error fires when the target explicitly
/// names a core kind like `emit "Started"`.
fn check_emit_targets(doc: &KdlDocument, errors: &mut Vec<SceneError>) {
    for_each_emit(doc, |target, _source| {
        if let Some(stripped) = strip_user_event_prefix(target) {
            let _ = stripped;
            return;
        }
        // Fully qualified core kind? That's the forbidden shape.
        if EventKind::parse(target).is_some() {
            errors.push(SceneError::EmitNonUserEvent {
                target: target.to_string(),
            });
        }
        // Otherwise (unprefixed user event name like "user.hello"):
        // treat as a user event. The R11 rewrite prepends the right
        // namespace; we don't second-guess it here.
    });
}

/// Walk every `emit` op and surface `source="<X>"` values outside the
/// R4 canonical set.
fn check_emit_sources(doc: &KdlDocument, errors: &mut Vec<SceneError>) {
    for_each_emit(doc, |_target, source| {
        if let Some(src) = source {
            if !is_canonical_source(src) {
                errors.push(SceneError::EmitInvalidSource {
                    value: src.to_string(),
                });
            }
        }
    });
}

/// Canonical `UserEvent.source` values from R4:
///
/// * `core` — emitted by ark-core.
/// * `scene` — emitted by a scene reaction (`emit` op; default).
/// * `ext:<name>` — emitted by an ark-native extension.
/// * `plugin:<name>` — emitted by a zellij wasm plugin.
/// * `hook:<name>` — emitted by a legacy TOML `[[hooks]]` entry.
/// * `agent` — emitted by the ACP agent (reserved; v1 has no scene
///   surface for this).
pub fn is_canonical_source(s: &str) -> bool {
    match s {
        "core" | "scene" | "agent" => true,
        _ => {
            s.starts_with("ext:")
                || s.starts_with("plugin:")
                || s.starts_with("hook:")
        }
    }
}

// ---------------------------------------------------------------------------
// Pass 2: cycle detection
// ---------------------------------------------------------------------------

/// Build the emit DAG and walk for cycles.
///
/// Nodes = user-event names (both sides: the `UserEvent:<name>`
/// reaction selectors and the `emit "<name>"` op targets). Edges run
/// from a reaction's selector to every user-event name the reaction's
/// body emits. A cycle is a DFS back-edge.
///
/// Example:
/// ```text
/// on "UserEvent:a" { emit "b" }
/// on "UserEvent:b" { emit "a" }
///
/// Graph: a → b, b → a
/// Cycle: a → b → a
/// ```
pub fn detect_emit_cycles(doc: &KdlDocument) -> Result<(), Vec<SceneError>> {
    let graph = build_emit_graph(doc);

    let mut errors = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for start in graph.keys() {
        if seen.contains(start) {
            continue;
        }
        // DFS with a path stack — when a neighbor is already on the
        // stack, we've closed a cycle.
        let mut stack: Vec<String> = vec![start.clone()];
        let mut path: Vec<String> = vec![start.clone()];
        let mut path_set: BTreeSet<String> = BTreeSet::new();
        path_set.insert(start.clone());

        dfs_collect(&graph, &mut stack, &mut path, &mut path_set, &mut errors, &mut seen);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Depth-first walk collecting every cycle's trail as a
/// `SceneError::EmitCycle`. Iterative (stack-based) to avoid borrowing
/// recursion issues.
fn dfs_collect(
    graph: &BTreeMap<String, BTreeSet<String>>,
    _stack: &mut Vec<String>,
    path: &mut Vec<String>,
    path_set: &mut BTreeSet<String>,
    errors: &mut Vec<SceneError>,
    seen: &mut BTreeSet<String>,
) {
    let current = match path.last() {
        Some(v) => v.clone(),
        None => return,
    };
    seen.insert(current.clone());

    let empty = BTreeSet::new();
    let neighbors = graph.get(&current).unwrap_or(&empty);
    for next in neighbors {
        if path_set.contains(next) {
            // Found cycle — render trail starting from `next`.
            let start_idx = path.iter().position(|p| p == next).unwrap_or(0);
            let mut trail_nodes: Vec<&str> =
                path.iter().skip(start_idx).map(|s| s.as_str()).collect();
            trail_nodes.push(next.as_str());
            let trail = trail_nodes.join(" → ");
            let err = SceneError::EmitCycle { trail };
            // De-dup: cycles found starting from different nodes in
            // the same SCC would produce identical trails. Since the
            // canonical form starts from the cycle's own first node
            // alphabetically wouldn't match here (path order is
            // DFS-specific), we accept at most one error per `trail`
            // string via a local set.
            if !errors.iter().any(|e| matches!(e, SceneError::EmitCycle { trail: t } if t == &match &err { SceneError::EmitCycle { trail } => trail.clone(), _ => String::new() })) {
                errors.push(err);
            }
            continue;
        }
        if seen.contains(next) {
            continue;
        }
        path.push(next.clone());
        path_set.insert(next.clone());
        dfs_collect(graph, _stack, path, path_set, errors, seen);
        let popped = path.pop();
        if let Some(p) = popped {
            path_set.remove(&p);
        }
    }
}

/// Construct the emit graph.
///
/// Keys = user-event names declared in `on "UserEvent:<name>"` selectors.
/// Values = set of user-event names each reaction body emits.
/// Orphan emit targets (no matching `on` handler) surface as nodes with
/// no outgoing edges — they can't participate in cycles, which is the
/// correct pruning: an unreachable emit is a user concern the validator
/// doesn't need to gate.
fn build_emit_graph(doc: &KdlDocument) -> BTreeMap<String, BTreeSet<String>> {
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let scene_body = match scene_body(doc) {
        Some(b) => b,
        None => return graph,
    };

    for node in scene_body.nodes() {
        let name = node.name().value();
        if name != "on" && name != "keybind" {
            continue;
        }

        // Selector: first positional argument of the `on` node.
        let selector = match node
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_string())
        {
            Some(s) => s,
            None => continue,
        };
        let from_name = match strip_user_event_prefix(selector) {
            Some(n) => n.to_string(),
            None => continue, // Only UserEvent:<name> selectors participate.
        };

        // Body: collect `emit "<name>"` targets.
        let emits = collect_emit_targets(node.children());
        let entry = graph.entry(from_name).or_default();
        for target in emits {
            if let Some(user_name) = strip_user_event_prefix(&target) {
                entry.insert(user_name.to_string());
            } else if EventKind::parse(&target).is_some() {
                // Core-kind emit — handled by the variant restriction
                // pass; skip for cycle construction.
            } else {
                // Unprefixed name — treated as a user event per R11.
                entry.insert(target);
            }
        }
    }

    graph
}

/// Collect every `emit "<target>"` op target from a `kdl` body.
/// Filters nothing — returns every emit literal verbatim so the caller
/// can classify.
fn collect_emit_targets(body: Option<&KdlDocument>) -> Vec<String> {
    let mut out = Vec::new();
    let body = match body {
        Some(b) => b,
        None => return out,
    };
    for node in body.nodes() {
        if node.name().value() != "emit" {
            continue;
        }
        // First positional argument = target.
        if let Some(target) = node
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_string())
        {
            out.push(target.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip the `UserEvent:` / `user_event:` prefix from a target string,
/// returning the namespaced name. `None` for non-prefixed inputs.
fn strip_user_event_prefix(s: &str) -> Option<&str> {
    s.strip_prefix("UserEvent:")
        .or_else(|| s.strip_prefix("user_event:"))
}

/// Locate the `scene "<name>" { … }` body. Scenes without the wrapper
/// surface as `None` (pre-wrapper shapes handled elsewhere by R15).
fn scene_body(doc: &KdlDocument) -> Option<&KdlDocument> {
    doc.nodes()
        .iter()
        .find(|n| n.name().value() == "scene")
        .and_then(|n| n.children())
}

/// Iterate every `emit` op in every `on`/`keybind` body, invoking the
/// callback with `(target, source?)` — `source` is the value of the
/// `source=` property when set.
fn for_each_emit<F: FnMut(&str, Option<&str>)>(doc: &KdlDocument, mut cb: F) {
    let scene_body = match scene_body(doc) {
        Some(b) => b,
        None => return,
    };
    for node in scene_body.nodes() {
        let name = node.name().value();
        if name != "on" && name != "keybind" {
            continue;
        }
        let body = match node.children() {
            Some(b) => b,
            None => continue,
        };
        for op in body.nodes() {
            if op.name().value() != "emit" {
                continue;
            }
            let target = op
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_string());
            let source = op
                .entries()
                .iter()
                .find(|e| e.name().map(|n| n.value()) == Some("source"))
                .and_then(|e| e.value().as_string());
            if let Some(target) = target {
                cb(target, source);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level source validator (public wrapper) — useful when callers
// only want the source pass (e.g. extension's `ark scene explain`).
// ---------------------------------------------------------------------------

/// Validate only the `UserEvent.source` attribution pass. Exposed for
/// callers that already did the cycle + restriction passes elsewhere
/// and only want the source sanity check.
pub fn validate_emit_sources(doc: &KdlDocument) -> Result<(), Vec<SceneError>> {
    let mut errors = Vec::new();
    check_emit_sources(doc, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn doc(s: &str) -> KdlDocument {
        s.parse().expect("parse kdl")
    }

    // -- Variant restriction --------------------------------------------

    #[test]
    fn emit_user_event_unprefixed_name_is_ok() {
        let d = doc(
            r#"
scene "demo" {
    on "Started" {
        emit "user.hello"
    }
}
"#,
        );
        let res = validate_emit_ops(&d);
        assert!(res.is_ok(), "user.hello is fine: {res:?}");
    }

    #[test]
    fn emit_user_event_prefixed_is_ok() {
        let d = doc(
            r#"
scene "demo" {
    on "Started" {
        emit "UserEvent:user.hello"
    }
}
"#,
        );
        let res = validate_emit_ops(&d);
        assert!(res.is_ok());
    }

    #[test]
    fn emit_core_kind_is_rejected() {
        let d = doc(
            r#"
scene "demo" {
    on "Done" {
        emit "Started"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("must error");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::EmitNonUserEvent);
    }

    #[test]
    fn emit_multiple_core_kinds_all_surface() {
        let d = doc(
            r#"
scene "demo" {
    on "Done" {
        emit "Started"
        emit "PhaseTransition"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("must error");
        // Two emit restrictions should fire.
        let count = errs
            .iter()
            .filter(|e| e.code_enum() == ErrorCode::EmitNonUserEvent)
            .count();
        assert_eq!(count, 2);
    }

    // -- Cycle detection ------------------------------------------------

    #[test]
    fn simple_two_node_cycle_detected() {
        let d = doc(
            r#"
scene "demo" {
    on "UserEvent:a" {
        emit "b"
    }
    on "UserEvent:b" {
        emit "a"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("cycle");
        let has_cycle = errs
            .iter()
            .any(|e| e.code_enum() == ErrorCode::EmitCycle);
        assert!(has_cycle, "errs: {errs:?}");
    }

    #[test]
    fn self_loop_detected() {
        let d = doc(
            r#"
scene "demo" {
    on "UserEvent:a" {
        emit "a"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("self-loop");
        assert!(errs.iter().any(|e| e.code_enum() == ErrorCode::EmitCycle));
    }

    #[test]
    fn linear_chain_is_not_a_cycle() {
        let d = doc(
            r#"
scene "demo" {
    on "UserEvent:a" {
        emit "b"
    }
    on "UserEvent:b" {
        emit "c"
    }
    on "UserEvent:c" {
    }
}
"#,
        );
        assert!(validate_emit_ops(&d).is_ok());
    }

    #[test]
    fn three_node_cycle_detected() {
        let d = doc(
            r#"
scene "demo" {
    on "UserEvent:a" {
        emit "b"
    }
    on "UserEvent:b" {
        emit "c"
    }
    on "UserEvent:c" {
        emit "a"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("3-cycle");
        assert!(errs.iter().any(|e| e.code_enum() == ErrorCode::EmitCycle));
    }

    // -- Source attribution ---------------------------------------------

    #[test]
    fn canonical_sources_accepted() {
        for src in ["core", "scene", "agent", "ext:my", "plugin:p", "hook:h"] {
            let input = format!(
                r#"
scene "demo" {{
    on "Started" {{
        emit "user.x" source="{src}"
    }}
}}
"#
            );
            let d = doc(&input);
            assert!(
                validate_emit_ops(&d).is_ok(),
                "source `{src}` must be accepted"
            );
        }
    }

    #[test]
    fn invalid_source_rejected() {
        let d = doc(
            r#"
scene "demo" {
    on "Started" {
        emit "user.x" source="bogus"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("bad source");
        assert!(
            errs
                .iter()
                .any(|e| e.code_enum() == ErrorCode::EmitInvalidSource)
        );
    }

    #[test]
    fn emits_with_no_body_do_not_panic() {
        let d = doc(
            r#"
scene "demo" {
    on "Started"
    keybind "Alt+p" intent="picker.show"
}
"#,
        );
        assert!(validate_emit_ops(&d).is_ok());
    }

    #[test]
    fn is_canonical_source_matches_r4() {
        assert!(is_canonical_source("core"));
        assert!(is_canonical_source("scene"));
        assert!(is_canonical_source("agent"));
        assert!(is_canonical_source("ext:anything"));
        assert!(is_canonical_source("plugin:anything"));
        assert!(is_canonical_source("hook:anything"));
        assert!(!is_canonical_source(""));
        assert!(!is_canonical_source("user"));
        assert!(!is_canonical_source("bogus"));
    }

    // Insta snapshot of the rendered EmitCycle for documentation.
    #[test]
    fn emit_cycle_trail_format() {
        let d = doc(
            r#"
scene "demo" {
    on "UserEvent:x" {
        emit "y"
    }
    on "UserEvent:y" {
        emit "x"
    }
}
"#,
        );
        let errs = validate_emit_ops(&d).expect_err("cycle");
        let cycle = errs
            .iter()
            .find_map(|e| match e {
                SceneError::EmitCycle { trail } => Some(trail.clone()),
                _ => None,
            })
            .expect("cycle found");
        // Trail contains both names and the arrow separator.
        assert!(cycle.contains('→'));
        assert!(cycle.contains("x"));
        assert!(cycle.contains("y"));
    }
}
