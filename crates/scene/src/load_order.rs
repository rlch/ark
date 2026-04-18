//! Load-order enforcement for composed scene bodies (T-079 / R11).
//!
//! After [`crate::compose::compose_scene`] has resolved all `include`
//! directives and spliced fragment body nodes in source order, this
//! module applies the **merge semantics** that determine the final set of
//! active nodes:
//!
//! - **Reactions (`on`)**: additive in load order — later reactions do not
//!   replace earlier ones; all are kept.
//! - **Keybinds (`bind`)**: last-wins per chord — a later `bind` for the
//!   same chord string replaces the earlier one.
//! - **`clear-reactions`**: removes matching reactions from preceding
//!   entries (by selector equality).
//! - **`clear-bind`**: removes a keybind for the named chord from
//!   preceding entries.
//! - **`disable-extension`**: records the extension name as inactive.
//! - **Layouts / modes / uses**: collected in source order.
//!
//! The load order is: extensions (topo-sorted, future T-075) then
//! includes (source order) then user scene (last). Since
//! `compose_scene` already produces a flat `Vec<SceneBodyNode>` in the
//! correct source order, this module walks that vec once and applies
//! the merge rules.

use crate::ast::{
    BindNode, ClearBindNode, ClearReactionsNode, LayoutNode, ModeNode, OnNode, SceneBodyNode,
    UseNode,
};

/// Result of applying load-order merge semantics to a composed scene body.
///
/// Each field holds the surviving nodes after `clear-reactions`,
/// `clear-bind`, and last-wins deduplication have been applied.
#[derive(Debug, Clone, Default)]
pub struct LoadOrderResult {
    /// Layout nodes in source order.
    pub layouts: Vec<LayoutNode>,

    /// Mode nodes in source order.
    pub modes: Vec<ModeNode>,

    /// Reaction (`on`) nodes — additive, in load order. Entries removed
    /// by preceding `clear-reactions` directives are absent.
    pub reactions: Vec<OnNode>,

    /// Keybind (`bind`) nodes — last-wins per chord. Only the final
    /// `bind` for each chord string survives.
    pub binds: Vec<BindNode>,

    /// Active extension (`use`) nodes in source order.
    pub uses: Vec<UseNode>,

    /// Extension names disabled via `disable-extension`.
    pub disabled_extensions: Vec<String>,
}

/// Apply load-order merge semantics to a composed (flat) scene body.
///
/// The input `body` must already have been through
/// [`crate::compose::compose_scene`] — all `include` directives resolved
/// and spliced. This function walks the body once in order and
/// accumulates the [`LoadOrderResult`].
pub fn enforce_load_order(body: &[SceneBodyNode]) -> LoadOrderResult {
    let mut result = LoadOrderResult::default();

    // Intermediate tracking for last-wins bind dedup. We store
    // (index_in_binds_vec, BindNode) keyed by chord string. On a
    // duplicate chord we overwrite the earlier entry in-place.
    //
    // Using a Vec + HashMap approach: binds vec holds all bind nodes in
    // insertion order; the map tracks the latest index per chord so we
    // can replace.
    let mut bind_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for node in body {
        match node {
            SceneBodyNode::Use(u) => {
                result.uses.push(u.clone());
            }

            SceneBodyNode::Include(_) => {
                // ext: includes that survived compose are preserved as-is
                // in the body; they don't contribute to load-order output
                // (extension resolution is a later pass).
            }

            SceneBodyNode::Layout(l) => {
                result.layouts.push(l.clone());
            }

            SceneBodyNode::Mode(m) => {
                result.modes.push(m.clone());
            }

            SceneBodyNode::On(on) => {
                result.reactions.push(on.clone());
            }

            SceneBodyNode::Bind(b) => {
                if let Some(&existing_idx) = bind_index.get(&b.chord) {
                    // Last-wins: replace the earlier bind for this chord.
                    result.binds[existing_idx] = b.clone();
                } else {
                    let idx = result.binds.len();
                    bind_index.insert(b.chord.clone(), idx);
                    result.binds.push(b.clone());
                }
            }

            SceneBodyNode::ClearReactions(cr) => {
                apply_clear_reactions(&mut result.reactions, cr);
            }

            SceneBodyNode::ClearBind(cb) => {
                apply_clear_bind(&mut result.binds, &mut bind_index, cb);
            }

            SceneBodyNode::DisableExtension(de) => {
                if !result.disabled_extensions.contains(&de.name) {
                    result.disabled_extensions.push(de.name.clone());
                }
            }
        }
    }

    result
}

/// Remove reactions whose selector text matches the `clear-reactions`
/// directive. Matching uses the same literal comparison as
/// [`crate::reactions::clear_selector_matches`]: the clear selector's
/// event string is compared against each reaction's selector. Since
/// `ClearReactionsNode` carries a raw `event="<selector>"` string, we
/// parse it into kind + field patterns for comparison.
///
/// For now we use a simplified approach: the `event` property on
/// `clear-reactions` is a bare event-kind string (e.g.
/// `clear-reactions event="FileEdited"`). Field-level clear matching
/// (e.g. `clear-reactions event="FileEdited path=**/*.md"`) is a future
/// refinement.
fn apply_clear_reactions(reactions: &mut Vec<OnNode>, cr: &ClearReactionsNode) {
    // Parse the clear selector: the `event` property may be just a kind
    // name, or "Kind field=pat …". For now we split on whitespace and
    // treat the first token as the kind.
    let selector_text = cr.selector.trim();
    let kind_token = selector_text.split_whitespace().next().unwrap_or("");

    reactions.retain(|on| {
        let on_kind = on.selector.as_ref().map(|s| s.kind.as_str()).unwrap_or("");
        // If the clear selector specifies only the kind with no field
        // patterns, it removes ALL reactions of that kind.
        // If it specifies field patterns, we compare them too.
        if !kinds_match(kind_token, on_kind) {
            return true; // different kind, keep
        }
        // Parse field patterns from the clear selector (everything after
        // the kind token).
        let field_part = selector_text.strip_prefix(kind_token).unwrap_or("").trim();
        if field_part.is_empty() {
            // No field constraints — remove all reactions of this kind.
            return false;
        }
        // Compare field patterns: parse "field=value" pairs from the
        // clear selector and check if the reaction's selector has
        // matching patterns.
        let Some(on_sel) = &on.selector else {
            return true; // no selector on the reaction, keep it
        };
        let clear_fields = parse_clear_field_patterns(field_part);
        for (field, value) in &clear_fields {
            match on_sel.field_patterns.get(field.as_str()) {
                Some(fp) if fp.raw == *value => {}
                _ => return true, // field mismatch, keep
            }
        }
        // All clear-selector fields matched — remove this reaction.
        false
    });
}

/// Parse `"field=value field2=value2"` from a clear-reactions selector
/// string into (field, value) pairs.
fn parse_clear_field_patterns(field_part: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for token in field_part.split_whitespace() {
        if let Some((field, value)) = token.split_once('=') {
            // Strip surrounding quotes from value if present.
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            out.push((field.to_string(), value.to_string()));
        }
    }
    out
}

/// Case-insensitive kind comparison that accepts both PascalCase and
/// snake_case spellings (e.g. `"FileEdited"` matches `"FileEdited"`,
/// `"file_edited"` matches `"file_edited"`).
///
/// Normalises by lowercasing and stripping underscores so
/// `"FileEdited"` == `"file_edited"`.
fn kinds_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let norm = |s: &str| -> String { s.to_lowercase().replace('_', "") };
    norm(a) == norm(b)
}

/// Remove a keybind for the given chord, updating the index.
fn apply_clear_bind(
    binds: &mut Vec<BindNode>,
    bind_index: &mut std::collections::HashMap<String, usize>,
    cb: &ClearBindNode,
) {
    if let Some(&idx) = bind_index.get(&cb.chord) {
        binds.remove(idx);
        bind_index.remove(&cb.chord);
        // Rebuild indices for entries after the removed position.
        for (_, v) in bind_index.iter_mut() {
            if *v > idx {
                *v -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::DisableExtensionNode;
    use crate::ast::ops::OpNode;
    use crate::ast::selector::{EventSelector, FieldPattern, MatchType};
    use std::collections::BTreeMap;

    // --- helpers ---

    fn on_node(kind: &str) -> OnNode {
        on_node_with_fields(kind, BTreeMap::new())
    }

    fn on_node_with_fields(kind: &str, field_patterns: BTreeMap<String, FieldPattern>) -> OnNode {
        OnNode {
            selector: Some(EventSelector {
                kind: kind.to_string(),
                field_patterns,
            }),
            when: None,
            ops: Vec::new(),
        }
    }

    fn bind_node(chord: &str) -> BindNode {
        BindNode {
            chord: chord.to_string(),
            ops: Vec::new(),
        }
    }

    fn bind_node_with_ops(chord: &str, ops: Vec<OpNode>) -> BindNode {
        BindNode {
            chord: chord.to_string(),
            ops,
        }
    }

    fn layout_node() -> LayoutNode {
        LayoutNode { tabs: Vec::new() }
    }

    fn mode_node(name: &str) -> ModeNode {
        ModeNode {
            name: name.to_string(),
            tabs: Vec::new(),
        }
    }

    fn use_node(name: &str) -> UseNode {
        UseNode {
            name: name.to_string(),
            config_block: None,
        }
    }

    // --- reactions are additive ---

    #[test]
    fn reactions_are_additive_in_load_order() {
        let body = vec![
            SceneBodyNode::On(on_node("FileEdited")),
            SceneBodyNode::On(on_node("FileEdited")),
            SceneBodyNode::On(on_node("Error")),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.reactions.len(), 3);
        assert_eq!(
            result.reactions[0].selector.as_ref().unwrap().kind,
            "FileEdited"
        );
        assert_eq!(
            result.reactions[1].selector.as_ref().unwrap().kind,
            "FileEdited"
        );
        assert_eq!(result.reactions[2].selector.as_ref().unwrap().kind, "Error");
    }

    // --- binds are last-wins per chord ---

    #[test]
    fn binds_last_wins_per_chord() {
        let body = vec![
            SceneBodyNode::Bind(bind_node("Alt d")),
            SceneBodyNode::Bind(bind_node("Alt d")),
            SceneBodyNode::Bind(bind_node("Ctrl c")),
        ];
        let result = enforce_load_order(&body);
        // Two unique chords.
        assert_eq!(result.binds.len(), 2);
        assert_eq!(result.binds[0].chord, "Alt d");
        assert_eq!(result.binds[1].chord, "Ctrl c");
    }

    #[test]
    fn bind_last_wins_replaces_earlier() {
        // The second "Alt d" bind should replace the first.
        let first_ops = vec![OpNode::Close(crate::ast::ops::CloseOp {
            handle: "@p1".to_string(),
            when: None,
        })];
        let second_ops = vec![OpNode::Close(crate::ast::ops::CloseOp {
            handle: "@p2".to_string(),
            when: None,
        })];
        let body = vec![
            SceneBodyNode::Bind(bind_node_with_ops("Alt d", first_ops)),
            SceneBodyNode::Bind(bind_node_with_ops("Alt d", second_ops)),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.binds.len(), 1);
        assert_eq!(result.binds[0].chord, "Alt d");
        // The surviving bind should have the second set of ops.
        match &result.binds[0].ops[0] {
            OpNode::Close(c) => assert_eq!(c.handle, "@p2"),
            other => panic!("expected Close op, got {other:?}"),
        }
    }

    // --- clear-reactions ---

    #[test]
    fn clear_reactions_removes_matching_kind() {
        let body = vec![
            SceneBodyNode::On(on_node("FileEdited")),
            SceneBodyNode::On(on_node("Error")),
            SceneBodyNode::ClearReactions(ClearReactionsNode {
                selector: "FileEdited".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.reactions.len(), 1);
        assert_eq!(result.reactions[0].selector.as_ref().unwrap().kind, "Error");
    }

    #[test]
    fn clear_reactions_with_field_pattern_is_selective() {
        let mut fps_md = BTreeMap::new();
        fps_md.insert(
            "path".to_string(),
            FieldPattern {
                raw: "**/*.md".to_string(),
                match_type: MatchType::Glob,
            },
        );
        let mut fps_rs = BTreeMap::new();
        fps_rs.insert(
            "path".to_string(),
            FieldPattern {
                raw: "**/*.rs".to_string(),
                match_type: MatchType::Glob,
            },
        );
        let body = vec![
            SceneBodyNode::On(on_node_with_fields("FileEdited", fps_md)),
            SceneBodyNode::On(on_node_with_fields("FileEdited", fps_rs)),
            SceneBodyNode::ClearReactions(ClearReactionsNode {
                selector: "FileEdited path=**/*.md".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.reactions.len(), 1);
        assert_eq!(
            result.reactions[0]
                .selector
                .as_ref()
                .unwrap()
                .field_patterns
                .get("path")
                .unwrap()
                .raw,
            "**/*.rs"
        );
    }

    #[test]
    fn clear_reactions_only_affects_preceding() {
        // clear-reactions appears BEFORE the reaction — the reaction
        // should survive because it comes after the clear.
        let body = vec![
            SceneBodyNode::ClearReactions(ClearReactionsNode {
                selector: "FileEdited".to_string(),
            }),
            SceneBodyNode::On(on_node("FileEdited")),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.reactions.len(), 1);
    }

    // --- clear-bind ---

    #[test]
    fn clear_bind_removes_matching_chord() {
        let body = vec![
            SceneBodyNode::Bind(bind_node("Alt d")),
            SceneBodyNode::Bind(bind_node("Ctrl c")),
            SceneBodyNode::ClearBind(ClearBindNode {
                chord: "Alt d".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.binds.len(), 1);
        assert_eq!(result.binds[0].chord, "Ctrl c");
    }

    #[test]
    fn clear_bind_no_match_is_noop() {
        let body = vec![
            SceneBodyNode::Bind(bind_node("Alt d")),
            SceneBodyNode::ClearBind(ClearBindNode {
                chord: "Alt x".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.binds.len(), 1);
        assert_eq!(result.binds[0].chord, "Alt d");
    }

    // --- disable-extension ---

    #[test]
    fn disable_extension_records_name() {
        let body = vec![
            SceneBodyNode::Use(use_node("git-status")),
            SceneBodyNode::DisableExtension(DisableExtensionNode {
                name: "git-status".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.uses.len(), 1);
        assert_eq!(result.disabled_extensions, vec!["git-status"]);
    }

    #[test]
    fn disable_extension_deduplicates() {
        let body = vec![
            SceneBodyNode::DisableExtension(DisableExtensionNode {
                name: "x".to_string(),
            }),
            SceneBodyNode::DisableExtension(DisableExtensionNode {
                name: "x".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.disabled_extensions.len(), 1);
    }

    // --- layouts and modes ---

    #[test]
    fn layouts_collected_in_order() {
        let body = vec![
            SceneBodyNode::Layout(layout_node()),
            SceneBodyNode::Layout(layout_node()),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.layouts.len(), 2);
    }

    #[test]
    fn modes_collected_in_order() {
        let body = vec![
            SceneBodyNode::Mode(mode_node("debug")),
            SceneBodyNode::Mode(mode_node("review")),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.modes.len(), 2);
        assert_eq!(result.modes[0].name, "debug");
        assert_eq!(result.modes[1].name, "review");
    }

    // --- uses ---

    #[test]
    fn uses_collected_in_order() {
        let body = vec![
            SceneBodyNode::Use(use_node("git-status")),
            SceneBodyNode::Use(use_node("claude-engine")),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.uses.len(), 2);
        assert_eq!(result.uses[0].name, "git-status");
        assert_eq!(result.uses[1].name, "claude-engine");
    }

    // --- empty body ---

    #[test]
    fn empty_body_returns_empty_result() {
        let result = enforce_load_order(&[]);
        assert!(result.layouts.is_empty());
        assert!(result.modes.is_empty());
        assert!(result.reactions.is_empty());
        assert!(result.binds.is_empty());
        assert!(result.uses.is_empty());
        assert!(result.disabled_extensions.is_empty());
    }

    // --- mixed scenario ---

    #[test]
    fn mixed_body_applies_all_rules() {
        let body = vec![
            // From included fragment:
            SceneBodyNode::Use(use_node("git-status")),
            SceneBodyNode::Layout(layout_node()),
            SceneBodyNode::On(on_node("FileEdited")),
            SceneBodyNode::Bind(bind_node("Alt d")),
            // From user scene:
            SceneBodyNode::On(on_node("Error")),
            SceneBodyNode::Bind(bind_node("Alt d")), // overrides above
            SceneBodyNode::Bind(bind_node("Ctrl c")),
            SceneBodyNode::ClearReactions(ClearReactionsNode {
                selector: "FileEdited".to_string(),
            }),
            SceneBodyNode::DisableExtension(DisableExtensionNode {
                name: "git-status".to_string(),
            }),
            SceneBodyNode::Mode(mode_node("debug")),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.uses.len(), 1);
        assert_eq!(result.layouts.len(), 1);
        // FileEdited was cleared, only Error remains.
        assert_eq!(result.reactions.len(), 1);
        assert_eq!(result.reactions[0].selector.as_ref().unwrap().kind, "Error");
        // Alt d last-wins + Ctrl c.
        assert_eq!(result.binds.len(), 2);
        assert_eq!(result.disabled_extensions, vec!["git-status"]);
        assert_eq!(result.modes.len(), 1);
    }

    // --- cross-case clear-reactions ---

    #[test]
    fn clear_reactions_matches_across_case() {
        let body = vec![
            SceneBodyNode::On(on_node("FileEdited")),
            SceneBodyNode::ClearReactions(ClearReactionsNode {
                selector: "file_edited".to_string(),
            }),
        ];
        let result = enforce_load_order(&body);
        assert_eq!(result.reactions.len(), 0);
    }
}
