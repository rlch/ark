use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// Identifier for one agent run.
///
/// String form: `{orchestrator}-{name}-{ulid}`.
/// All three components are normalized to be filesystem-safe and URL-safe:
/// only `[a-z0-9_]` plus the two `-` separators introduced by this type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentIdParseError {
    #[error("agent id is empty")]
    Empty,
    #[error("agent id missing orchestrator/name/ulid segments: `{0}`")]
    MissingSegments(String),
    #[error("agent id contains unsafe characters: `{0}`")]
    UnsafeCharacters(String),
    #[error("agent id ulid suffix is invalid: `{0}`")]
    InvalidUlid(String),
}

impl AgentId {
    /// Build a new agent id with a freshly generated ULID suffix.
    pub fn new(orchestrator: &str, name: &str) -> Self {
        let orchestrator = sanitize(orchestrator);
        let name = sanitize(name);
        let ulid = Ulid::new().to_string().to_lowercase();
        Self(format!("{orchestrator}-{name}-{ulid}"))
    }

    /// Build an id from explicit pieces — used by tests and replay paths
    /// where the ULID is fixed.
    pub fn from_parts(orchestrator: &str, name: &str, ulid: Ulid) -> Self {
        let orchestrator = sanitize(orchestrator);
        let name = sanitize(name);
        let ulid = ulid.to_string().to_lowercase();
        Self(format!("{orchestrator}-{name}-{ulid}"))
    }

    /// Parse an existing id, validating shape and character set.
    pub fn parse(s: &str) -> Result<Self, AgentIdParseError> {
        if s.is_empty() {
            return Err(AgentIdParseError::Empty);
        }
        if !s.bytes().all(is_id_byte) {
            return Err(AgentIdParseError::UnsafeCharacters(s.to_string()));
        }
        let (rest, ulid_str) = s
            .rsplit_once('-')
            .ok_or_else(|| AgentIdParseError::MissingSegments(s.to_string()))?;
        if rest.is_empty() || ulid_str.is_empty() {
            return Err(AgentIdParseError::MissingSegments(s.to_string()));
        }
        if !rest.contains('-') {
            return Err(AgentIdParseError::MissingSegments(s.to_string()));
        }
        Ulid::from_string(&ulid_str.to_uppercase())
            .map_err(|_| AgentIdParseError::InvalidUlid(ulid_str.to_string()))?;
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Orchestrator slug (first hyphen-delimited segment).
    pub fn orchestrator(&self) -> &str {
        self.split().0
    }

    /// Human name (middle segment — may itself contain hyphens after sanitization, but
    /// sanitization replaces hyphens, so this is always a single token in practice).
    pub fn name(&self) -> &str {
        self.split().1
    }

    /// Lowercase ULID suffix (last segment).
    pub fn ulid(&self) -> &str {
        self.split().2
    }

    /// Zellij-friendly session name. ULID suffix is dropped for brevity;
    /// callers are responsible for collision handling (`-{short-ulid}`
    /// suffix on collision is the documented convention).
    pub fn session_name(&self) -> String {
        format!("ark-{}-{}", self.orchestrator(), self.name())
    }

    /// Per-agent state directory under the supplied state base.
    pub fn state_dir(&self, base: &Path) -> PathBuf {
        base.join("agents").join(&self.0)
    }

    fn split(&self) -> (&str, &str, &str) {
        let mut it = self.0.splitn(3, '-');
        let orchestrator = it.next().unwrap_or("");
        let name = it.next().unwrap_or("");
        let ulid = it.next().unwrap_or("");
        (orchestrator, name, ulid)
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentId {
    type Err = AgentIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Normalize a free-form string to id-safe characters: lowercase ASCII letters,
/// digits, and `_`. Anything else collapses to `_`.
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

fn is_id_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_format_has_three_segments() {
        let id = AgentId::new("cavekit", "myfeat");
        let parts: Vec<&str> = id.as_str().splitn(3, '-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "cavekit");
        assert_eq!(parts[1], "myfeat");
        assert_eq!(parts[2].len(), 26); // ULID
    }

    #[test]
    fn sanitize_strips_unsafe_chars() {
        let id = AgentId::new("Cave Kit!", "My Feat 2");
        assert!(id.as_str().starts_with("cave_kit_-my_feat_2-"));
    }

    #[test]
    fn session_name_drops_ulid() {
        let id = AgentId::new("cavekit", "auth");
        assert_eq!(id.session_name(), "ark-cavekit-auth");
    }

    #[test]
    fn state_dir_under_base() {
        let id = AgentId::new("cavekit", "auth");
        let base = Path::new("/state");
        let dir = id.state_dir(base);
        assert!(dir.starts_with("/state/agents/"));
        assert!(dir.to_string_lossy().contains(id.as_str()));
    }

    #[test]
    fn parse_roundtrip() {
        let id = AgentId::new("cavekit", "auth");
        let parsed = AgentId::parse(id.as_str()).expect("parse");
        assert_eq!(parsed, id);
    }

    #[test]
    fn parse_rejects_empty() {
        assert_eq!(AgentId::parse(""), Err(AgentIdParseError::Empty));
    }

    #[test]
    fn parse_rejects_missing_segments() {
        assert!(matches!(
            AgentId::parse("only-two"),
            Err(AgentIdParseError::MissingSegments(_))
        ));
    }

    #[test]
    fn parse_rejects_unsafe_chars() {
        assert!(matches!(
            AgentId::parse("foo/bar-baz-01jx7z8k6x9y2zt4abcdef0123"),
            Err(AgentIdParseError::UnsafeCharacters(_))
        ));
    }

    #[test]
    fn parse_rejects_invalid_ulid() {
        assert!(matches!(
            AgentId::parse("cavekit-auth-not_a_ulid"),
            Err(AgentIdParseError::InvalidUlid(_))
        ));
    }

    #[test]
    fn from_parts_uses_supplied_ulid() {
        let ulid = Ulid::new();
        let id = AgentId::from_parts("cavekit", "auth", ulid);
        assert!(id.as_str().ends_with(&ulid.to_string().to_lowercase()));
    }

    #[test]
    fn fs_safe_chars_only() {
        let id = AgentId::new("cavekit", "auth");
        for byte in id.as_str().bytes() {
            assert!(is_id_byte(byte), "byte {byte:#x} not fs-safe");
        }
    }

    #[test]
    fn url_safe_no_percent_encoding_needed() {
        let id = AgentId::new("cavekit", "auth");
        let raw = id.as_str();
        let encoded: String = raw
            .bytes()
            .map(|b| match b {
                b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        assert_eq!(raw, encoded);
    }

    #[test]
    fn serde_transparent_round_trip() {
        let id = AgentId::new("cavekit", "auth");
        let json = serde_json::to_string(&id).expect("ser");
        assert!(json.starts_with('"'));
        let back: AgentId = serde_json::from_str(&json).expect("de");
        assert_eq!(back, id);
    }
}
