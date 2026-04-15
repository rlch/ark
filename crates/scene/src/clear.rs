//! `clear-reactions` / `clear-keybind` / `disable-plugin` directive
//! application (T-9.3, R11).
//!
//! **Evaluation order** (from R11): after parent + included fragments
//! are merged into the accumulator, but BEFORE the root's own
//! reactions / keybinds / plugins land on top. That ordering matches
//! the intuitive "inherit but drop this piece" the user expresses when
//! they write `extends "base"` + `clear-keybind "Alt p"`.
//!
//! **Parent-scoped clears cannot drop descendants' contributions** —
//! parents don't know about their descendants, so a parent's
//! `clear-*` directive in isolation is a silent noop unless the
//! matched contribution was already in the accumulator when the
//! parent was applied. This falls out naturally from the ordering
//! above: each fragment merges its clears in the context its
//! contributions actually see.
//!
//! v1 matches **literally**: `clear-reactions selector="<sel>"`
//! compares the reaction's selector string verbatim; `clear-keybind
//! "<chord>"` compares the chord string; `disable-plugin "<name>"`
//! compares the plugin name. Glob / regex matching is deferred — the
//! selector grammar already supports literal strings end-to-end and
//! that covers the composition use cases we've seen. See the TODO in
//! [`matches_selector`] for the extension point.

use std::collections::HashMap;

use crate::ast::{ClearKeybindNode, ClearReactionsNode, DisablePluginNode, PaneNode};
use crate::merge::ComposedScene;

/// Apply a root fragment's `clear-*` directives to the accumulated
/// composition state.
///
/// Invoked by [`crate::merge::merge_fragments`] at the moment the
/// root fragment is about to land. Mutates:
///
/// * `acc.reactions` — drops any reaction whose selector matches one
///   of `clear_reactions`.
/// * `acc.keybinds` + `keybind_index` — drops the keybind whose
///   chord matches one of `clear_keybinds`, refreshing the index to
///   keep subsequent last-wins lookups stable.
/// * `acc.plugins` — drops any plugin whose name matches one of
///   `disable_plugins`.
///
/// Silent noop when no target matches. Callers don't receive errors
/// from this pass — R11 spec explicitly wants "drop if present,
/// otherwise silent" so scene authors can inherit, see a change
/// upstream, then have their existing `clear-*` directive continue
/// to work without editing.
pub fn apply_clears(
    acc: &mut ComposedScene,
    clear_reactions: &[ClearReactionsNode],
    clear_keybinds: &[ClearKeybindNode],
    disable_plugins: &[DisablePluginNode],
    keybind_index: &mut HashMap<String, usize>,
) {
    // Reactions: drop by selector match.
    if !clear_reactions.is_empty() {
        acc.reactions.retain(|r| {
            !clear_reactions
                .iter()
                .any(|c| matches_selector(&c.selector, &r.selector))
        });
    }

    // Keybinds: drop by chord match, then rebuild the index.
    if !clear_keybinds.is_empty() {
        acc.keybinds
            .retain(|kb| !clear_keybinds.iter().any(|c| c.chord == kb.chord));
        keybind_index.clear();
        for (idx, kb) in acc.keybinds.iter().enumerate() {
            keybind_index.insert(kb.chord.clone(), idx);
        }
    }

    // Plugins: drop by name match.
    if !disable_plugins.is_empty() {
        acc.plugins
            .retain(|p| !disable_plugins.iter().any(|c| c.name == p.name));
    }
}

/// Literal-string match for `clear-reactions selector="<sel>"`.
///
/// Compares the clear directive's selector verbatim against the
/// reaction's selector string. v1 does not expand the comparison to
/// glob / regex — see module docs.
///
// TODO(T-9.x / v0.3+): consider glob / regex expansion. The selector
// grammar already supports literal strings end-to-end; a future tier
// can extend this function to delegate to `globset` for
// `clear-reactions` entries that opt in via a `glob=true` attribute.
fn matches_selector(clear_selector: &str, reaction_selector: &str) -> bool {
    clear_selector == reaction_selector
}

/// Clone a [`PaneNode`] — the AST type doesn't derive Clone, so
/// [`crate::merge::merge_fragments`] + [`crate::compile`] share this
/// helper. Deep-recursive: nested panes are cloned recursively.
pub(crate) fn clone_pane_node(src: &PaneNode) -> PaneNode {
    PaneNode {
        when: src.when.clone(),
        name: src.name.clone(),
        command: src.command.clone(),
        size: src.size.clone(),
        split_direction: src.split_direction.clone(),
        focus: src.focus,
        cwd: src.cwd.clone(),
        panes: src.panes.iter().map(clone_pane_node).collect(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{KeybindNode, OnNode, PluginNode};

    fn reaction(selector: &str) -> OnNode {
        OnNode {
            selector: selector.to_string(),
            if_: None,
            ops: vec![],
        }
    }

    fn keybind(chord: &str) -> KeybindNode {
        KeybindNode {
            chord: chord.to_string(),
            intent: None,
            ops: vec![],
        }
    }

    fn plugin(name: &str) -> PluginNode {
        PluginNode {
            name: name.to_string(),
            override_: None,
            source: None,
            mount: None,
            summon: None,
            dismiss: None,
            on: vec![],
            subscribes: vec![],
            config: None,
        }
    }

    /// `clear-reactions selector="X"` drops the matching reaction,
    /// leaves others intact.
    #[test]
    fn clear_reactions_drops_matching_selector() {
        let mut acc = ComposedScene::default();
        acc.reactions.push(reaction("AgentReady"));
        acc.reactions.push(reaction("Stall"));

        let clears = vec![ClearReactionsNode {
            selector: "AgentReady".to_string(),
        }];
        let mut idx = HashMap::new();
        apply_clears(&mut acc, &clears, &[], &[], &mut idx);
        assert_eq!(acc.reactions.len(), 1);
        assert_eq!(acc.reactions[0].selector, "Stall");
    }

    /// `clear-keybind "X"` drops the matching keybind and refreshes
    /// the index.
    #[test]
    fn clear_keybind_drops_by_chord() {
        let mut acc = ComposedScene::default();
        acc.keybinds.push(keybind("Alt p"));
        acc.keybinds.push(keybind("Ctrl s"));
        let mut idx = HashMap::new();
        idx.insert("Alt p".to_string(), 0);
        idx.insert("Ctrl s".to_string(), 1);

        let clears = vec![ClearKeybindNode {
            chord: "Alt p".to_string(),
        }];
        apply_clears(&mut acc, &[], &clears, &[], &mut idx);
        assert_eq!(acc.keybinds.len(), 1);
        assert_eq!(acc.keybinds[0].chord, "Ctrl s");
        assert_eq!(idx.get("Alt p"), None);
        assert_eq!(idx.get("Ctrl s"), Some(&0));
    }

    /// `disable-plugin "X"` drops the matching plugin silently.
    #[test]
    fn disable_plugin_drops_by_name() {
        let mut acc = ComposedScene::default();
        acc.plugins.push(plugin("picker"));
        acc.plugins.push(plugin("status"));

        let disables = vec![DisablePluginNode {
            name: "picker".to_string(),
        }];
        let mut idx = HashMap::new();
        apply_clears(&mut acc, &[], &[], &disables, &mut idx);
        assert_eq!(acc.plugins.len(), 1);
        assert_eq!(acc.plugins[0].name, "status");
    }

    /// Clears targeting items that aren't present are a silent
    /// noop (R11 spec).
    #[test]
    fn clears_without_match_are_silent_noop() {
        let mut acc = ComposedScene::default();
        acc.reactions.push(reaction("AgentReady"));
        acc.keybinds.push(keybind("Alt p"));
        acc.plugins.push(plugin("status"));

        let mut idx = HashMap::new();
        idx.insert("Alt p".to_string(), 0);

        apply_clears(
            &mut acc,
            &[ClearReactionsNode {
                selector: "NoSuchEvent".to_string(),
            }],
            &[ClearKeybindNode {
                chord: "Ctrl q".to_string(),
            }],
            &[DisablePluginNode {
                name: "no-such-plugin".to_string(),
            }],
            &mut idx,
        );
        assert_eq!(acc.reactions.len(), 1);
        assert_eq!(acc.keybinds.len(), 1);
        assert_eq!(acc.plugins.len(), 1);
    }

    /// Multiple clear directives in one pass all apply.
    #[test]
    fn multiple_clears_combine() {
        let mut acc = ComposedScene::default();
        acc.reactions.push(reaction("AgentReady"));
        acc.reactions.push(reaction("Stall"));
        acc.reactions.push(reaction("UserEvent:a"));

        let clears = vec![
            ClearReactionsNode {
                selector: "AgentReady".to_string(),
            },
            ClearReactionsNode {
                selector: "UserEvent:a".to_string(),
            },
        ];
        let mut idx = HashMap::new();
        apply_clears(&mut acc, &clears, &[], &[], &mut idx);
        assert_eq!(acc.reactions.len(), 1);
        assert_eq!(acc.reactions[0].selector, "Stall");
    }
}

// ---------------------------------------------------------------------------
// Integration tests: drive `apply_clears` through the real composition
// pipeline (`load_composition` + `merge_fragments`). These verify the
// "apply clears between parent merge and root additions" ordering
// holds end-to-end, not just the in-isolation mutation API above.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod composition_tests {
    use crate::extends::SceneSearchCtx;
    use crate::merge::{load_composition, merge_fragments};
    use crate::parse::parse_scene;
    use std::fs;
    use tempfile::TempDir;

    /// Parent contributes two reactions; child clears one of them.
    /// After merge, only the unmatched parent reaction survives.
    #[test]
    fn child_clear_reactions_drops_inherited_match() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    on "AgentReady" {
        exec
    }
    on "Stall" {
        emit
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    clear-reactions selector="AgentReady"
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags =
            load_composition(doc, child_path, &SceneSearchCtx::new(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.reactions.len(), 1);
        assert_eq!(merged.reactions[0].selector, "Stall");
    }

    /// Parent contributes a keybind; child clears it then adds its own
    /// on the same chord. After merge the child's keybind wins (there
    /// is only one) — exercises clear-then-re-add.
    #[test]
    fn child_clear_keybind_then_re_add() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    keybind "Alt p" intent="base.picker"
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    clear-keybind "Alt p"
    keybind "Alt p" intent="child.picker"
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags =
            load_composition(doc, child_path, &SceneSearchCtx::new(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.keybinds.len(), 1);
        assert_eq!(
            merged.keybinds[0].intent.as_deref(),
            Some("child.picker")
        );
    }

    /// Parent contributes a plugin; child disables it. After merge the
    /// plugin is absent.
    #[test]
    fn child_disable_plugin_drops_inherited() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    disable-plugin "picker"
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags =
            load_composition(doc, child_path, &SceneSearchCtx::new(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.plugins.len(), 1);
        assert_eq!(merged.plugins[0].name, "status");
    }

    /// Grandparent-scoped clear cannot see a descendant's contribution
    /// — parent has no knowledge of child, so a parent's clear
    /// directive targeting a selector only the child adds is a silent
    /// noop (R11 spec). Here: base clears `child.only` before child
    /// declares it; the child's contribution is applied AFTER parent
    /// merge, so the final set contains it.
    #[test]
    fn parent_clear_cannot_drop_descendant_contribution() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    clear-reactions selector="UserEvent:child.only"
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    on "UserEvent:child.only" {
        emit
    }
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags =
            load_composition(doc, child_path, &SceneSearchCtx::new(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge");
        assert_eq!(merged.reactions.len(), 1);
        assert_eq!(merged.reactions[0].selector, "UserEvent:child.only");
    }

    /// Clear for a non-existent target is a silent noop — no error,
    /// merge continues unchanged.
    #[test]
    fn root_clear_non_existent_target_is_silent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"
scene "base" {
    on "AgentReady" { exec }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        let src = r#"
scene "child" {
    extends "base"
    clear-reactions selector="NoSuchEvent"
    clear-keybind "F99"
    disable-plugin "ghost"
}
"#;
        fs::write(&child_path, src).unwrap();
        let doc = parse_scene(src, &child_path).unwrap();
        let frags =
            load_composition(doc, child_path, &SceneSearchCtx::new(&root)).unwrap();
        let merged = merge_fragments(frags).expect("merge, noop clears");
        assert_eq!(merged.reactions.len(), 1);
    }
}
