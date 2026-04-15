//! Scope-rule enforcement pass.
//!
//! `facet-kdl` parses the scene document into typed nodes (see
//! `crate::ast`), but its structural-rejection surface is lax: nodes
//! whose names don't map to a declared `#[facet(kdl::child[ren])]`
//! field are silently ignored rather than raised as errors. For the
//! scope table in `cavekit-scene.md` R2 we need the stricter behaviour
//! — every misplaced node MUST surface as an `error[scene/...]`
//! diagnostic with a parent-context label.
//!
//! So we run a second pass over the raw KDL 2.0 document (the same
//! `kdl::KdlDocument::parse` path the formatter already uses) and walk
//! it by hand, checking each node + attribute against the R2 rule
//! table. Diagnostics produced here carry precise spans from the
//! upstream `kdl` crate's `KdlNode::span()` / `KdlEntry::span()`
//! surface.
//!
//! ## R2 rules enforced
//!
//! 1. `on`, `keybind`, `plugin`, `use`, `extends`, `include`,
//!    `engine`, `clear-reactions`, `clear-keybind`, `disable-plugin`
//!    are legal only at scene root.
//! 2. `tab`, `pane`, `floating-panes` are legal only inside a
//!    `layout { }` block (or nested inside another `pane`/`tab`).
//! 3. `when=` attribute is legal on `tab` and `pane` only.
//! 4. `source`, `mount`, `summon`, `dismiss`, `subscribes`, `config`
//!    are legal only inside a `plugin { }` block. A nested `on` is
//!    ALSO legal inside `plugin { }` (its lifecycle-marker form per
//!    R6), so `on` is special-cased: it's allowed at scene root OR
//!    inside `plugin`.
//! 5. `if=` attribute is legal on `on { }` nodes only.
//! 6. `intent=` attribute is legal on `keybind` (shorthand form) only.
//!
//! ## Not enforced here
//!
//! * R6's `plugin-ambiguous-lifecycle` (`summon` + `on` both present)
//!   is a plugin-body rule, not a pure scope rule — deferred to a
//!   later compile-pass task (covered by `SceneError::PluginAmbiguousLifecycle`).
//! * `extends`-chain loops, `include`-cycle detection, `engine`-vs-`use`
//!   mutual exclusion — cross-file / cross-node constraints that run
//!   after the full load graph is assembled.
//!
//! Those constraints share the same miette-backed `SceneError`
//! surface, they just fire later in the pipeline.
//!
//! ## Unknown-node-at-root handling
//!
//! When the scope walker finds a root-level node whose name isn't in
//! the scene-root whitelist, it emits `SceneError::UnknownNode`
//! populated with a typo suggestion from `crate::suggest::suggest_similar`
//! (T-1.3 wiring). That keeps R1's "did you mean …?" acceptance
//! criterion working for the *structural* unknowns facet-kdl silently
//! accepts.

use std::path::Path;

use kdl::{KdlDocument, KdlNode};
use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;
use crate::suggest::suggest_similar;

/// Scene-root node names admitted by R1.
///
/// Used both for scope checking and (via [`suggest_similar`]) for
/// typo-suggestions on unknown root nodes.
pub const SCENE_ROOT_NODES: &[&str] = &[
    "extends",
    "include",
    "use",
    "layout",
    "plugin",
    "on",
    "keybind",
    "engine",
    "clear-reactions",
    "clear-keybind",
    "disable-plugin",
];

/// Nodes legal inside a `layout { }` block (R2 clause 2). `pane` and
/// `tab` recurse into themselves; `floating-panes` is zellij-native
/// pass-through that only appears inside layout.
const LAYOUT_CHILD_NODES: &[&str] = &["tab", "pane", "floating-panes"];

/// Nodes legal inside a `plugin { }` body (R2 clause 4).
const PLUGIN_BODY_NODES: &[&str] = &[
    "source",
    "mount",
    "summon",
    "dismiss",
    "on",
    "subscribes",
    "config",
];

/// Nodes legal inside a `tab` or `pane` body — zellij-style recursion
/// plus `floating-panes`. Same acceptance as `LAYOUT_CHILD_NODES`
/// today; factored out to keep the intent explicit at the call sites.
const TAB_OR_PANE_CHILD_NODES: &[&str] = &["tab", "pane", "floating-panes"];

/// Walk a scene file's raw KDL document and report any R2 scope
/// violations.
///
/// Returns a vector of `SceneError` diagnostics. An empty vector means
/// every node and attribute satisfied the scope table. Callers should
/// run this AFTER [`crate::parse::parse_scene`] succeeds — parse
/// errors have their own dedicated surface and should not be
/// re-discovered here.
///
/// When the underlying KDL document itself fails to parse (which
/// shouldn't happen if `parse_scene` already succeeded, but is
/// defensively checked), a single [`SceneError::Parse`] is returned so
/// the caller sees one consistent error shape.
#[allow(clippy::result_large_err)]
pub fn check_scope(src: &str, path: &Path) -> Vec<SceneError> {
    let doc = match KdlDocument::parse(src) {
        Ok(doc) => doc,
        Err(err) => {
            // Defensive path: parse_scene already succeeded in the
            // normal pipeline. If somehow we reach here with a
            // malformed file, return a single Parse error so the
            // caller surfaces SOMETHING rather than silently passing.
            let message = err.to_string();
            let at = err.diagnostics.first().and_then(|d| d.span.into());
            return vec![SceneError::Parse {
                src: NamedSource::new(path.display().to_string(), src.to_string()),
                at: at.unwrap_or_else(|| SourceSpan::new(0.into(), src.len().min(1))),
                message,
            }];
        }
    };

    let mut ctx = Ctx {
        src,
        path,
        errors: Vec::new(),
    };

    // R1: exactly one top-level `scene "<name>" { … }` node. The
    // parser-level path (`parse_scene`) enforces that for the typed
    // `SceneDoc`; we still guard here because `check_scope` can be
    // called independently in tests and tooling.
    let scenes: Vec<&KdlNode> = doc
        .nodes()
        .iter()
        .filter(|n| n.name().value() == "scene")
        .collect();

    // Any non-`scene` top-level node is unexpected — `scene` is the
    // only file-root shape admitted by R1. Report each as
    // `UnknownNode` so the user gets a typo-hint if the name is
    // close to `scene`. (`AmbiguousFileShape` / `EmptyOrUnknown`
    // from R15 handle the specific `layout {}` legacy case; that
    // shape-probe pass is separate from this scope walker.)
    for stray in doc
        .nodes()
        .iter()
        .filter(|n| n.name().value() != "scene")
    {
        ctx.emit_unknown_root_sibling(stray);
    }

    // Without a `scene` node we have nothing else to walk; the
    // file-shape probe + parse pass own that error surface.
    let Some(scene) = scenes.first() else {
        return ctx.errors;
    };

    // Any second (or later) `scene` node is a duplicate-root error.
    // Rendered as `scene/duplicate-node` with both spans — only the
    // first pair is reported to keep the diagnostic list short.
    if let Some(second) = scenes.get(1) {
        ctx.errors.push(SceneError::DuplicateNode {
            node: "scene".to_string(),
            src: ctx.named_source(),
            first: scene.name().span(),
            second: second.name().span(),
        });
    }

    // Walk the first `scene` body with the scene-root rule set.
    if let Some(body) = scene.children() {
        for node in body.nodes() {
            ctx.check_scene_root_node(node);
        }
    }

    ctx.errors
}


/// Top-3 did-you-mean candidates for a purported scene-root node
/// name. Thin convenience wrapper over [`crate::suggest::suggest_similar`]
/// so downstream tooling (`ark scene check` etc.) uses the same
/// haystack and threshold as the scope walker.
pub fn scene_root_suggestions(name: &str) -> Vec<String> {
    suggest_similar(name, SCENE_ROOT_NODES)
}

// ---------------------------------------------------------------------------
// Internal walker
// ---------------------------------------------------------------------------

/// Walker context — holds the diagnostic accumulator and source
/// material. Kept as a dedicated struct so scope-checking fns can
/// read/write the error list without threading `&mut Vec<_>` through
/// every signature.
struct Ctx<'a> {
    src: &'a str,
    path: &'a Path,
    errors: Vec<SceneError>,
}

impl<'a> Ctx<'a> {
    /// Build a fresh `NamedSource` for an error variant. Each variant
    /// consumes the source by value (miette wants owned source text
    /// per diagnostic so render impls are independent), so we rebuild
    /// the `NamedSource` at every call site.
    fn named_source(&self) -> NamedSource<String> {
        NamedSource::new(self.path.display().to_string(), self.src.to_string())
    }

    /// Emit an `UnknownNode` diagnostic for a top-level sibling of
    /// the `scene { }` block. R1 admits only a single `scene` root
    /// node, so anything else at this position is unknown.
    fn emit_unknown_root_sibling(&mut self, node: &KdlNode) {
        let name = node.name().value();
        // Only `scene` is admitted at the top level (R1), so that's
        // the entire haystack. Typos near `scene` (`sceen`, `scen`)
        // surface as hints; further-away names stay unsuggested.
        let suggestion = suggest_similar(name, &["scene"]).into_iter().next();
        self.errors.push(SceneError::UnknownNode {
            node: name.to_string(),
            suggestion,
            src: self.named_source(),
            at: node.name().span(),
        });
    }

    /// Check a single node at the scene-root level against R1 + R2.
    fn check_scene_root_node(&mut self, node: &KdlNode) {
        let name = node.name().value();

        if !SCENE_ROOT_NODES.contains(&name) {
            // Unknown root node — R1 "did you mean …?" path, backed by
            // Jaro-Winkler similarity over the admitted root-node set.
            let suggestion = suggest_similar(name, SCENE_ROOT_NODES)
                .into_iter()
                .next();
            self.errors.push(SceneError::UnknownNode {
                node: name.to_string(),
                suggestion,
                src: self.named_source(),
                at: node.name().span(),
            });
            return;
        }

        // Attribute rules applied at the scene-root position.
        self.check_node_attributes(node, name);

        // Recurse into body per node kind.
        match name {
            "layout" => self.walk_layout(node),
            "plugin" => self.walk_plugin(node),
            "on" => self.walk_on_reaction(node),
            "keybind" => self.walk_keybind(node),
            // `engine { }` has a known body shape (name / command /
            // args / env children) but those sub-nodes aren't in the
            // R2 scope table — they're validated by facet-kdl's
            // struct shape. We still walk to catch stray nodes, but
            // their parent context ("engine") is included in any
            // error.
            "engine" => self.walk_simple_parent(node, "engine", &["name", "command", "args", "env"]),
            _ => {
                // For leaf-like scene-root nodes (extends, include,
                // use, clear-reactions, clear-keybind, disable-plugin),
                // any children are misplaced.
                self.forbid_children(node, name);
            }
        }
    }

    /// Check a node's attributes (named properties) against the
    /// per-kind attribute-scope table (R2 clauses 3/5/6).
    fn check_node_attributes(&mut self, node: &KdlNode, kind: &str) {
        for entry in node.entries() {
            let Some(key_ident) = entry.name() else {
                // Positional arg — not a named attribute, skip.
                continue;
            };
            let key = key_ident.value();

            // `when=` — only on `tab` and `pane`.
            if key == "when" && !matches!(kind, "tab" | "pane") {
                self.errors.push(SceneError::MisplacedNode {
                    node: "when=".to_string(),
                    parent: kind.to_string(),
                    src: self.named_source(),
                    at: key_ident.span(),
                });
            }

            // `if=` — only on `on`.
            if key == "if" && kind != "on" {
                self.errors.push(SceneError::MisplacedNode {
                    node: "if=".to_string(),
                    parent: kind.to_string(),
                    src: self.named_source(),
                    at: key_ident.span(),
                });
            }

            // `intent=` — only on `keybind`.
            if key == "intent" && kind != "keybind" {
                self.errors.push(SceneError::MisplacedNode {
                    node: "intent=".to_string(),
                    parent: kind.to_string(),
                    src: self.named_source(),
                    at: key_ident.span(),
                });
            }
        }
    }

    /// Walk a `layout { }` block. Only `tab`, `pane`, `floating-panes`
    /// are permitted children.
    fn walk_layout(&mut self, node: &KdlNode) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            if !LAYOUT_CHILD_NODES.contains(&child_name) {
                self.errors.push(SceneError::MisplacedNode {
                    node: child_name.to_string(),
                    parent: "layout".to_string(),
                    src: self.named_source(),
                    at: child.name().span(),
                });
                continue;
            }
            self.check_node_attributes(child, child_name);
            // Recurse into tab/pane — both can host further tab/pane
            // children per zellij semantics.
            self.walk_tab_or_pane(child, child_name);
        }
    }

    /// Walk a `tab { }` or `pane { }` body. Same rule set as
    /// `LAYOUT_CHILD_NODES` at the child level.
    fn walk_tab_or_pane(&mut self, node: &KdlNode, kind: &str) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            if !TAB_OR_PANE_CHILD_NODES.contains(&child_name) {
                self.errors.push(SceneError::MisplacedNode {
                    node: child_name.to_string(),
                    parent: kind.to_string(),
                    src: self.named_source(),
                    at: child.name().span(),
                });
                continue;
            }
            self.check_node_attributes(child, child_name);
            self.walk_tab_or_pane(child, child_name);
        }
    }

    /// Walk a `plugin { }` body. Nodes outside `PLUGIN_BODY_NODES`
    /// are misplaced.
    fn walk_plugin(&mut self, node: &KdlNode) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            if !PLUGIN_BODY_NODES.contains(&child_name) {
                self.errors.push(SceneError::MisplacedNode {
                    node: child_name.to_string(),
                    parent: "plugin".to_string(),
                    src: self.named_source(),
                    at: child.name().span(),
                });
                continue;
            }
            // Plugin-body children are leaves for the R2 scope pass
            // today. Op bodies inside `on` sub-blocks (plugin
            // lifecycle marker form) are op-vocabulary territory
            // (R7) — handled in the op-typing pass (T-3.x).
            //
            // Attribute rules (`when=` / `if=` / `intent=`) still
            // apply: if a user writes `source "…" when="true"`, that
            // `when=` is misplaced under `source`.
            self.check_node_attributes(child, child_name);
        }
    }

    /// Walk an `on { }` reaction body (scene root form). Children
    /// are op nodes — their names are R7 op vocabulary, not
    /// structural scene grammar, so we do NOT validate names here
    /// (op-vocab is T-3.x). Attribute rules still apply to each op
    /// (reactions MUST NOT carry stray `when=` / `intent=` attrs on
    /// their op children).
    fn walk_on_reaction(&mut self, node: &KdlNode) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            self.check_node_attributes(child, child_name);
        }
    }

    /// Walk a `keybind { }` body (block form). Same shape as an
    /// `on { }` body — children are op nodes, scope pass only
    /// validates their attributes.
    fn walk_keybind(&mut self, node: &KdlNode) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            self.check_node_attributes(child, child_name);
        }
    }

    /// Generic walker for a node whose body has a known allow-list
    /// of child names. Used for `engine { }`.
    fn walk_simple_parent(&mut self, node: &KdlNode, parent: &str, allowed: &[&str]) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            let child_name = child.name().value();
            if !allowed.contains(&child_name) {
                self.errors.push(SceneError::MisplacedNode {
                    node: child_name.to_string(),
                    parent: parent.to_string(),
                    src: self.named_source(),
                    at: child.name().span(),
                });
                continue;
            }
            self.check_node_attributes(child, child_name);
        }
    }

    /// Report any child nodes of a leaf-shaped scene-root node
    /// (e.g. `extends "parent" { stray }`) as misplaced.
    fn forbid_children(&mut self, node: &KdlNode, parent: &str) {
        let Some(body) = node.children() else { return };
        for child in body.nodes() {
            self.errors.push(SceneError::MisplacedNode {
                node: child.name().value().to_string(),
                parent: parent.to_string(),
                src: self.named_source(),
                at: child.name().span(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("scope_test.kdl")
    }

    /// Sanity: a well-shaped scene raises zero scope errors.
    #[test]
    fn clean_scene_has_no_errors() {
        let input = r#"
scene "ok" {
    use "picker"
    layout {
        tab "work" {
            pane name="editor"
        }
    }
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
    on "AgentReady" {
        emit "demo.ready"
    }
    keybind "Alt p" intent="picker.show"
}
"#;
        let errs = check_scope(input, &p());
        assert!(errs.is_empty(), "unexpected errors: {errs:#?}");
    }

    /// R2 clause 1 — `on` at scene root is fine, but inside `layout`
    /// it's misplaced.
    #[test]
    fn on_inside_layout_is_misplaced() {
        let input = r#"
scene "x" {
    layout {
        on "AgentReady" { emit "boom" }
    }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::MisplacedNode);
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode, got {:?}", errs[0]);
        };
        assert_eq!(node, "on");
        assert_eq!(parent, "layout");
    }

    /// R2 clause 2 — `tab` at scene root is misplaced.
    #[test]
    fn tab_at_scene_root_is_unknown() {
        let input = r#"
scene "x" {
    tab "logs"
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        // `tab` isn't in SCENE_ROOT_NODES so it surfaces as an
        // unknown-node diagnostic (R1 path). The did-you-mean
        // suggester should surface a nearby candidate.
        assert_eq!(errs[0].code_enum(), ErrorCode::UnknownNode);
        let SceneError::UnknownNode { node, .. } = &errs[0] else {
            panic!("expected UnknownNode, got {:?}", errs[0]);
        };
        assert_eq!(node, "tab");
    }

    /// R2 clause 2 — `pane` at scene root is also misplaced.
    #[test]
    fn pane_at_scene_root_is_unknown() {
        let input = r#"
scene "x" {
    pane name="editor"
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::UnknownNode);
    }

    /// R2 clause 3 — `when=` on an `on` block is misplaced.
    #[test]
    fn when_attribute_on_on_is_misplaced() {
        let input = r#"
scene "x" {
    on "AgentReady" when="true" {
    }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::MisplacedNode);
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode, got {:?}", errs[0]);
        };
        assert_eq!(node, "when=");
        assert_eq!(parent, "on");
    }

    /// R2 clause 3 — `when=` is permitted on `tab` and `pane`.
    #[test]
    fn when_attribute_on_tab_and_pane_is_ok() {
        let input = r#"
scene "x" {
    layout {
        tab "work" when="agent.phase == 'ready'" {
            pane when="event.kind == 'AgentReady'" name="e"
        }
    }
}
"#;
        let errs = check_scope(input, &p());
        assert!(errs.is_empty(), "unexpected errors: {errs:#?}");
    }

    /// R2 clause 5 — `if=` on a `keybind` is misplaced.
    #[test]
    fn if_attribute_on_keybind_is_misplaced() {
        let input = r#"
scene "x" {
    keybind "Alt p" if="true" intent="picker.show"
}
"#;
        let errs = check_scope(input, &p());
        // One error for the stray `if=` on `keybind`.
        assert_eq!(errs.len(), 1, "got: {errs:#?}");
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode, got {:?}", errs[0]);
        };
        assert_eq!(node, "if=");
        assert_eq!(parent, "keybind");
    }

    /// R2 clause 5 — `if=` on an `on` block is legal.
    #[test]
    fn if_attribute_on_on_is_ok() {
        let input = r#"
scene "x" {
    on "AgentReady" if="agent.phase == 'ready'" {
    }
}
"#;
        let errs = check_scope(input, &p());
        assert!(errs.is_empty(), "unexpected errors: {errs:#?}");
    }

    /// R2 clause 6 — `intent=` on an `on` block is misplaced.
    #[test]
    fn intent_attribute_on_on_is_misplaced() {
        let input = r#"
scene "x" {
    on "AgentReady" intent="picker.show" {
    }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode");
        };
        assert_eq!(node, "intent=");
        assert_eq!(parent, "on");
    }

    /// R2 clause 4 — `source` at scene root is misplaced.
    #[test]
    fn plugin_body_node_at_scene_root_is_unknown() {
        let input = r#"
scene "x" {
    source "shipped:status"
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::UnknownNode);
    }

    /// R2 clause 4 — a bogus node inside `plugin { }` is misplaced.
    #[test]
    fn bogus_node_inside_plugin_is_misplaced() {
        let input = r#"
scene "x" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
        keybind "Alt s" intent="x"
    }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode");
        };
        assert_eq!(node, "keybind");
        assert_eq!(parent, "plugin");
    }

    /// R2 clause 4 — `on` inside `plugin { }` is legal (lifecycle
    /// marker form per R6).
    #[test]
    fn on_inside_plugin_is_ok_as_lifecycle_marker() {
        let input = r#"
scene "x" {
    plugin "status" {
        source "shipped:status"
        mount "floating"
        on "UserEvent:show-status"
    }
}
"#;
        let errs = check_scope(input, &p());
        assert!(errs.is_empty(), "unexpected errors: {errs:#?}");
    }

    /// R1 — unknown scene-root node emits `scene/unknown-node`.
    #[test]
    fn unknown_scene_root_node_emits_unknown_node() {
        let input = r#"
scene "x" {
    reaction "AgentReady" { }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::UnknownNode { node, .. } = &errs[0] else {
            panic!("expected UnknownNode");
        };
        assert_eq!(node, "reaction");
    }

    /// T-1.3 wiring: a typo close to an existing scene-root node
    /// surfaces as a concrete did-you-mean suggestion.
    #[test]
    fn unknown_scene_root_typo_yields_suggestion() {
        let input = r#"
scene "x" {
    keybnd "Alt p" intent="x"
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::UnknownNode { node, suggestion, .. } = &errs[0] else {
            panic!("expected UnknownNode");
        };
        assert_eq!(node, "keybnd");
        assert_eq!(suggestion.as_deref(), Some("keybind"));
    }

    /// T-1.3 wiring: a typo on the top-level `scene` node name
    /// yields a suggestion pointing at `scene`.
    #[test]
    fn stray_top_level_typo_suggests_scene() {
        let input = r#"
sceen "x" { }
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::UnknownNode { node, suggestion, .. } = &errs[0] else {
            panic!("expected UnknownNode");
        };
        assert_eq!(node, "sceen");
        assert_eq!(suggestion.as_deref(), Some("scene"));
    }

    /// T-1.3 wiring: distant unknown node names do not produce a
    /// spurious suggestion (threshold-gated).
    #[test]
    fn distant_unknown_node_has_no_suggestion() {
        let input = r#"
scene "x" {
    xyzzy "?"
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::UnknownNode { suggestion, .. } = &errs[0] else {
            panic!("expected UnknownNode");
        };
        assert!(suggestion.is_none(), "got: {suggestion:?}");
    }

    /// T-1.3 wiring: exported helper covers both the happy path and
    /// the no-match case so downstream tooling can share the same
    /// surface.
    #[test]
    fn scene_root_suggestions_helper_is_exposed() {
        let hits = scene_root_suggestions("keybnd");
        assert!(!hits.is_empty());
        assert_eq!(hits[0], "keybind");
        let misses = scene_root_suggestions("xyzzy");
        assert!(misses.is_empty(), "got: {misses:?}");
    }

    /// Leaf-shape root nodes (`extends`, `include`, `use`, …) MUST
    /// NOT carry a body. Stray child nodes are misplaced.
    #[test]
    fn leaf_root_node_with_body_flags_children() {
        let input = r#"
scene "x" {
    extends "base" {
        stray "thing"
    }
}
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        let SceneError::MisplacedNode { node, parent, .. } = &errs[0] else {
            panic!("expected MisplacedNode");
        };
        assert_eq!(node, "stray");
        assert_eq!(parent, "extends");
    }

    /// Stray top-level node (outside `scene { }`) is reported
    /// separately from misplaced-node — R1 only permits one `scene`
    /// at top level.
    #[test]
    fn top_level_non_scene_node_is_unknown() {
        let input = r#"
not-a-scene "hello" { }
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::UnknownNode);
    }

    /// Multiple `scene` top-level nodes fire `scene/duplicate-node`
    /// on the second.
    #[test]
    fn duplicate_top_level_scene_is_duplicate_node() {
        let input = r#"
scene "a" { }
scene "b" { }
"#;
        let errs = check_scope(input, &p());
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code_enum(), ErrorCode::DuplicateNode);
    }

    /// Multiple errors surface in one pass — scope walker should not
    /// bail on the first failure.
    #[test]
    fn multiple_violations_all_reported() {
        let input = r#"
scene "x" {
    tab "logs"
    on "AgentReady" when="true" intent="picker.show" {
    }
}
"#;
        let errs = check_scope(input, &p());
        // 1× tab at root (UnknownNode), 2× attribute misplaced on
        // `on` (when= + intent=).
        assert_eq!(errs.len(), 3, "got: {errs:#?}");
    }

}
