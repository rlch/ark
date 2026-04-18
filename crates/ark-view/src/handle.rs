//! T-004 + T-005 (soul phase 2 cavekit R2 + R5): the untyped `HandleKind`
//! enum and the opaque `HandleId` wire-format newtype.
//!
//! These two types are the primitive "header" of every handle the view
//! surface exposes. Typed wrappers (`Pane<V>`, `Stack<V>`, `TabHandle`)
//! land in later tiers (T-009..T-012); their `V` parameter is
//! compile-time-only and never touches the wire — the wire always
//! carries just `(HandleKind, HandleId)`.

/// Kind of handle a scene declares — narrowed in Phase 2 to the three
/// kinds zellij itself distinguishes. The old `Command` and `Plugin`
/// variants conflated render-mode with handle-kind; view-type info now
/// lives exclusively on the typed wrapper `Pane<V>` (see R3/R4).
///
/// `#[non_exhaustive]` lets future Rust consumers add match arms
/// without Rust-side breakage. It does **not** buy wire-format forward
/// compatibility: a 1.0 peer receiving a 1.1 variant over serde will
/// still fail with `unknown variant`. Any new kind requires a peer
/// protocol bump (see `CURRENT_PROTOCOL_VERSION`).
#[derive(
    Copy, Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize, facet::Facet,
)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
#[non_exhaustive]
pub enum HandleKind {
    Tab,
    Pane,
    Stack,
}

/// Opaque per-handle string identifier. Collapses every typed handle
/// (`Pane<V>`, `Stack<V>`, `TabHandle`) to a single wire-format string.
///
/// The `V` type parameter on typed wrappers is compile-time-only; the
/// wire format carries only this id (see R5). Consumers MUST treat the
/// id as opaque — no splitting, pattern-matching, or prefix-sniffing.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize, facet::Facet)]
#[serde(transparent)]
pub struct HandleId(String);

impl HandleId {
    /// Construct from any string-convertible value.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HandleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    // ---- HandleKind ---------------------------------------------------------

    #[test]
    fn handle_kind_serde_lowercase_tag() {
        assert_eq!(serde_json::to_value(HandleKind::Tab).unwrap(), json!("tab"));
        assert_eq!(
            serde_json::to_value(HandleKind::Pane).unwrap(),
            json!("pane")
        );
        assert_eq!(
            serde_json::to_value(HandleKind::Stack).unwrap(),
            json!("stack")
        );
    }

    #[test]
    fn handle_kind_roundtrip_all_variants() {
        for kind in [HandleKind::Tab, HandleKind::Pane, HandleKind::Stack] {
            let s = serde_json::to_string(&kind).unwrap();
            let back: HandleKind = serde_json::from_str(&s).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn handle_kind_exhaustive_pattern_match() {
        // `#[non_exhaustive]` requires a wildcard arm, which is acceptable
        // per the kit: this test asserts every current variant maps to a
        // distinct string. Later tiers can add variants without breaking
        // the wildcard arm's existence.
        fn describe(k: HandleKind) -> &'static str {
            match k {
                HandleKind::Tab => "tab",
                HandleKind::Pane => "pane",
                HandleKind::Stack => "stack",
                _ => "unknown",
            }
        }
        assert_eq!(describe(HandleKind::Tab), "tab");
        assert_eq!(describe(HandleKind::Pane), "pane");
        assert_eq!(describe(HandleKind::Stack), "stack");
        // Distinctness.
        let tab = describe(HandleKind::Tab);
        let pane = describe(HandleKind::Pane);
        let stack = describe(HandleKind::Stack);
        assert_ne!(tab, pane);
        assert_ne!(pane, stack);
        assert_ne!(tab, stack);
    }

    #[test]
    fn handle_kind_is_copy_eq_hash() {
        // Copy: use twice without `clone()`.
        let k = HandleKind::Pane;
        let a = k;
        let b = k;
        assert_eq!(a, b);

        // Hash + Eq: works as a HashMap key.
        let mut map: HashMap<HandleKind, usize> = HashMap::new();
        map.insert(HandleKind::Tab, 1);
        map.insert(HandleKind::Pane, 2);
        map.insert(HandleKind::Stack, 3);
        assert_eq!(map.get(&HandleKind::Tab), Some(&1));
        assert_eq!(map.get(&HandleKind::Pane), Some(&2));
        assert_eq!(map.get(&HandleKind::Stack), Some(&3));
    }

    // ---- HandleId -----------------------------------------------------------

    #[test]
    fn handle_id_serializes_as_plain_string() {
        let id = HandleId::new("abc-123");
        assert_eq!(serde_json::to_value(&id).unwrap(), json!("abc-123"));
    }

    #[test]
    fn handle_id_deserializes_from_plain_string() {
        let id: HandleId = serde_json::from_str("\"abc-123\"").unwrap();
        assert_eq!(id, HandleId::new("abc-123"));
        assert_eq!(id.as_str(), "abc-123");
    }

    #[test]
    fn handle_id_roundtrip_preserves_bytes() {
        for raw in [
            "",
            "simple",
            "with-dash_and_underscore",
            "utf8-café-🦀",
            "  spaces  ",
        ] {
            let id = HandleId::new(raw);
            let s = serde_json::to_string(&id).unwrap();
            let back: HandleId = serde_json::from_str(&s).unwrap();
            assert_eq!(id, back);
            assert_eq!(back.as_str(), raw);
        }
    }

    #[test]
    fn handle_id_display_matches_inner() {
        let id = HandleId::new("display-me");
        assert_eq!(format!("{}", id), "display-me");
    }
}
