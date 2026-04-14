//! Agent-ID resolution helper.
//!
//! Given a user-supplied `query` string, resolve it to a unique [`AgentId`]
//! against the on-disk state layout (`$STATE/agents/*/`).
//!
//! Resolution order (first match wins, ambiguity detected per tier):
//!   1. Exact match: `query` parses as a full `AgentId` AND its agent dir exists.
//!   2. Prefix match on the `AgentId` string.
//!   3. Substring match on the `AgentId` string.
//!   4. Name-field match (prefix then substring) against each agent's
//!      `spec.json` `name` field.
//!
//! See cavekit-cli.md R3 (`ark list`) and R4 (`ark kill`) — both delegate their
//! ID resolution to this helper.
//!
//! This module is a pure helper. It does NOT touch any supervisor process or
//! socket; it only reads directory names and optionally `spec.json` contents.
//! Missing state dir → `NotFound`. Malformed/missing `spec.json` → skipped
//! cleanly during the name-scan tier.

use std::fmt;
use std::fs;
use std::io;

use ark_types::{AgentId, StateLayout};
use serde::Deserialize;

/// Errors returned by [`resolve_agent_id`].
#[derive(Debug)]
pub enum ResolveError {
    /// No agent matched in any tier.
    NotFound { query: String },
    /// More than one agent dirname starts with `query`.
    AmbiguousPrefix {
        query: String,
        candidates: Vec<AgentId>,
    },
    /// More than one agent dirname contains `query` (non-prefix tier).
    AmbiguousSubstring {
        query: String,
        candidates: Vec<AgentId>,
    },
    /// More than one agent's `spec.json` name matches `query`.
    AmbiguousName {
        query: String,
        candidates: Vec<AgentId>,
    },
    /// I/O error reading the agents root (other than "not found", which maps
    /// to `NotFound`).
    Io(io::Error),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound { query } => {
                write!(f, "no agent found matching '{query}'")
            }
            ResolveError::AmbiguousPrefix { query, candidates } => {
                write!(
                    f,
                    "ambiguous query '{query}' — prefix matches: {}",
                    candidates_list(candidates)
                )
            }
            ResolveError::AmbiguousSubstring { query, candidates } => {
                write!(
                    f,
                    "ambiguous query '{query}' — substring matches: {}",
                    candidates_list(candidates)
                )
            }
            ResolveError::AmbiguousName { query, candidates } => {
                write!(
                    f,
                    "ambiguous query '{query}' — name matches: {}",
                    candidates_list(candidates)
                )
            }
            ResolveError::Io(e) => write!(f, "io error resolving agent id: {e}"),
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResolveError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ResolveError {
    fn from(e: io::Error) -> Self {
        ResolveError::Io(e)
    }
}

fn candidates_list(candidates: &[AgentId]) -> String {
    candidates
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<&str>>()
        .join(", ")
}

/// Enumerate every agent-id directory under `$base/agents/`.
///
/// Returns a sorted `Vec<AgentId>`. Entries whose directory name is not a
/// valid `AgentId` are skipped. Missing `agents_root` → empty vec (NOT an
/// error) so callers can treat an unpopulated state dir uniformly.
pub fn list_agent_ids(state_layout: &StateLayout) -> io::Result<Vec<AgentId>> {
    let root = state_layout.agents_root();
    let read = match fs::read_dir(&root) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids: Vec<AgentId> = Vec::new();
    for entry in read {
        let entry = entry?;
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Ok(id) = AgentId::parse(&name) {
            ids.push(id);
        }
    }
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(ids)
}

/// Minimal projection of `spec.json` — only the `name` field is used by the
/// name-scan tier. Defined separately from `AgentSpec` so a spec file that is
/// missing newer fields still yields a usable name.
#[derive(Deserialize)]
struct SpecNameProjection {
    name: String,
}

/// Read the `name` field out of `$base/agents/{id}/spec.json`. Returns `None`
/// on any error (missing, unreadable, malformed JSON, missing `name` field).
fn read_spec_name(state_layout: &StateLayout, id: &AgentId) -> Option<String> {
    let path = state_layout.spec_path(id);
    let bytes = fs::read(&path).ok()?;
    let proj: SpecNameProjection = serde_json::from_slice(&bytes).ok()?;
    Some(proj.name)
}

/// Resolve `query` to a unique [`AgentId`], or surface an ambiguity/not-found
/// error. See module docs for the tiered matching order.
pub fn resolve_agent_id(query: &str, state_layout: &StateLayout) -> Result<AgentId, ResolveError> {
    // Tier 1: exact AgentId match with existing agent dir.
    if let Ok(id) = AgentId::parse(query) {
        let dir = state_layout.agent_dir(&id);
        if dir.is_dir() {
            return Ok(id);
        }
    }

    // Enumerate all agent ids once; reused by the remaining tiers.
    let ids = list_agent_ids(state_layout)?;

    // Tier 2: prefix match on the AgentId string.
    let prefix_matches: Vec<AgentId> = ids
        .iter()
        .filter(|id| id.as_str().starts_with(query))
        .cloned()
        .collect();
    match prefix_matches.len() {
        0 => {}
        1 => return Ok(prefix_matches.into_iter().next().unwrap()),
        _ => {
            return Err(ResolveError::AmbiguousPrefix {
                query: query.to_string(),
                candidates: prefix_matches,
            });
        }
    }

    // Tier 3: substring match on the AgentId string.
    let substring_matches: Vec<AgentId> = ids
        .iter()
        .filter(|id| id.as_str().contains(query))
        .cloned()
        .collect();
    match substring_matches.len() {
        0 => {}
        1 => return Ok(substring_matches.into_iter().next().unwrap()),
        _ => {
            return Err(ResolveError::AmbiguousSubstring {
                query: query.to_string(),
                candidates: substring_matches,
            });
        }
    }

    // Tier 4: name-field match against spec.json. Try prefix then substring
    // against the `name` field of each readable spec.
    let named: Vec<(AgentId, String)> = ids
        .iter()
        .filter_map(|id| read_spec_name(state_layout, id).map(|n| (id.clone(), n)))
        .collect();

    let name_prefix: Vec<AgentId> = named
        .iter()
        .filter(|(_, n)| n.starts_with(query))
        .map(|(id, _)| id.clone())
        .collect();
    match name_prefix.len() {
        0 => {}
        1 => return Ok(name_prefix.into_iter().next().unwrap()),
        _ => {
            return Err(ResolveError::AmbiguousName {
                query: query.to_string(),
                candidates: name_prefix,
            });
        }
    }

    let name_substring: Vec<AgentId> = named
        .iter()
        .filter(|(_, n)| n.contains(query))
        .map(|(id, _)| id.clone())
        .collect();
    match name_substring.len() {
        0 => {}
        1 => return Ok(name_substring.into_iter().next().unwrap()),
        _ => {
            return Err(ResolveError::AmbiguousName {
                query: query.to_string(),
                candidates: name_substring,
            });
        }
    }

    Err(ResolveError::NotFound {
        query: query.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
    use ulid::Ulid;

    fn layout_with_base(base: PathBuf) -> StateLayout {
        let runtime = base.join("runtime");
        let config = base.join("config");
        StateLayout::new(base, runtime, config)
    }

    /// Make an AgentId with a fixed ULID so tests are deterministic.
    fn id_with(orchestrator: &str, name: &str, ulid: Ulid) -> AgentId {
        AgentId::from_parts(orchestrator, name, ulid)
    }

    fn mkdir(p: &Path) {
        fs::create_dir_all(p).expect("mkdir");
    }

    fn seed_agent_dir(layout: &StateLayout, id: &AgentId) {
        mkdir(&layout.agent_dir(id));
    }

    fn seed_agent_dir_with_spec(layout: &StateLayout, id: &AgentId, name: &str) {
        let dir = layout.agent_dir(id);
        mkdir(&dir);
        let spec = serde_json::json!({ "name": name });
        fs::write(
            layout.spec_path(id),
            serde_json::to_vec_pretty(&spec).expect("serialize"),
        )
        .expect("write spec");
    }

    fn ulid_a() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0123").expect("ulid a")
    }
    fn ulid_b() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0456").expect("ulid b")
    }
    fn ulid_c() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0789").expect("ulid c")
    }

    #[test]
    fn empty_state_dir_returns_not_found() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let err = resolve_agent_id("foo", &layout).expect_err("should not find");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn missing_state_dir_returns_not_found() {
        let tmp = tempdir().expect("tempdir");
        // layout.base() intentionally points at a non-existent subpath.
        let layout = layout_with_base(tmp.path().join("does-not-exist"));
        let err = resolve_agent_id("foo", &layout).expect_err("should not find");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn list_agent_ids_missing_root_returns_empty() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let ids = list_agent_ids(&layout).expect("list");
        assert!(ids.is_empty());
    }

    #[test]
    fn list_agent_ids_skips_invalid_dirnames() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        mkdir(&layout.agents_root());
        // Valid.
        let id = id_with("cavekit", "auth", ulid_a());
        seed_agent_dir(&layout, &id);
        // Invalid dirname (no ulid).
        mkdir(&layout.agents_root().join("not-an-agent-id"));
        // Non-directory file.
        fs::write(layout.agents_root().join("stray.txt"), b"hi").expect("write");

        let ids = list_agent_ids(&layout).expect("list");
        assert_eq!(ids, vec![id]);
    }

    #[test]
    fn exact_agent_id_match_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = id_with("cavekit", "auth", ulid_a());
        seed_agent_dir(&layout, &id);

        let resolved = resolve_agent_id(id.as_str(), &layout).expect("resolve");
        assert_eq!(resolved, id);
    }

    #[test]
    fn exact_agent_id_without_dir_falls_through() {
        // If the user types a full-shape id but no such dir exists, we should
        // still try prefix/substring rather than short-circuiting.
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = id_with("cavekit", "auth", ulid_a());
        seed_agent_dir(&layout, &id);

        // Another, unrelated well-formed id string — no dir for it.
        let phantom = id_with("cavekit", "other", ulid_b());
        let err = resolve_agent_id(phantom.as_str(), &layout).expect_err("no match");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn prefix_match_unique_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = id_with("cavekit", "auth", ulid_a());
        seed_agent_dir(&layout, &id);

        let resolved = resolve_agent_id("cavekit-auth", &layout).expect("resolve");
        assert_eq!(resolved, id);
    }

    #[test]
    fn prefix_match_ambiguous_returns_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = id_with("cavekit", "auth", ulid_a());
        let b = id_with("cavekit", "auth", ulid_b());
        seed_agent_dir(&layout, &a);
        seed_agent_dir(&layout, &b);

        let err = resolve_agent_id("cavekit-auth", &layout).expect_err("ambiguous");
        match err {
            ResolveError::AmbiguousPrefix { query, candidates } => {
                assert_eq!(query, "cavekit-auth");
                assert!(candidates.contains(&a));
                assert!(candidates.contains(&b));
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousPrefix, got {other:?}"),
        }
    }

    #[test]
    fn substring_match_unique_non_prefix_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Only one agent. Query is a substring of id but not a prefix.
        let id = id_with("cavekit", "authsvc", ulid_a());
        seed_agent_dir(&layout, &id);

        let resolved = resolve_agent_id("auth", &layout).expect("resolve");
        assert_eq!(resolved, id);
    }

    #[test]
    fn substring_match_ambiguous_returns_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Two agents that both contain "auth" in their id but where neither
        // id *starts* with "auth" — so tier 2 (prefix) yields 0 and tier 3
        // (substring) sees both.
        let a = id_with("cavekit", "authsvc", ulid_a());
        let b = id_with("claudecode", "reauth", ulid_b());
        seed_agent_dir(&layout, &a);
        seed_agent_dir(&layout, &b);

        let err = resolve_agent_id("auth", &layout).expect_err("ambiguous");
        match err {
            ResolveError::AmbiguousSubstring { query, candidates } => {
                assert_eq!(query, "auth");
                assert!(candidates.contains(&a));
                assert!(candidates.contains(&b));
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousSubstring, got {other:?}"),
        }
    }

    #[test]
    fn name_field_fallback_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Agent dir has an unrelated id but a matching spec.json name.
        let id = id_with("cavekit", "svc", ulid_a());
        seed_agent_dir_with_spec(&layout, &id, "myfeature");

        let resolved = resolve_agent_id("myfeature", &layout).expect("resolve");
        assert_eq!(resolved, id);
    }

    #[test]
    fn name_field_prefix_fallback_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = id_with("cavekit", "svc", ulid_a());
        seed_agent_dir_with_spec(&layout, &id, "myfeature");

        // "myfeat" is a prefix of the name "myfeature" but not of the id.
        let resolved = resolve_agent_id("myfeat", &layout).expect("resolve");
        assert_eq!(resolved, id);
    }

    #[test]
    fn name_field_ambiguous_returns_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Two agents whose ids don't match the query at all — fallback to
        // name-field match must fire, and both have the same name prefix.
        let a = id_with("cavekit", "svca", ulid_a());
        let b = id_with("claudecode", "svcb", ulid_b());
        seed_agent_dir_with_spec(&layout, &a, "myfeature-one");
        seed_agent_dir_with_spec(&layout, &b, "myfeature-two");

        let err = resolve_agent_id("myfeature", &layout).expect_err("ambiguous");
        match err {
            ResolveError::AmbiguousName { query, candidates } => {
                assert_eq!(query, "myfeature");
                assert!(candidates.contains(&a));
                assert!(candidates.contains(&b));
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousName, got {other:?}"),
        }
    }

    #[test]
    fn malformed_spec_json_is_skipped_cleanly() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Agent 1 has a garbage spec.json — name tier must skip it.
        let a = id_with("cavekit", "broken", ulid_a());
        mkdir(&layout.agent_dir(&a));
        fs::write(layout.spec_path(&a), b"{ not valid json").expect("write garbage");
        // Agent 2 has a good spec with a matching name.
        let b = id_with("claudecode", "good", ulid_b());
        seed_agent_dir_with_spec(&layout, &b, "target");

        let resolved = resolve_agent_id("target", &layout).expect("resolve");
        assert_eq!(resolved, b);
    }

    #[test]
    fn missing_spec_json_is_skipped_cleanly() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // Agent 1 has no spec.json at all.
        let a = id_with("cavekit", "nospec", ulid_a());
        seed_agent_dir(&layout, &a);
        // Agent 2 has a spec with matching name.
        let b = id_with("claudecode", "withspec", ulid_b());
        seed_agent_dir_with_spec(&layout, &b, "target");

        let resolved = resolve_agent_id("target", &layout).expect("resolve");
        assert_eq!(resolved, b);
    }

    #[test]
    fn tier_order_prefers_id_prefix_over_name_match() {
        // If both a name-field match and an id-prefix match exist, the id
        // prefix wins — tier 2 (prefix) fires before tier 4 (name).
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        // This agent's id starts with "cavekit".
        let id_hit = id_with("cavekit", "x", ulid_a());
        seed_agent_dir_with_spec(&layout, &id_hit, "some-name");
        // This agent's id does NOT start with "cavekit" but its name does.
        let name_hit = id_with("claudecode", "y", ulid_b());
        seed_agent_dir_with_spec(&layout, &name_hit, "cavekit-like");

        let resolved = resolve_agent_id("cavekit", &layout).expect("resolve");
        assert_eq!(resolved, id_hit);
    }

    #[test]
    fn not_found_when_no_tier_matches() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = id_with("cavekit", "auth", ulid_a());
        seed_agent_dir_with_spec(&layout, &a, "auth");

        let err = resolve_agent_id("completely-unrelated", &layout).expect_err("no match");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn list_agent_ids_sorted() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let b = id_with("cavekit", "bbb", ulid_a());
        let a = id_with("cavekit", "aaa", ulid_b());
        let c = id_with("cavekit", "ccc", ulid_c());
        seed_agent_dir(&layout, &b);
        seed_agent_dir(&layout, &a);
        seed_agent_dir(&layout, &c);

        let ids = list_agent_ids(&layout).expect("list");
        let names: Vec<&str> = ids.iter().map(|i| i.name()).collect();
        assert_eq!(names, vec!["aaa", "bbb", "ccc"]);
    }

    // ---------- Display impl ----------

    #[test]
    fn display_not_found() {
        let e = ResolveError::NotFound {
            query: "foo".to_string(),
        };
        assert_eq!(format!("{e}"), "no agent found matching 'foo'");
    }

    #[test]
    fn display_ambiguous_prefix() {
        let a = id_with("cavekit", "foo", ulid_a());
        let b = id_with("cavekit", "foobar", ulid_b());
        let e = ResolveError::AmbiguousPrefix {
            query: "foo".to_string(),
            candidates: vec![a.clone(), b.clone()],
        };
        let s = format!("{e}");
        assert!(s.starts_with("ambiguous query 'foo' — prefix matches: "));
        assert!(s.contains(a.as_str()));
        assert!(s.contains(b.as_str()));
    }

    #[test]
    fn display_ambiguous_substring() {
        let a = id_with("cavekit", "foo", ulid_a());
        let b = id_with("cavekit", "foobar", ulid_b());
        let e = ResolveError::AmbiguousSubstring {
            query: "foo".to_string(),
            candidates: vec![a.clone(), b.clone()],
        };
        let s = format!("{e}");
        assert!(s.starts_with("ambiguous query 'foo' — substring matches: "));
        assert!(s.contains(a.as_str()));
        assert!(s.contains(b.as_str()));
    }

    #[test]
    fn display_ambiguous_name() {
        let a = id_with("cavekit", "foo", ulid_a());
        let b = id_with("cavekit", "foobar", ulid_b());
        let e = ResolveError::AmbiguousName {
            query: "foo".to_string(),
            candidates: vec![a.clone(), b.clone()],
        };
        let s = format!("{e}");
        assert!(s.starts_with("ambiguous query 'foo' — name matches: "));
        assert!(s.contains(a.as_str()));
        assert!(s.contains(b.as_str()));
    }

    #[test]
    fn display_io_error() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "nope");
        let e = ResolveError::Io(io_err);
        let s = format!("{e}");
        assert!(s.starts_with("io error resolving agent id:"));
    }
}
