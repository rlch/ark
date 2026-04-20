//! Structural parser for the `plugins { }` block in `ark.kdl`.
//!
//! T-PP-003 (cavekit-plugin-protocol R5): GRAMMAR ONLY. Semantic
//! validation (closed cap-name set, URL scheme gate, Levenshtein
//! suggestions for unknown caps, …) lives in T-PP-037. This module
//! only walks KDL nodes into a plain-data Rust struct — callers are
//! responsible for field-level validation.
//!
//! Parse shape:
//!
//! ```kdl
//! plugins {
//!     claude-code location="file:./plugins/claude-code.wasm" {
//!         capabilities {
//!             fs-read
//!             fs-write
//!             spawn-process
//!         }
//!         runtime {
//!             update-failure-budget 16
//!             render-budget-ms 16
//!         }
//!     }
//! }
//! ```
//!
//! Both `capabilities { }` and `runtime { }` are optional. An omitted
//! capabilities block = empty grant set. Duplicate keys WITHIN a
//! single plugin's runtime/capabilities block are preserved as-is
//! (caller decides whether to error); duplicate plugin names at the
//! top-level `plugins { }` raise `PluginsKdlError::DuplicatePluginName`
//! because KDL itself permits them and the kit treats them as a hard
//! error (R5 acceptance "Duplicate names = plugin-name-clash").

use std::collections::BTreeMap;

/// A single plugin entry inside `plugins { }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Bare identifier — NOT yet validated against the `[a-z][a-z0-9-]*`
    /// regex (that check lives in T-PP-037).
    pub name: String,
    /// Value of the required `location=` property. Left as a raw
    /// string here — scheme gating (`file:` only in v1) happens in
    /// T-PP-037.
    pub location: Option<String>,
    /// Every bare-identifier child of `capabilities { }`, in source
    /// order. Validation against the closed cap set lives in T-PP-037.
    pub capabilities: Vec<String>,
    /// Every `key value` pair under `runtime { }`. Values preserved as
    /// raw KDL-text form (stringified) — typed coercion happens in
    /// T-PP-037.
    pub runtime: BTreeMap<String, String>,
}

/// Parsed result of the top-level `plugins { }` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginsBlock {
    pub plugins: Vec<PluginEntry>,
}

/// Errors raised during structural parsing of `plugins { }`.
///
/// Strictly parse-level — field semantics (cap name validation, URL
/// scheme, …) are T-PP-037's responsibility. Duplicate plugin names
/// surface here because they are a structural property of the block
/// shape.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginsKdlError {
    /// The KDL document itself failed to parse.
    #[error("KDL parse error: {0}")]
    Kdl(#[from] kdl::KdlError),

    /// Two `<plugin-name>` entries under a single `plugins { }` block.
    ///
    /// Stable code prefix matches `PluginLoadError::DuplicatePluginName`
    /// in `ark-plugin-protocol`.
    #[error("error[ark-kdl/duplicate-plugin-name]: plugins block declares {name} twice")]
    DuplicatePluginName { name: String },
}

/// Parse a full `ark.kdl` document string, extracting just the
/// top-level `plugins { }` block if present.
///
/// Returns `Ok(None)` when there is no `plugins` node at all (the
/// common case for configs that don't use plugins yet). Any other
/// top-level nodes are ignored.
pub fn parse_plugins_block(src: &str) -> Result<Option<PluginsBlock>, PluginsKdlError> {
    let doc: kdl::KdlDocument = src.parse()?;
    let Some(node) = doc.nodes().iter().find(|n| n.name().value() == "plugins") else {
        return Ok(None);
    };
    let mut block = PluginsBlock::default();
    let children = match node.children() {
        Some(c) => c,
        None => return Ok(Some(block)), // `plugins` node with no body = empty block
    };
    let mut seen = std::collections::BTreeSet::new();
    for plugin_node in children.nodes() {
        let name = plugin_node.name().value().to_string();
        if !seen.insert(name.clone()) {
            return Err(PluginsKdlError::DuplicatePluginName { name });
        }
        block.plugins.push(parse_plugin_node(plugin_node, name));
    }
    Ok(Some(block))
}

fn parse_plugin_node(node: &kdl::KdlNode, name: String) -> PluginEntry {
    // Extract `location=` property if present. KDL entries carry
    // either positional args or named properties; `location=...` is a
    // named property.
    let location = node
        .entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some("location"))
        .and_then(|e| entry_value_to_string(e));

    let mut capabilities = Vec::new();
    let mut runtime = BTreeMap::new();

    if let Some(children) = node.children() {
        for child in children.nodes() {
            match child.name().value() {
                "capabilities" => {
                    if let Some(cap_children) = child.children() {
                        for cap_node in cap_children.nodes() {
                            capabilities.push(cap_node.name().value().to_string());
                        }
                    }
                }
                "runtime" => {
                    if let Some(rt_children) = child.children() {
                        for rt_node in rt_children.nodes() {
                            let key = rt_node.name().value().to_string();
                            let value = rt_node
                                .entries()
                                .iter()
                                .find(|e| e.name().is_none())
                                .and_then(|e| entry_value_to_string(e))
                                .unwrap_or_default();
                            runtime.insert(key, value);
                        }
                    }
                }
                _ => {
                    // Unknown children are ignored at grammar level —
                    // T-PP-037 may warn on them but grammar stage is
                    // permissive.
                }
            }
        }
    }

    PluginEntry {
        name,
        location,
        capabilities,
        runtime,
    }
}

fn entry_value_to_string(entry: &kdl::KdlEntry) -> Option<String> {
    let v = entry.value();
    if let Some(s) = v.as_string() {
        return Some(s.to_string());
    }
    if let Some(i) = v.as_integer() {
        return Some(i.to_string());
    }
    if let Some(f) = v.as_float() {
        return Some(f.to_string());
    }
    if let Some(b) = v.as_bool() {
        return Some(b.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_plugins_block_returns_none() {
        let src = r#"
            other-block {
                foo
            }
        "#;
        let got = parse_plugins_block(src).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn empty_plugins_block_parses_to_empty_vec() {
        let src = r#"
            plugins {
            }
        "#;
        let got = parse_plugins_block(src).unwrap().unwrap();
        assert!(got.plugins.is_empty());
    }

    #[test]
    fn single_plugin_with_caps_and_runtime_parses() {
        let src = r#"
            plugins {
                claude-code location="file:./plugins/claude-code.wasm" {
                    capabilities {
                        fs-read
                        fs-write
                        spawn-process
                    }
                    runtime {
                        update-failure-budget 16
                        render-budget-ms 16
                    }
                }
            }
        "#;
        let got = parse_plugins_block(src).unwrap().unwrap();
        assert_eq!(got.plugins.len(), 1);
        let p = &got.plugins[0];
        assert_eq!(p.name, "claude-code");
        assert_eq!(
            p.location.as_deref(),
            Some("file:./plugins/claude-code.wasm")
        );
        assert_eq!(
            p.capabilities,
            vec![
                "fs-read".to_string(),
                "fs-write".to_string(),
                "spawn-process".to_string()
            ]
        );
        assert_eq!(
            p.runtime.get("update-failure-budget").map(String::as_str),
            Some("16")
        );
        assert_eq!(
            p.runtime.get("render-budget-ms").map(String::as_str),
            Some("16")
        );
    }

    #[test]
    fn plugin_without_capabilities_block_is_empty_grants() {
        let src = r#"
            plugins {
                quiet location="file:./q.wasm" {
                }
            }
        "#;
        let got = parse_plugins_block(src).unwrap().unwrap();
        assert_eq!(got.plugins.len(), 1);
        assert!(got.plugins[0].capabilities.is_empty());
    }

    #[test]
    fn plugin_without_body_is_empty_everything() {
        // Plugin node with NO body at all — `foo location=...`
        let src = r#"
            plugins {
                minimal location="file:./m.wasm"
            }
        "#;
        let got = parse_plugins_block(src).unwrap().unwrap();
        assert_eq!(got.plugins.len(), 1);
        let p = &got.plugins[0];
        assert_eq!(p.name, "minimal");
        assert_eq!(p.location.as_deref(), Some("file:./m.wasm"));
        assert!(p.capabilities.is_empty());
        assert!(p.runtime.is_empty());
    }

    #[test]
    fn duplicate_plugin_name_errors() {
        let src = r#"
            plugins {
                dup location="file:./a.wasm"
                dup location="file:./b.wasm"
            }
        "#;
        let err = parse_plugins_block(src).unwrap_err();
        match err {
            PluginsKdlError::DuplicatePluginName { name } => assert_eq!(name, "dup"),
            other => panic!("expected DuplicatePluginName, got {other:?}"),
        }
    }

    #[test]
    fn multiple_distinct_plugins_parse_in_order() {
        let src = r#"
            plugins {
                alpha location="file:./a.wasm"
                beta location="file:./b.wasm" {
                    capabilities {
                        network
                    }
                }
            }
        "#;
        let got = parse_plugins_block(src).unwrap().unwrap();
        assert_eq!(got.plugins.len(), 2);
        assert_eq!(got.plugins[0].name, "alpha");
        assert_eq!(got.plugins[1].name, "beta");
        assert_eq!(got.plugins[1].capabilities, vec!["network".to_string()]);
    }

    #[test]
    fn malformed_kdl_surfaces_parse_error() {
        // missing closing brace
        let src = r#"
            plugins {
                foo location="x"
        "#;
        let err = parse_plugins_block(src).unwrap_err();
        match err {
            PluginsKdlError::Kdl(_) => {}
            other => panic!("expected Kdl parse error, got {other:?}"),
        }
    }
}
