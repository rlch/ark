//! Hook compatibility shim (v2 → v3).
//!
//! The v2 supervisor imported `HookEntry`, `build_hook_registry()`, and
//! `extend_registry_with_hooks()` from `ark-scene-v2-archive::hook_compat`.
//! Hooks were TOML-style `{event, command}` pairs that mapped to scene
//! reactions at runtime.
//!
//! V3 represents reactions as [`OnNode`] AST nodes registered in a
//! [`ReactionRegistry`]. This module bridges the gap: TOML hook entries
//! are converted to `OnNode` reactions with `exec script="<command>"`
//! ops, then inserted into the registry.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ast::ops::{ExecOp, OpNode};
use crate::ast::selector::{EventSelector, FieldPattern, MatchType};
use crate::ast::OnNode;
use crate::reactions::{
    Entry, EventKind, OriginKind, ReactionOrigin, ReactionRegistry,
};

/// A legacy TOML-style hook entry: event name + shell command.
///
/// The supervisor reads these from the user's TOML config and passes
/// them here for conversion into scene reactions.
#[derive(Debug, Clone)]
pub struct HookEntry {
    /// Event kind or name to match (e.g. `"Error"`,
    /// `"ext:myext.something"`).
    pub event: String,
    /// Shell command to execute when the event fires.
    pub command: String,
}

/// Convert legacy TOML hooks into v3 [`OnNode`] reactions.
///
/// Each hook becomes an `on "<event>" { exec script="<command>" }`
/// reaction node. The returned `OnNode` values can be fed into
/// [`extend_registry_with_hooks`] or consumed directly by callers that
/// build their own reaction pipeline.
pub fn hooks_to_on_nodes(hooks: &[HookEntry]) -> Vec<OnNode> {
    hooks.iter().map(hook_to_on_node).collect()
}

/// Convert a single [`HookEntry`] into an [`OnNode`].
fn hook_to_on_node(hook: &HookEntry) -> OnNode {
    // Parse the event string. If it contains a colon (e.g.
    // `"ext:myext.something"`) the part before the colon is the kind
    // and the part after is the `name=` field pattern. Otherwise the
    // whole string is the event kind.
    let (kind, name_pattern) = match hook.event.split_once(':') {
        Some((k, n)) => (k.to_string(), Some(n.to_string())),
        None => (hook.event.clone(), None),
    };

    let mut field_patterns = BTreeMap::new();
    if let Some(name) = &name_pattern {
        field_patterns.insert(
            "name".to_string(),
            FieldPattern {
                raw: name.clone(),
                match_type: MatchType::Exact,
            },
        );
    }

    let selector = EventSelector {
        kind,
        field_patterns,
    };

    OnNode {
        selector: Some(selector),
        when: None,
        ops: vec![OpNode::Exec(ExecOp {
            script: hook.command.clone(),
            shell: None,
            timeout_ms: None,
            when: None,
        })],
    }
}

/// Insert hook-derived reactions into an existing [`ReactionRegistry`].
///
/// Each hook is converted to an [`OnNode`], then registered under the
/// appropriate [`EventKind`]. Hooks whose event kind is unrecognised are
/// silently skipped (the supervisor's config validator should catch
/// those earlier).
pub fn extend_registry_with_hooks(
    registry: &mut ReactionRegistry,
    hooks: &[HookEntry],
) {
    for hook in hooks {
        let on = hook_to_on_node(hook);
        let Some(selector) = on.selector.clone() else {
            continue;
        };
        let Some(kind) = EventKind::parse(&selector.kind) else {
            // Unknown event kind — skip rather than error.
            continue;
        };
        let ext_name = if kind == EventKind::Ext {
            selector
                .field_patterns
                .get("name")
                .filter(|fp| fp.match_type == MatchType::Exact)
                .map(|fp| fp.raw.clone())
        } else {
            None
        };
        let entry = Entry {
            selector,
            predicate: None,
            ops: on.ops,
            origin: ReactionOrigin {
                file: PathBuf::from("<hooks>"),
                line: None,
                kind: OriginKind::UserScene,
            },
        };
        registry.insert(kind, ext_name, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_to_on_node_simple_event() {
        let hook = HookEntry {
            event: "Error".into(),
            command: "echo error".into(),
        };
        let on = hook_to_on_node(&hook);
        let sel = on.selector.as_ref().unwrap();
        assert_eq!(sel.kind, "Error");
        assert!(sel.field_patterns.is_empty());
        assert_eq!(on.ops.len(), 1);
        match &on.ops[0] {
            OpNode::Exec(exec) => assert_eq!(exec.script, "echo error"),
            other => panic!("expected Exec op, got: {other:?}"),
        }
    }

    #[test]
    fn hook_to_on_node_ext_with_name() {
        let hook = HookEntry {
            event: "ext:myext.custom".into(),
            command: "notify-send hello".into(),
        };
        let on = hook_to_on_node(&hook);
        let sel = on.selector.as_ref().unwrap();
        assert_eq!(sel.kind, "ext");
        let name_fp = sel.field_patterns.get("name").unwrap();
        assert_eq!(name_fp.raw, "myext.custom");
        assert_eq!(name_fp.match_type, MatchType::Exact);
    }

    #[test]
    fn hooks_to_on_nodes_batch() {
        let hooks = vec![
            HookEntry {
                event: "Log".into(),
                command: "echo log".into(),
            },
            HookEntry {
                event: "Error".into(),
                command: "echo error".into(),
            },
        ];
        let nodes = hooks_to_on_nodes(&hooks);
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn extend_registry_with_known_hooks() {
        let mut reg = ReactionRegistry::new();
        let hooks = vec![
            HookEntry {
                event: "Error".into(),
                command: "echo error".into(),
            },
            HookEntry {
                event: "ext:myext.custom".into(),
                command: "echo custom".into(),
            },
        ];
        extend_registry_with_hooks(&mut reg, &hooks);

        assert_eq!(reg.by_kind(EventKind::Error).len(), 1);
        assert_eq!(reg.by_kind(EventKind::Ext).len(), 1);
        assert_eq!(reg.by_ext_name("myext.custom").len(), 1);
    }

    #[test]
    fn extend_registry_skips_unknown_event_kind() {
        let mut reg = ReactionRegistry::new();
        let hooks = vec![HookEntry {
            event: "BogusKind".into(),
            command: "echo boom".into(),
        }];
        extend_registry_with_hooks(&mut reg, &hooks);
        // Unknown kind silently skipped — registry stays empty.
        assert!(reg.is_empty());
    }

    #[test]
    fn extend_registry_preserves_existing_entries() {
        let mut reg = ReactionRegistry::new();
        // Pre-populate with a manual entry.
        let sel = EventSelector {
            kind: "Error".into(),
            field_patterns: BTreeMap::new(),
        };
        let entry = Entry {
            selector: sel,
            predicate: None,
            ops: vec![],
            origin: ReactionOrigin::user_scene(PathBuf::from("scene.kdl")),
        };
        reg.insert(EventKind::Error, None, entry);

        let hooks = vec![HookEntry {
            event: "Error".into(),
            command: "echo error".into(),
        }];
        extend_registry_with_hooks(&mut reg, &hooks);

        // Both the original and the hook-derived entry should be present.
        assert_eq!(reg.by_kind(EventKind::Error).len(), 2);
    }
}
