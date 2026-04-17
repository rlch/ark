//! Session identity — `SessionId` for ark sessions.
//!
//! Every session has a human-friendly `name` plus a freshly generated
//! `Ulid`. Path-leaf form is `<name>-<ulid>`. See cavekit-soul-phase-1-types.md R3.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Identifier for an ark session.
///
/// Carries a human-friendly `name` and a `Ulid` generated at construction
/// time. The on-disk path leaf is `<name>-<ulid>` (see [`Self::as_path_leaf`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId {
    /// Human-friendly session name, sanitized to filesystem-safe characters.
    pub name: String,
    /// ULID generated at construction time for uniqueness.
    pub ulid: Ulid,
}

impl SessionId {
    /// Construct a new `SessionId` for the given name; generates a fresh
    /// ULID internally. The caller does not supply the ulid.
    pub fn new(name: &str) -> Self {
        Self {
            name: sanitize(name),
            ulid: Ulid::new(),
        }
    }

    /// On-disk path leaf: `<name>-<ulid>` (lowercase ulid).
    pub fn as_path_leaf(&self) -> String {
        format!("{}-{}", self.name, self.ulid.to_string().to_lowercase())
    }
}

/// Normalize a free-form string to id-safe characters: lowercase ASCII letters,
/// digits, and `_`. Anything else collapses to `_`. Empty input becomes `_`.
fn sanitize(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    for ch in lower.chars() {
        match ch {
            'a'..='z' | '0'..='9' | '_' => out.push(ch),
            _ => out.push('_'),
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_path_leaf_shape() {
        let id = SessionId::new("foo");
        let leaf = id.as_path_leaf();
        // Expect `foo-<26 lowercase alphanumeric ulid chars>`.
        let (name, ulid) = leaf.split_once('-').expect("has hyphen");
        assert_eq!(name, "foo");
        assert_eq!(ulid.len(), 26, "ulid segment must be 26 chars, got {ulid}");
        for ch in ulid.chars() {
            assert!(
                ch.is_ascii_digit() || ch.is_ascii_lowercase(),
                "ulid char {ch:?} not lowercase alphanumeric"
            );
        }
    }

    #[test]
    fn session_id_new_generates_distinct_ulids() {
        let a = SessionId::new("foo");
        let b = SessionId::new("foo");
        assert_ne!(a, b);
        assert_ne!(a.ulid, b.ulid);
    }

    #[test]
    fn session_id_sanitizes_unsafe_chars() {
        let id = SessionId::new("Foo Bar/baz");
        assert_eq!(id.name, "foo_bar_baz");
    }

    #[test]
    fn session_id_serde_roundtrip() {
        let id = SessionId::new("foo");
        let json = serde_json::to_string(&id).expect("ser");
        let back: SessionId = serde_json::from_str(&json).expect("de");
        assert_eq!(back, id);
    }
}
