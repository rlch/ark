//! Transitive `use` resolution with cycle detection and topo-sort (T-095).
//!
//! When a scene file contains `use "<ext>"`, that extension's metadata may
//! itself declare `requires` entries — transitive dependencies that must be
//! activated before the extension itself. [`resolve_uses`] walks the full
//! dependency graph, detects cycles via DFS gray/black colouring, enforces
//! a depth limit of [`MAX_DEPTH`] (16), and returns extensions in
//! topological activation order (dependencies first).
//!
//! The `lookup` closure abstracts over how metadata is fetched — compiled-in
//! registry, filesystem search path, or test doubles.

use std::collections::HashMap;

use ark_ext_metadata_types::ExtensionMetadata;

use crate::error::SceneError;

/// Maximum recursion depth for transitive `use` resolution. Exceeding
/// this limit produces `error[ext/cycle]` (depth-exceeded variant).
pub const MAX_DEPTH: usize = 16;

/// DFS node colour for cycle detection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Colour {
    /// Not yet visited.
    White,
    /// Currently on the DFS stack (ancestor of the node being explored).
    Gray,
    /// Fully explored — all descendants have been visited.
    Black,
}

/// Resolve `use` directives transitively with cycle detection and topo-sort.
///
/// Returns extensions in activation order (dependencies first). Each entry
/// is `(name, metadata)`. The list is deduplicated — if multiple paths
/// converge on the same extension it appears only once.
///
/// # Errors
///
/// - [`SceneError::ExtCycle`] if a dependency cycle is detected.
/// - [`SceneError::ExtCycle`] if the dependency depth exceeds [`MAX_DEPTH`].
/// - [`SceneError::ExtMissing`] (constructed by the caller via the `lookup`
///   closure returning `None`) is NOT emitted here — callers should map a
///   `None` return from `lookup` into `ExtMissing` before calling this
///   function. Instead, this function returns a lightweight `ExtMissing`
///   with empty help/src/span when `lookup` yields `None`.
pub fn resolve_uses(
    use_names: &[String],
    lookup: &dyn Fn(&str) -> Option<ExtensionMetadata>,
) -> Result<Vec<(String, ExtensionMetadata)>, SceneError> {
    let mut colour: HashMap<String, Colour> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut meta_cache: HashMap<String, ExtensionMetadata> = HashMap::new();

    for name in use_names {
        if colour.get(name.as_str()).copied().unwrap_or(Colour::White) == Colour::White {
            visit(
                name,
                lookup,
                &mut colour,
                &mut order,
                &mut meta_cache,
                &mut vec![name.clone()],
                1,
            )?;
        }
    }

    // Build the result in topo order (dependencies first).
    let mut result = Vec::with_capacity(order.len());
    for name in &order {
        if let Some(meta) = meta_cache.remove(name) {
            result.push((name.clone(), meta));
        }
    }

    Ok(result)
}

/// Recursive DFS visitor with gray/black colouring.
fn visit(
    name: &str,
    lookup: &dyn Fn(&str) -> Option<ExtensionMetadata>,
    colour: &mut HashMap<String, Colour>,
    order: &mut Vec<String>,
    meta_cache: &mut HashMap<String, ExtensionMetadata>,
    trail: &mut Vec<String>,
    depth: usize,
) -> Result<(), SceneError> {
    // Depth guard.
    if depth > MAX_DEPTH {
        return Err(SceneError::ExtCycle {
            trail: trail.clone(),
        });
    }

    // Look up metadata.
    let meta = lookup(name).ok_or_else(|| SceneError::ExtMissing {
        name: name.to_string(),
        help: String::new(),
        src: miette::NamedSource::new("<resolve>", String::new()),
        span: (0, 0).into(),
    })?;

    // Mark as in-progress (gray).
    colour.insert(name.to_string(), Colour::Gray);
    meta_cache.insert(name.to_string(), meta.clone());

    // Visit transitive dependencies.
    for dep in &meta.requires {
        let dep_name = dep.value.as_str();
        match colour.get(dep_name).copied().unwrap_or(Colour::White) {
            Colour::Gray => {
                // Cycle detected — build trail ending with the repeated name.
                let mut cycle_trail = trail.clone();
                cycle_trail.push(dep_name.to_string());
                return Err(SceneError::ExtCycle {
                    trail: cycle_trail,
                });
            }
            Colour::White => {
                trail.push(dep_name.to_string());
                visit(dep_name, lookup, colour, order, meta_cache, trail, depth + 1)?;
                trail.pop();
            }
            Colour::Black => {
                // Already fully explored — skip (diamond dedup).
            }
        }
    }

    // Mark as fully explored (black) and record in topo order.
    colour.insert(name.to_string(), Colour::Black);
    // Only add if not already present (handles diamond convergence from
    // multiple top-level use entries pointing at the same transitive dep).
    if !order.iter().any(|n| n == name) {
        order.push(name.to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode,
    };
    use std::collections::HashMap;

    /// Build a minimal `ExtensionMetadata` with the given requires list.
    fn meta(name: &str, requires: &[&str]) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: requires.iter().map(|r| StringNode::new(*r)).collect(),
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
        }
    }

    /// Build a lookup closure from a map of name -> metadata.
    fn make_lookup(
        map: HashMap<String, ExtensionMetadata>,
    ) -> impl Fn(&str) -> Option<ExtensionMetadata> {
        move |name: &str| map.get(name).cloned()
    }

    // ── Linear chain A→B→C resolves in order C, B, A ──────────────

    #[test]
    fn linear_chain_resolves_deps_first() {
        let mut map = HashMap::new();
        map.insert("A".into(), meta("A", &["B"]));
        map.insert("B".into(), meta("B", &["C"]));
        map.insert("C".into(), meta("C", &[]));
        let lookup = make_lookup(map);

        let result = resolve_uses(&["A".into()], &lookup).unwrap();
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["C", "B", "A"]);
    }

    // ── Cycle A→B→A errors ────────────────────────────────────────

    #[test]
    fn cycle_detected() {
        let mut map = HashMap::new();
        map.insert("A".into(), meta("A", &["B"]));
        map.insert("B".into(), meta("B", &["A"]));
        let lookup = make_lookup(map);

        let err = resolve_uses(&["A".into()], &lookup).unwrap_err();
        match err {
            SceneError::ExtCycle { trail } => {
                assert_eq!(trail, vec!["A", "B", "A"]);
            }
            other => panic!("expected ExtCycle, got: {other:?}"),
        }
    }

    // ── Diamond A→B, A→C, B→D, C→D deduplicates D ────────────────

    #[test]
    fn diamond_deduplicates() {
        let mut map = HashMap::new();
        map.insert("A".into(), meta("A", &["B", "C"]));
        map.insert("B".into(), meta("B", &["D"]));
        map.insert("C".into(), meta("C", &["D"]));
        map.insert("D".into(), meta("D", &[]));
        let lookup = make_lookup(map);

        let result = resolve_uses(&["A".into()], &lookup).unwrap();
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();

        // D appears exactly once.
        assert_eq!(names.iter().filter(|&&n| n == "D").count(), 1);
        // D comes before B, C, and A.
        let pos = |n: &str| names.iter().position(|&x| x == n).unwrap();
        assert!(pos("D") < pos("B"));
        assert!(pos("D") < pos("C"));
        assert!(pos("B") < pos("A"));
        assert!(pos("C") < pos("A"));
    }

    // ── Depth limit exceeded ──────────────────────────────────────

    #[test]
    fn depth_limit_exceeded() {
        // Build a chain of depth MAX_DEPTH + 1.
        let mut map = HashMap::new();
        for i in 0..=MAX_DEPTH {
            let name = format!("ext-{i}");
            let dep = if i < MAX_DEPTH {
                vec![format!("ext-{}", i + 1)]
            } else {
                vec![]
            };
            let dep_refs: Vec<&str> = dep.iter().map(|s| s.as_str()).collect();
            map.insert(name.clone(), meta(&name, &dep_refs));
        }
        let lookup = make_lookup(map);

        let err = resolve_uses(&["ext-0".into()], &lookup).unwrap_err();
        match err {
            SceneError::ExtCycle { trail } => {
                // Trail should have MAX_DEPTH + 1 entries (the full path
                // from ext-0 down to the point where depth was exceeded).
                assert!(
                    trail.len() > MAX_DEPTH,
                    "expected trail length > {MAX_DEPTH}, got {}",
                    trail.len()
                );
            }
            other => panic!("expected ExtCycle (depth exceeded), got: {other:?}"),
        }
    }

    // ── Missing extension ─────────────────────────────────────────

    #[test]
    fn missing_extension_errors() {
        let map = HashMap::new();
        let lookup = make_lookup(map);

        let err = resolve_uses(&["ghost".into()], &lookup).unwrap_err();
        match err {
            SceneError::ExtMissing { name, .. } => {
                assert_eq!(name, "ghost");
            }
            other => panic!("expected ExtMissing, got: {other:?}"),
        }
    }

    // ── Empty use list ────────────────────────────────────────────

    #[test]
    fn empty_use_list_returns_empty() {
        let map = HashMap::new();
        let lookup = make_lookup(map);

        let result = resolve_uses(&[], &lookup).unwrap();
        assert!(result.is_empty());
    }

    // ── Multiple top-level uses with shared dep ───────────────────

    #[test]
    fn multiple_roots_shared_dep_deduplicates() {
        let mut map = HashMap::new();
        map.insert("X".into(), meta("X", &["shared"]));
        map.insert("Y".into(), meta("Y", &["shared"]));
        map.insert("shared".into(), meta("shared", &[]));
        let lookup = make_lookup(map);

        let result = resolve_uses(&["X".into(), "Y".into()], &lookup).unwrap();
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();

        // "shared" appears exactly once, before both X and Y.
        assert_eq!(names.iter().filter(|&&n| n == "shared").count(), 1);
        let pos = |n: &str| names.iter().position(|&x| x == n).unwrap();
        assert!(pos("shared") < pos("X"));
        assert!(pos("shared") < pos("Y"));
    }

    // ── No deps returns just the requested extension ──────────────

    #[test]
    fn no_deps_returns_single() {
        let mut map = HashMap::new();
        map.insert("solo".into(), meta("solo", &[]));
        let lookup = make_lookup(map);

        let result = resolve_uses(&["solo".into()], &lookup).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "solo");
    }

    // ── Self-cycle ────────────────────────────────────────────────

    #[test]
    fn self_cycle_detected() {
        let mut map = HashMap::new();
        map.insert("narcissist".into(), meta("narcissist", &["narcissist"]));
        let lookup = make_lookup(map);

        let err = resolve_uses(&["narcissist".into()], &lookup).unwrap_err();
        match err {
            SceneError::ExtCycle { trail } => {
                assert_eq!(trail, vec!["narcissist", "narcissist"]);
            }
            other => panic!("expected ExtCycle, got: {other:?}"),
        }
    }
}
