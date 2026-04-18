//! View-type symbol table + manifest-set hash (T-034).
//!
//! Builds a `<ext>.<view> → ViewDecl` lookup from the set of
//! installed extension manifests. Scene compile queries this table to
//! reject `pane @h { view "..." }` references to undeclared views +
//! `stack` references whose declared kind doesn't match.
//!
//! Also exposes a stable manifest-set hash (blake3 of canonical JSON)
//! that figment + scene-compile cache layers key on to detect ext
//! install/update/remove transitions.
//!
//! Per cavekit-soul-phase-2-host-dispatch.md R4, phase-2-design-
//! decisions.md §R-6 (algorithm locked to blake3).

use ark_ext_metadata_types::{ExtensionMetadata, ViewDecl};
use std::collections::BTreeMap;

/// View-type identity on the wire — `<ext>.<view>`.
pub type ViewTypeToken = String;

/// Symbol table of installed view types. Keyed by fully-qualified
/// `<ext>.<view>` token for O(log n) lookup during scene validation.
///
/// Built once from the set of extension manifests (see
/// [`ViewTypeTable::from_manifests`]) and reused across every scene
/// file compile. The table is owned by the scene-compile cache layer
/// and invalidated via [`manifest_set_hash`] when the installed
/// extension set changes.
#[derive(Debug, Clone, Default)]
pub struct ViewTypeTable {
    /// `<ext>.<view>` → ViewEntry. BTreeMap keeps iteration order
    /// deterministic for diagnostics + "did you mean …?" listings.
    entries: BTreeMap<ViewTypeToken, ViewEntry>,
}

/// One row in the [`ViewTypeTable`]. Carries the ext name that
/// contributed the view alongside the full [`ViewDecl`] so locatable
/// diagnostics can name the origin extension without a second lookup.
#[derive(Debug, Clone)]
pub struct ViewEntry {
    /// Extension that contributed this view.
    pub ext_name: String,
    /// Full ViewDecl (with name, component, kind).
    pub decl: ViewDecl,
}

impl ViewTypeTable {
    /// Build from an iterator of `(ext_name, metadata)` pairs. Each
    /// metadata's `views` vector contributes one entry per ViewDecl,
    /// keyed by `<ext>.<view>`.
    ///
    /// Duplicate tokens (same `<ext>.<view>` from two manifest copies)
    /// are resolved last-wins; the cache layer above is responsible
    /// for feeding a deduplicated manifest set in the first place.
    pub fn from_manifests<I>(manifests: I) -> Self
    where
        I: IntoIterator<Item = (String, ExtensionMetadata)>,
    {
        let mut entries = BTreeMap::new();
        for (ext_name, meta) in manifests {
            for view in &meta.views {
                let token = format!("{}.{}", ext_name, view.name);
                entries.insert(
                    token,
                    ViewEntry {
                        ext_name: ext_name.clone(),
                        decl: view.clone(),
                    },
                );
            }
        }
        Self { entries }
    }

    /// Look up a view by its fully-qualified `<ext>.<view>` token.
    pub fn lookup(&self, token: &str) -> Option<&ViewEntry> {
        self.entries.get(token)
    }

    /// Count of declared view types.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no views are declared (no extensions installed, or
    /// installed extensions declare no views).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate every declared `(token, entry)` pair in sorted order.
    /// Useful for typo-suggestion + `ark scene list-views`-style CLI
    /// surfaces.
    pub fn iter(&self) -> impl Iterator<Item = (&ViewTypeToken, &ViewEntry)> {
        self.entries.iter()
    }
}

/// Validation error for a view-type reference. Locatable via the
/// `(file, line, column)` triple so upstream scene errors can show
/// an exact diagnostic pointer.
#[derive(Debug, Clone)]
pub struct ViewTypeError {
    /// What went wrong.
    pub kind: ViewTypeErrorKind,
    /// The offending token as written in the scene file.
    pub token: String,
    /// Source location of the reference, if known. Absent when the
    /// caller couldn't compute a span (e.g. synthetic scenes in tests).
    pub location: Option<SourceLocation>,
}

/// Error kind for a view-type reference.
#[derive(Debug, Clone)]
pub enum ViewTypeErrorKind {
    /// Token isn't declared by any installed extension.
    Unknown,
    /// Token is declared but the scene used it in a context that
    /// doesn't match the declared kind (e.g. `stack @h { view "X" }`
    /// where X is declared as kind=pane, or vice versa).
    KindMismatch {
        /// Kind required by the surrounding scene context (`"pane"` or
        /// `"stack"`).
        expected: String,
        /// Kind the extension actually declared on the view.
        declared: String,
    },
}

/// `(file, line, column)` triple locating the offending token in the
/// scene source. 1-indexed line + column per the standard editor
/// convention.
#[derive(Debug, Clone)]
pub struct SourceLocation {
    /// Path to the scene file carrying the reference.
    pub file: String,
    /// 1-indexed line number.
    pub line: u32,
    /// 1-indexed column.
    pub column: u32,
}

impl std::fmt::Display for ViewTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ViewTypeErrorKind::Unknown => {
                write!(f, "unknown view type \"{}\"", self.token)?;
            }
            ViewTypeErrorKind::KindMismatch { expected, declared } => {
                write!(
                    f,
                    "view type \"{}\" declared as kind={} but used in a context requiring kind={}",
                    self.token, declared, expected
                )?;
            }
        }
        if let Some(loc) = &self.location {
            write!(f, " at {}:{}:{}", loc.file, loc.line, loc.column)?;
        }
        Ok(())
    }
}

impl std::error::Error for ViewTypeError {}

/// Validate a `pane @h { view "..." }` / `stack @h { view "..." }`
/// reference against the table. `expected_kind` is the scene context:
/// `"pane"` when the reference appeared under a pane node, `"stack"`
/// under a stack node.
///
/// Returns `Ok(())` when the token is declared AND its declared kind
/// matches `expected_kind`. Returns [`ViewTypeError::Unknown`] when
/// the token isn't in the table, or [`ViewTypeErrorKind::KindMismatch`]
/// when the kinds disagree.
pub fn validate_view_reference(
    table: &ViewTypeTable,
    token: &str,
    expected_kind: &str,
    location: Option<SourceLocation>,
) -> Result<(), ViewTypeError> {
    let Some(entry) = table.lookup(token) else {
        return Err(ViewTypeError {
            kind: ViewTypeErrorKind::Unknown,
            token: token.to_string(),
            location,
        });
    };

    // T-023 stores ViewDecl.kind as Option<StringNode>. Absent = default
    // "pane" (conservative R17 default for pre-T-023 manifests).
    let declared_kind = entry
        .decl
        .kind
        .as_ref()
        .map(|k| k.value.as_str())
        .unwrap_or("pane");
    if declared_kind != expected_kind {
        return Err(ViewTypeError {
            kind: ViewTypeErrorKind::KindMismatch {
                expected: expected_kind.to_string(),
                declared: declared_kind.to_string(),
            },
            token: token.to_string(),
            location,
        });
    }

    Ok(())
}

/// Compute a reproducible hash of the manifest set. Canonicalised by
/// sorting `(ext_name, view_name)` entries and emitting minimal JSON
/// before feeding to blake3. Identical input → identical output across
/// runs and across machines.
///
/// Used as a cache key for figment config-section materialisation and
/// scene-compile view-type resolution (phase-2 design decision §R-6).
/// Per §R-6 the algorithm is locked to blake3 — do NOT substitute
/// SHA-256 / xxhash without bumping the cache-key salt.
pub fn manifest_set_hash(manifests: &[(String, ExtensionMetadata)]) -> [u8; 32] {
    // Collect a canonical view of (ext, view_name, kind, component)
    // triples — the minimum needed to decide cache invalidation. Any
    // structural diff in this projection should bust the cache.
    let mut entries: Vec<serde_json::Value> = Vec::new();
    for (ext_name, meta) in manifests {
        for view in &meta.views {
            let kind = view
                .kind
                .as_ref()
                .map(|k| k.value.clone())
                .unwrap_or_else(|| "pane".to_string());
            entries.push(serde_json::json!({
                "ext": ext_name,
                "view": view.name,
                "component": view.component.value,
                "kind": kind,
            }));
        }
    }
    // Deterministic order: sort by (ext, view).
    entries.sort_by(|a, b| {
        let ak = (
            a["ext"].as_str().unwrap_or(""),
            a["view"].as_str().unwrap_or(""),
        );
        let bk = (
            b["ext"].as_str().unwrap_or(""),
            b["view"].as_str().unwrap_or(""),
        );
        ak.cmp(&bk)
    });
    let canonical = serde_json::to_string(&entries).expect("manifest canonical ser");
    *blake3::hash(canonical.as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode, ViewDecl,
    };

    fn decl(name: &str, component: &str, kind: Option<&str>) -> ViewDecl {
        ViewDecl {
            name: name.to_string(),
            component: StringNode::new(component),
            kind: kind.map(StringNode::new),
        }
    }

    fn make_meta(name: &str, views: Vec<ViewDecl>) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("1.0.0"),
            ark_range: StringNode::new(">=0.1"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views,
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        }
    }

    #[test]
    fn lookup_returns_entry_when_declared() {
        let meta = make_meta(
            "my-ext",
            vec![decl("panel", "PanelComponent", Some("pane"))],
        );
        let table = ViewTypeTable::from_manifests([("my-ext".to_string(), meta)]);
        let entry = table.lookup("my-ext.panel").expect("present");
        assert_eq!(entry.ext_name, "my-ext");
        assert_eq!(entry.decl.name, "panel");
    }

    #[test]
    fn lookup_returns_none_for_unknown_token() {
        let table = ViewTypeTable::default();
        assert!(table.lookup("unknown.view").is_none());
    }

    #[test]
    fn table_len_and_is_empty_reflect_inserts() {
        let empty = ViewTypeTable::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let meta = make_meta(
            "ext1",
            vec![decl("a", "A", None), decl("b", "B", Some("stack"))],
        );
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        assert!(!table.is_empty());
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn validate_accepts_matching_pane_kind() {
        let meta = make_meta("ext1", vec![decl("foo", "Foo", Some("pane"))]);
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        assert!(validate_view_reference(&table, "ext1.foo", "pane", None).is_ok());
    }

    #[test]
    fn validate_accepts_stack_kind() {
        let meta = make_meta("ext1", vec![decl("grid", "GridView", Some("stack"))]);
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        assert!(validate_view_reference(&table, "ext1.grid", "stack", None).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_token_with_location() {
        let table = ViewTypeTable::default();
        let loc = SourceLocation {
            file: "main.kdl".to_string(),
            line: 12,
            column: 5,
        };
        let err =
            validate_view_reference(&table, "nope.missing", "pane", Some(loc.clone())).unwrap_err();
        match err.kind {
            ViewTypeErrorKind::Unknown => {}
            other => panic!("wrong kind: {other:?}"),
        }
        assert_eq!(err.token, "nope.missing");
        let loc = err.location.as_ref().unwrap();
        assert_eq!(loc.file, "main.kdl");
        assert_eq!(loc.line, 12);
        assert_eq!(loc.column, 5);
        // Display renders the trailing "at file:line:col" suffix.
        let rendered = err.to_string();
        assert!(rendered.contains("main.kdl:12:5"), "got: {rendered}");
    }

    #[test]
    fn validate_rejects_pane_used_as_stack() {
        let meta = make_meta("ext1", vec![decl("foo", "Foo", Some("pane"))]);
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        let err = validate_view_reference(&table, "ext1.foo", "stack", None).unwrap_err();
        match err.kind {
            ViewTypeErrorKind::KindMismatch { expected, declared } => {
                assert_eq!(expected, "stack");
                assert_eq!(declared, "pane");
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_stack_used_as_pane() {
        let meta = make_meta("ext1", vec![decl("grid", "GridView", Some("stack"))]);
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        let err = validate_view_reference(&table, "ext1.grid", "pane", None).unwrap_err();
        match err.kind {
            ViewTypeErrorKind::KindMismatch { expected, declared } => {
                assert_eq!(expected, "pane");
                assert_eq!(declared, "stack");
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn missing_kind_defaults_to_pane() {
        let meta = make_meta("ext1", vec![decl("legacy", "LegacyView", None)]);
        let table = ViewTypeTable::from_manifests([("ext1".to_string(), meta)]);
        assert!(validate_view_reference(&table, "ext1.legacy", "pane", None).is_ok());
        assert!(validate_view_reference(&table, "ext1.legacy", "stack", None).is_err());
    }

    #[test]
    fn multi_ext_tokens_namespaced_correctly() {
        let m1 = make_meta("ext-a", vec![decl("view1", "V1", None)]);
        let m2 = make_meta("ext-b", vec![decl("view1", "V1Other", Some("stack"))]);
        let table =
            ViewTypeTable::from_manifests([("ext-a".to_string(), m1), ("ext-b".to_string(), m2)]);
        // Same view name, different ext → two distinct tokens.
        assert_eq!(table.len(), 2);
        assert_eq!(table.lookup("ext-a.view1").unwrap().ext_name, "ext-a");
        assert_eq!(table.lookup("ext-b.view1").unwrap().ext_name, "ext-b");
    }

    #[test]
    fn hash_is_deterministic() {
        let meta = make_meta("ext1", vec![decl("foo", "Foo", Some("pane"))]);
        let input = vec![("ext1".to_string(), meta)];
        let h1 = manifest_set_hash(&input);
        let h2 = manifest_set_hash(&input);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_is_order_independent_across_manifests() {
        let m1 = make_meta("a-ext", vec![decl("foo", "Foo", None)]);
        let m2 = make_meta("z-ext", vec![decl("bar", "Bar", Some("stack"))]);
        let h_ab = manifest_set_hash(&[
            ("a-ext".to_string(), m1.clone()),
            ("z-ext".to_string(), m2.clone()),
        ]);
        let h_ba = manifest_set_hash(&[("z-ext".to_string(), m2), ("a-ext".to_string(), m1)]);
        assert_eq!(h_ab, h_ba);
    }

    #[test]
    fn hash_changes_when_view_added() {
        let m = make_meta("ext1", vec![decl("foo", "Foo", None)]);
        let h1 = manifest_set_hash(&[("ext1".to_string(), m.clone())]);

        let m2 = make_meta(
            "ext1",
            vec![decl("foo", "Foo", None), decl("bar", "Bar", None)],
        );
        let h2 = manifest_set_hash(&[("ext1".to_string(), m2)]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_when_kind_changes() {
        let m_pane = make_meta("ext1", vec![decl("v", "V", Some("pane"))]);
        let m_stack = make_meta("ext1", vec![decl("v", "V", Some("stack"))]);
        let h_pane = manifest_set_hash(&[("ext1".to_string(), m_pane)]);
        let h_stack = manifest_set_hash(&[("ext1".to_string(), m_stack)]);
        assert_ne!(h_pane, h_stack);
    }

    #[test]
    fn hash_changes_when_component_changes() {
        let m1 = make_meta("ext1", vec![decl("v", "ComponentA", None)]);
        let m2 = make_meta("ext1", vec![decl("v", "ComponentB", None)]);
        let h1 = manifest_set_hash(&[("ext1".to_string(), m1)]);
        let h2 = manifest_set_hash(&[("ext1".to_string(), m2)]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn empty_manifest_set_hashes_cleanly() {
        // No installed extensions = valid state; hash must still be stable.
        let h1 = manifest_set_hash(&[]);
        let h2 = manifest_set_hash(&[]);
        assert_eq!(h1, h2);
    }
}
