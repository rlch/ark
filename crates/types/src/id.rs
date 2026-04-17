//! Session identity — `SessionId` for ark sessions.
//!
//! Every session has a human-friendly `name` plus a freshly generated
//! `Ulid`. Path-leaf form is `<name>-<ulid>`. See cavekit-soul-phase-1-types.md R3.
//!
//! `AgentId` is retained as a transitional alias for [`SessionId`] —
//! some pre-soul callers (mux/layout writer, hook bridge, plugins) still
//! reference the old name. Phase 2+ extension migration removes the
//! alias once those call sites move off `AgentId`.

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

    /// Render as a single string. Equivalent to [`Self::as_path_leaf`];
    /// retained for transitional callers that used the legacy `AgentId::as_str`.
    pub fn as_str(&self) -> String {
        self.as_path_leaf()
    }

    /// Parse a `<name>-<ulid>` string into a `SessionId`. Lossy on a
    /// malformed input (returns `Err` if the trailing 26-char ulid does
    /// not parse).
    ///
    /// Retained for transitional callers that used the legacy
    /// `AgentId::parse` constructor; new code should use [`Self::new`]
    /// (which generates a fresh ulid).
    pub fn parse(s: &str) -> Result<Self, String> {
        let (name, ulid) = s
            .rsplit_once('-')
            .ok_or_else(|| format!("missing `-` in id `{s}`"))?;
        let ulid = Ulid::from_string(&ulid.to_uppercase())
            .map_err(|e| format!("bad ulid in `{s}`: {e}"))?;
        Ok(Self {
            name: sanitize(name),
            ulid,
        })
    }
}

/// Transitional alias for [`SessionId`].
///
/// Pre-soul code referenced an `AgentId` distinct from `SessionId`; the
/// two unified under cavekit-soul Phase 1. The alias exists so untouched
/// call sites (mux layout writer, hook bridge, plugins) continue to
/// compile while Phase 2+ tiers move them off the legacy name.
pub type AgentId = SessionId;

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
