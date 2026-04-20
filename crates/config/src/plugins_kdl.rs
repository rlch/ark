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

use std::collections::{BTreeMap, BTreeSet};

use crate::url_gate::{PluginUrl, UrlGateError, parse_plugin_url};

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

    /// Semantic validation (T-PP-037..T-PP-039) failed after the
    /// grammar parse succeeded. See [`PluginsSemanticError`].
    #[error(transparent)]
    Semantic(#[from] PluginsSemanticError),
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

// ===========================================================================
// T-PP-037..T-PP-039: semantic validation layer
// ===========================================================================
//
// Builds on the structural parser above. `parse_plugins_block_semantic`
// walks the grammar output and applies:
//
//   1. Plugin name regex `^[a-z][a-z0-9-]*$` (R5).
//   2. Required `location=` property → URL scheme gate (R12) via
//      `url_gate::parse_plugin_url`.
//   3. Closed capability name set `["fs-read", "fs-write", "network",
//      "spawn-process", "bus-send", "bus-receive"]` (R5 / R2). Unknown
//      caps produce Levenshtein-1 "did you mean …?" suggestions.
//   4. `runtime { update-failure-budget=N; render-budget-ms=M }` with
//      defaults 16 / 16. Unknown `runtime` keys → error with
//      suggestions.
//
// The semantic pass deliberately does NOT check that the `file:` path
// exists on disk — that lives in ark-host's loader phase (R12 acceptance
// "Missing or unreadable file at startup = error[plugin/location-
// unreachable]"), after the config-level parse succeeds.

/// Closed cap-name set accepted in `plugins.<name>.capabilities { }`.
///
/// Matches the `ark:cap/*` interface set defined in R2 (see
/// cavekit-plugin-protocol.md). Adding a new cap requires a kit
/// revision + updating this constant in lockstep with
/// `ark-plugin-protocol`'s WIT world.
pub const KNOWN_CAPABILITIES: &[&str] = &[
    "fs-read",
    "fs-write",
    "network",
    "spawn-process",
    "bus-send",
    "bus-receive",
];

/// Runtime-block keys recognised by [`RuntimeConfig`]. Unknown keys
/// raise [`PluginsKdlError::UnknownRuntimeKey`] with Levenshtein
/// suggestions against this set.
pub const KNOWN_RUNTIME_KEYS: &[&str] = &["update-failure-budget", "render-budget-ms"];

/// Default `runtime.update-failure-budget` when the KDL omits the key
/// (or the entire `runtime { }` block).  Matches R5 acceptance.
pub const DEFAULT_UPDATE_FAILURE_BUDGET: u32 = 16;

/// Default `runtime.render-budget-ms` when the KDL omits the key
/// (or the entire `runtime { }` block).  Matches R5 acceptance.
pub const DEFAULT_RENDER_BUDGET_MS: u32 = 16;

/// Per-plugin runtime tuning pulled from `runtime { … }` under a
/// plugin entry. Both fields come with kit-mandated defaults; an
/// omitted block = defaults on every field, not "no runtime at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Max consecutive `update()` failures before the host marks the
    /// plugin unresponsive. R7 acceptance — default 16.
    pub update_failure_budget: u32,
    /// Soft per-frame render budget in milliseconds. R10 acceptance —
    /// default 16.
    pub render_budget_ms: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            update_failure_budget: DEFAULT_UPDATE_FAILURE_BUDGET,
            render_budget_ms: DEFAULT_RENDER_BUDGET_MS,
        }
    }
}

/// Fully-validated plugin entry — grammar-plus-semantics.
///
/// Produced by [`parse_plugins_block_semantic`]. Only entries that
/// survive all of {name regex, URL gate, cap-set membership, runtime
/// key validation} end up here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticPluginEntry {
    /// Validated plugin name. Matches `^[a-z][a-z0-9-]*$`.
    pub name: String,
    /// Parsed + scheme-gated plugin location URL (R12).
    pub location: PluginUrl,
    /// Granted capability set — membership of [`KNOWN_CAPABILITIES`].
    /// Deduplicated via the `BTreeSet` shape.
    pub capabilities: BTreeSet<String>,
    /// Runtime knobs, defaults applied where KDL omitted the key.
    pub runtime: RuntimeConfig,
}

/// Top-level semantic parse output. Empty `plugins { }` block yields
/// `Some(SemanticPluginsBlock { plugins: vec![] })`; a config with no
/// `plugins` node at all yields `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticPluginsBlock {
    pub plugins: Vec<SemanticPluginEntry>,
}

/// Semantic parse errors (grammar errors re-enter via
/// [`PluginsKdlError::Kdl`] / [`PluginsKdlError::DuplicatePluginName`]).
///
/// Each variant's `Display` message begins with a stable
/// `error[ark-kdl/<slug>]:` prefix so users can grep the code against
/// their terminal output.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PluginsSemanticError {
    /// Plugin name failed the `^[a-z][a-z0-9-]*$` regex.
    #[error("error[ark-kdl/invalid-plugin-name]: plugin name {name:?} must match [a-z][a-z0-9-]*")]
    InvalidPluginName { name: String },

    /// `location=` property missing from a plugin node.
    #[error("error[ark-kdl/missing-location]: plugin {plugin:?} must declare location=\"...\"")]
    MissingLocation { plugin: String },

    /// URL scheme gate rejected the `location=` value.
    #[error("{source}")]
    InvalidLocation {
        plugin: String,
        #[source]
        source: UrlGateError,
    },

    /// Capability name not in [`KNOWN_CAPABILITIES`].
    ///
    /// `suggestions` contains every closed-set cap within edit
    /// distance 1 of the unknown identifier, sorted alphabetically.
    #[error(
        "error[ark-kdl/unknown-capability]: capability {cap:?} not in ark-cap/* set{}",
        fmt_suggestions(suggestions)
    )]
    UnknownCapability {
        cap: String,
        suggestions: Vec<String>,
    },

    /// Key inside a `runtime { }` block isn't in [`KNOWN_RUNTIME_KEYS`].
    #[error(
        "error[ark-kdl/unknown-runtime-key]: runtime key {key:?} not recognised{}",
        fmt_suggestions(suggestions)
    )]
    UnknownRuntimeKey {
        key: String,
        suggestions: Vec<String>,
    },

    /// Runtime key value wouldn't parse as `u32`.
    #[error(
        "error[ark-kdl/invalid-runtime-value]: runtime.{key}={value:?} must be a non-negative integer"
    )]
    InvalidRuntimeValue { key: String, value: String },
}

fn fmt_suggestions(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        String::new()
    } else {
        format!(" (did you mean: {})", suggestions.join(", "))
    }
}

/// Top-level entry point for the semantic layer.
///
/// Returns `Ok(None)` when the KDL document has no `plugins { }` node.
/// Any single failing plugin entry fails the whole block with the
/// first error encountered in document order.
pub fn parse_plugins_block_semantic(
    src: &str,
) -> Result<Option<SemanticPluginsBlock>, PluginsKdlError> {
    let Some(raw) = parse_plugins_block(src)? else {
        return Ok(None);
    };
    let mut out = SemanticPluginsBlock::default();
    for entry in raw.plugins {
        out.plugins.push(validate_entry(entry)?);
    }
    Ok(Some(out))
}

fn validate_entry(raw: PluginEntry) -> Result<SemanticPluginEntry, PluginsSemanticError> {
    if !is_valid_plugin_name(&raw.name) {
        return Err(PluginsSemanticError::InvalidPluginName { name: raw.name });
    }

    let location_raw = raw
        .location
        .ok_or_else(|| PluginsSemanticError::MissingLocation {
            plugin: raw.name.clone(),
        })?;
    let location = parse_plugin_url(&location_raw).map_err(|source| {
        PluginsSemanticError::InvalidLocation {
            plugin: raw.name.clone(),
            source,
        }
    })?;

    let mut capabilities = BTreeSet::new();
    for cap in raw.capabilities {
        if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
            let suggestions = nearby(&cap, KNOWN_CAPABILITIES);
            return Err(PluginsSemanticError::UnknownCapability {
                cap,
                suggestions,
            });
        }
        capabilities.insert(cap);
    }

    let runtime = validate_runtime(raw.runtime)?;

    Ok(SemanticPluginEntry {
        name: raw.name,
        location,
        capabilities,
        runtime,
    })
}

fn validate_runtime(raw: BTreeMap<String, String>) -> Result<RuntimeConfig, PluginsSemanticError> {
    let mut cfg = RuntimeConfig::default();
    for (key, value) in raw {
        match key.as_str() {
            "update-failure-budget" => {
                cfg.update_failure_budget = value
                    .parse::<u32>()
                    .map_err(|_| PluginsSemanticError::InvalidRuntimeValue {
                        key: key.clone(),
                        value: value.clone(),
                    })?;
            }
            "render-budget-ms" => {
                cfg.render_budget_ms = value
                    .parse::<u32>()
                    .map_err(|_| PluginsSemanticError::InvalidRuntimeValue {
                        key: key.clone(),
                        value: value.clone(),
                    })?;
            }
            other => {
                let suggestions = nearby(other, KNOWN_RUNTIME_KEYS);
                return Err(PluginsSemanticError::UnknownRuntimeKey {
                    key: other.to_string(),
                    suggestions,
                });
            }
        }
    }
    Ok(cfg)
}

/// Plugin-name regex match. No `regex` crate — the expression is simple
/// enough that a hand-rolled ASCII scan is cheaper than pulling in a
/// full regex engine for this one check.
fn is_valid_plugin_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Find every entry in `pool` within Damerau-Levenshtein edit distance
/// ≤ 1 of `needle`. Deterministic sort by source-order-in-pool to keep
/// the diagnostic stable across runs.
fn nearby(needle: &str, pool: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for candidate in pool {
        if strsim::damerau_levenshtein(needle, candidate) <= 1 {
            out.push(candidate.to_string());
        }
    }
    out
}

// `PluginsKdlError::Semantic` uses `#[from]` so the structural parser
// and semantic validator can compose into a single error type via `?`.

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

    // =======================================================================
    // T-PP-037..T-PP-039 semantic-layer tests.
    // =======================================================================

    #[test]
    fn semantic_absent_plugins_block_returns_none() {
        let src = r#"other-block { foo }"#;
        assert!(parse_plugins_block_semantic(src).unwrap().is_none());
    }

    #[test]
    fn semantic_valid_plugin_entry_parses_fully() {
        let src = r#"
            plugins {
                claude-code location="https://example.com/p.wasm" {
                    capabilities {
                        fs-read
                        fs-write
                        spawn-process
                    }
                    runtime {
                        update-failure-budget 32
                        render-budget-ms 8
                    }
                }
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        assert_eq!(got.plugins.len(), 1);
        let p = &got.plugins[0];
        assert_eq!(p.name, "claude-code");
        assert_eq!(p.location.scheme(), crate::url_gate::UrlScheme::Https);
        let caps: Vec<&str> = p.capabilities.iter().map(String::as_str).collect();
        assert_eq!(caps, vec!["fs-read", "fs-write", "spawn-process"]);
        assert_eq!(p.runtime.update_failure_budget, 32);
        assert_eq!(p.runtime.render_budget_ms, 8);
    }

    #[test]
    fn semantic_defaults_runtime_when_block_absent() {
        let src = r#"
            plugins {
                quiet location="https://example.com/q.wasm"
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        assert_eq!(got.plugins[0].runtime, RuntimeConfig::default());
        assert_eq!(
            got.plugins[0].runtime.update_failure_budget,
            DEFAULT_UPDATE_FAILURE_BUDGET
        );
        assert_eq!(
            got.plugins[0].runtime.render_budget_ms,
            DEFAULT_RENDER_BUDGET_MS
        );
    }

    #[test]
    fn semantic_partial_runtime_keeps_defaults_for_missing_keys() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    runtime {
                        render-budget-ms 42
                    }
                }
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        assert_eq!(got.plugins[0].runtime.render_budget_ms, 42);
        assert_eq!(
            got.plugins[0].runtime.update_failure_budget,
            DEFAULT_UPDATE_FAILURE_BUDGET
        );
    }

    #[test]
    fn semantic_unknown_runtime_key_errors_with_suggestion() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    runtime {
                        render-budget-mz 16
                    }
                }
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::UnknownRuntimeKey {
                key,
                suggestions,
            }) => {
                assert_eq!(key, "render-budget-mz");
                assert!(
                    suggestions.contains(&"render-budget-ms".to_string()),
                    "expected edit-distance-1 suggestion; got {suggestions:?}"
                );
            }
            other => panic!("expected UnknownRuntimeKey, got {other:?}"),
        }
    }

    #[test]
    fn semantic_empty_capabilities_block_is_empty_grant() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    capabilities {
                    }
                }
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        assert!(got.plugins[0].capabilities.is_empty());
    }

    #[test]
    fn semantic_omitted_capabilities_block_is_empty_grant() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm"
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        assert!(got.plugins[0].capabilities.is_empty());
    }

    #[test]
    fn semantic_unknown_capability_suggests_closest() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    capabilities {
                        fs-reed
                    }
                }
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::UnknownCapability {
                cap,
                suggestions,
            }) => {
                assert_eq!(cap, "fs-reed");
                assert!(
                    suggestions.contains(&"fs-read".to_string()),
                    "expected fs-read suggestion; got {suggestions:?}"
                );
            }
            other => panic!("expected UnknownCapability, got {other:?}"),
        }
    }

    #[test]
    fn semantic_unknown_capability_far_has_no_suggestions() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    capabilities {
                        quantum-entanglement
                    }
                }
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::UnknownCapability {
                cap,
                suggestions,
            }) => {
                assert_eq!(cap, "quantum-entanglement");
                assert!(suggestions.is_empty(), "unexpected suggestion: {suggestions:?}");
            }
            other => panic!("expected UnknownCapability, got {other:?}"),
        }
    }

    #[test]
    fn semantic_invalid_plugin_name_errors() {
        let src = r#"
            plugins {
                Bad_Name location="https://example.com/p.wasm"
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::InvalidPluginName { name }) => {
                assert_eq!(name, "Bad_Name");
            }
            other => panic!("expected InvalidPluginName, got {other:?}"),
        }
    }

    #[test]
    fn semantic_underscore_in_plugin_name_refused() {
        let src = r#"
            plugins {
                snake_case location="https://example.com/p.wasm"
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::InvalidPluginName { name }) => {
                assert_eq!(name, "snake_case");
            }
            other => panic!("expected InvalidPluginName, got {other:?}"),
        }
    }

    #[test]
    fn semantic_missing_location_errors() {
        let src = r#"
            plugins {
                p {
                    capabilities {
                        fs-read
                    }
                }
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::MissingLocation { plugin }) => {
                assert_eq!(plugin, "p");
            }
            other => panic!("expected MissingLocation, got {other:?}"),
        }
    }

    #[test]
    fn semantic_http_location_explicitly_refused() {
        let src = r#"
            plugins {
                p location="http://example.com/p.wasm"
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::InvalidLocation {
                plugin,
                source,
            }) => {
                assert_eq!(plugin, "p");
                assert!(matches!(source, UrlGateError::HttpNotAllowed));
            }
            other => panic!("expected InvalidLocation(HttpNotAllowed), got {other:?}"),
        }
    }

    #[test]
    fn semantic_duplicate_plugin_name_errors_from_grammar_layer() {
        // Duplicate-name check lives in the structural parser, but the
        // semantic entry-point must still surface it (it composes via
        // `parse_plugins_block`).
        let src = r#"
            plugins {
                dup location="https://example.com/a.wasm"
                dup location="https://example.com/b.wasm"
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        assert!(matches!(
            err,
            PluginsKdlError::DuplicatePluginName { .. }
        ));
    }

    #[test]
    fn semantic_capability_set_is_deduplicated() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    capabilities {
                        fs-read
                        fs-read
                        fs-write
                    }
                }
            }
        "#;
        let got = parse_plugins_block_semantic(src).unwrap().unwrap();
        let caps: Vec<&str> = got.plugins[0]
            .capabilities
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(caps, vec!["fs-read", "fs-write"]);
    }

    #[test]
    fn known_capabilities_snapshot_matches_kit_r5() {
        // Kit R5 lists six caps: fs-read, fs-write, network,
        // spawn-process, bus-send, bus-receive. If this test fails, you
        // either added a cap (bump the WIT world + kit revision) or
        // deleted one (same).
        let expected: std::collections::BTreeSet<&str> = [
            "fs-read",
            "fs-write",
            "network",
            "spawn-process",
            "bus-send",
            "bus-receive",
        ]
        .into_iter()
        .collect();
        let actual: std::collections::BTreeSet<&str> =
            KNOWN_CAPABILITIES.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn semantic_invalid_runtime_value_errors() {
        let src = r#"
            plugins {
                p location="https://example.com/p.wasm" {
                    runtime {
                        update-failure-budget "not-a-number"
                    }
                }
            }
        "#;
        let err = parse_plugins_block_semantic(src).unwrap_err();
        match err {
            PluginsKdlError::Semantic(PluginsSemanticError::InvalidRuntimeValue {
                key,
                value,
            }) => {
                assert_eq!(key, "update-failure-budget");
                assert_eq!(value, "not-a-number");
            }
            other => panic!("expected InvalidRuntimeValue, got {other:?}"),
        }
    }
}
