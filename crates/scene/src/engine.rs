//! Engine lowering: typed `EngineNode` (AST) → [`EngineLaunch`] (runtime spec).
//!
//! The scene AST in [`crate::ast::EngineNode`] models an
//! `engine { name "…"; command "…"; args …; env { … } }` block per
//! `cavekit-scene.md` R17. This module promotes that source-fidelity
//! shape into the **lowered** [`EngineLaunch`] form the supervisor
//! consumes when spawning the ACP-agent process:
//!
//! * `command` becomes `argv[0]`.
//! * Every `args "v1" "v2" …` line is flattened into `argv[1..]` in
//!   source order. Multiple `args` lines are concatenated.
//! * `env { KEY "VAL"; … }` becomes the `env` map. Duplicate keys are
//!   resolved last-wins to mirror process-environment semantics.
//!
//! ## Scope-rule TODO
//!
//! R17 caps a scene at one `engine { }` block. The AST does NOT
//! enforce that today (it's modeled as `Option<EngineNode>` on
//! [`crate::ast::SceneNode`], so the typed parser already rejects
//! duplicates — but cross-file `extends`/`include` merges can
//! re-introduce a second engine). The dedicated cross-file rule lives
//! in [`crate::scope`] / the merge pipeline; T-ACP.4 (or the
//! follow-on) wires the
//! [`crate::error::SceneError::EngineConflict`] surface into
//! `scope.rs`. For now this module assumes a single
//! [`crate::ast::EngineNode`] per scene as a precondition documented
//! on [`lower_engine`].
//
// TODO(T-ACP.4): emit `scene/engine-conflict` from the merge pass when
// extends/include/use combine to produce more than one engine block.

use std::collections::BTreeMap;

use crate::ast::EngineNode;

/// Lowered, runtime-ready ACP engine launch spec.
///
/// Built by [`lower_engine`] from a typed [`EngineNode`]. The
/// supervisor crate consumes this directly when spawning the agent
/// process — see `crates/supervisor` for the producer side of the
/// integration once T-ACP.4 wires it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineLaunch {
    /// Human-friendly engine identifier (e.g. `"claude"`). May be
    /// empty when the scene omitted the `name` child — the supervisor
    /// substitutes the binary name in that case.
    pub name: String,

    /// Argv-0 of the engine process — the executable. Empty when the
    /// scene omitted `command`; the supervisor surfaces a
    /// `scene/grammar` error in that case (R17 makes `command`
    /// effectively required).
    pub command: String,

    /// Argv-1.. — additional positional arguments, flattened from
    /// every `args "<v>" …` line in source order.
    pub args: Vec<String>,

    /// Environment variables to inject at spawn. `BTreeMap` so the
    /// rendered order is deterministic across runs (helpful for
    /// snapshot tests and for `ark scene graph` output). Last-wins on
    /// duplicate keys (mirrors `setenv` semantics).
    pub env: BTreeMap<String, String>,
}

/// Lower a typed [`EngineNode`] (AST) into a runtime-ready
/// [`EngineLaunch`].
///
/// Total — never errors. Missing children (`name`, `command`)
/// surface as empty strings on the lowered struct; whether that's a
/// hard error or a soft default is the supervisor's call (see R17
/// acceptance criterion: `command` is effectively required).
///
/// Precondition: the caller has already enforced the
/// "at-most-one-engine-per-scene" rule (R17). The AST naturally
/// admits at most one because [`crate::ast::SceneNode::engine`] is
/// `Option<EngineNode>`, but cross-file merges can still synthesise a
/// conflict — see the module-level TODO for the dedicated scope rule.
pub fn lower_engine(node: &EngineNode) -> EngineLaunch {
    let name = node
        .name
        .as_ref()
        .map(|n| n.value.clone())
        .unwrap_or_default();

    let command = node
        .command
        .as_ref()
        .map(|c| c.value.clone())
        .unwrap_or_default();

    // Flatten every `args "v1" "v2" …` line in source order.
    let args: Vec<String> = node
        .args
        .iter()
        .flat_map(|line| line.values.iter().cloned())
        .collect();

    // Last-wins on duplicate keys, matching `setenv` semantics.
    let env: BTreeMap<String, String> = node
        .env
        .as_ref()
        .map(|e| {
            e.vars
                .iter()
                .map(|v| (v.key.clone(), v.value.clone()))
                .collect()
        })
        .unwrap_or_default();

    EngineLaunch {
        name,
        command,
        args,
        env,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("test.kdl")
    }

    fn engine_from(src: &str) -> EngineNode {
        let doc = parse_scene(src, &p()).expect("parse fixture");
        doc.scene.engine.expect("fixture must declare an engine")
    }

    #[test]
    fn full_engine_block_lowers_round_trip() {
        let src = r#"
scene "s" {
    engine {
        name "claude"
        command "claude-bin"
        args "--acp" "--verbose"
        env {
            ANTHROPIC_API_KEY "secret"
            DEBUG "1"
        }
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(launched.name, "claude");
        assert_eq!(launched.command, "claude-bin");
        assert_eq!(launched.args, vec!["--acp", "--verbose"]);
        assert_eq!(
            launched.env.get("ANTHROPIC_API_KEY"),
            Some(&"secret".to_string())
        );
        assert_eq!(launched.env.get("DEBUG"), Some(&"1".to_string()));
        assert_eq!(launched.env.len(), 2);
    }

    #[test]
    fn minimal_engine_block_lowers_with_defaults() {
        // Just `name` and `command` — no args, no env.
        let src = r#"
scene "s" {
    engine {
        name "claude"
        command "claude"
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(launched.name, "claude");
        assert_eq!(launched.command, "claude");
        assert!(launched.args.is_empty());
        assert!(launched.env.is_empty());
    }

    #[test]
    fn multiple_args_lines_concatenate_in_source_order() {
        let src = r#"
scene "s" {
    engine {
        command "claude"
        args "--acp"
        args "--mcp" "config.json"
        args "--verbose"
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(
            launched.args,
            vec!["--acp", "--mcp", "config.json", "--verbose"]
        );
    }

    #[test]
    fn empty_engine_block_lowers_to_empty_launch() {
        let src = r#"
scene "s" {
    engine {
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(launched.name, "");
        assert_eq!(launched.command, "");
        assert!(launched.args.is_empty());
        assert!(launched.env.is_empty());
    }

    #[test]
    fn env_arbitrary_key_shapes_round_trip() {
        // Keys with mixed case, kebab, and underscored forms — KDL
        // bareword identifiers admit all three; node_name capture
        // preserves them verbatim.
        let src = r#"
scene "s" {
    engine {
        command "claude"
        env {
            UPPER_SNAKE "1"
            kebab-case "2"
            mixedCase "3"
        }
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(launched.env.get("UPPER_SNAKE"), Some(&"1".to_string()));
        assert_eq!(launched.env.get("kebab-case"), Some(&"2".to_string()));
        assert_eq!(launched.env.get("mixedCase"), Some(&"3".to_string()));
    }

    /// Probe the facet-kdl routing for `args` to confirm whether the
    /// parser matches by `args` (literal field name) or `arg`
    /// (singularized form). Documenting the behaviour pins it down so
    /// future facet-kdl upgrades don't silently flip the grammar.
    #[test]
    fn args_routing_probe_records_actual_behaviour() {
        let src_plural = r#"
scene "s" {
    engine {
        command "claude"
        args "--acp"
    }
}
"#;
        let src_singular = r#"
scene "s" {
    engine {
        command "claude"
        arg "--acp"
    }
}
"#;
        let plural = parse_scene(src_plural, &p());
        let singular = parse_scene(src_singular, &p());

        // Whichever shape the parser accepts, the args slot must
        // populate. The other shape is permitted to either succeed
        // (ignored) or fail — what matters is that AT LEAST ONE of
        // the spec-aligned forms lands in `args`.
        let plural_count = plural
            .as_ref()
            .ok()
            .and_then(|d| d.scene.engine.as_ref().map(|e| e.args.len()))
            .unwrap_or(0);
        let singular_count = singular
            .as_ref()
            .ok()
            .and_then(|d| d.scene.engine.as_ref().map(|e| e.args.len()))
            .unwrap_or(0);

        assert!(
            plural_count > 0 || singular_count > 0,
            "neither `args` nor `arg` routed into engine.args — facet-kdl regression?"
        );
        // Pin the documented behaviour: the spec form (`args` —
        // matching how the user writes it in their scene file) MUST
        // populate the field. Singular `arg` may or may not also work
        // depending on facet-kdl's matching rules; we don't pin that
        // since R17 only documents `args`.
        assert!(
            plural_count > 0,
            "the spec form `args \"…\"` did not populate engine.args; \
             facet-kdl's child-routing changed and the field name needs \
             a rename or grammar adjustment"
        );
    }

    #[test]
    fn duplicate_env_keys_are_last_wins() {
        // Mirrors process-environment `setenv` semantics: the second
        // declaration overwrites the first. A future scope-pass may
        // also surface a soft warning, but the lowering step itself
        // is total + deterministic.
        let src = r#"
scene "s" {
    engine {
        command "claude"
        env {
            FOO "first"
            FOO "second"
        }
    }
}
"#;
        let engine = engine_from(src);
        let launched = lower_engine(&engine);
        assert_eq!(launched.env.get("FOO"), Some(&"second".to_string()));
        assert_eq!(launched.env.len(), 1);
    }
}
