//! Session-ID resolution helper.
//!
//! Given a user-supplied `query` string, resolve it to a unique [`SessionId`]
//! against the on-disk state layout (`$STATE/sessions/*/`).
//!
//! Resolution order (first match wins, ambiguity detected per tier):
//!   1. Exact match: `query` parses as a full `SessionId` AND its session dir exists.
//!   2. Prefix match on the `SessionId` string.
//!   3. Substring match on the `SessionId` string.
//!   4. Name-field match (prefix then substring) against each session's
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

use ark_types::{SessionId, StateLayout};
use serde::Deserialize;

/// Errors returned by [`resolve_session_id`].
#[derive(Debug)]
pub enum ResolveError {
    /// No session matched in any tier.
    NotFound { query: String },
    /// More than one session dirname starts with `query`.
    AmbiguousPrefix {
        query: String,
        candidates: Vec<SessionId>,
    },
    /// More than one session dirname contains `query` (non-prefix tier).
    AmbiguousSubstring {
        query: String,
        candidates: Vec<SessionId>,
    },
    /// More than one session's `spec.json` name matches `query`.
    AmbiguousName {
        query: String,
        candidates: Vec<SessionId>,
    },
    /// I/O error reading the sessions root (other than "not found", which maps
    /// to `NotFound`).
    Io(io::Error),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound { query } => {
                write!(f, "no session found matching '{query}'")
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
            ResolveError::Io(e) => write!(f, "io error resolving session id: {e}"),
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

fn candidates_list(candidates: &[SessionId]) -> String {
    candidates
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<String>>()
        .join(", ")
}

/// Enumerate every session-id directory under `$base/sessions/`.
///
/// Returns a sorted `Vec<SessionId>`. Entries whose directory name is not a
/// valid `SessionId` are skipped. Missing `sessions_root` → empty vec (NOT an
/// error) so callers can treat an unpopulated state dir uniformly.
pub fn list_session_ids(state_layout: &StateLayout) -> io::Result<Vec<SessionId>> {
    let root = state_layout.sessions_root();
    let read = match fs::read_dir(&root) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids: Vec<SessionId> = Vec::new();
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
        if let Ok(id) = SessionId::parse(&name) {
            ids.push(id);
        }
    }
    ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    Ok(ids)
}

/// Minimal projection of `spec.json` — only the `name` field is used by the
/// name-scan tier.
#[derive(Deserialize)]
struct SpecNameProjection {
    name: String,
}

/// Read the `name` field out of `$base/sessions/{id}/spec.json`. Returns `None`
/// on any error (missing, unreadable, malformed JSON, missing `name` field).
fn read_spec_name(state_layout: &StateLayout, id: &SessionId) -> Option<String> {
    let path = state_layout.session_spec_path(id);
    let bytes = fs::read(&path).ok()?;
    let proj: SpecNameProjection = serde_json::from_slice(&bytes).ok()?;
    Some(proj.name)
}

/// Resolve `query` to a unique [`SessionId`], or surface an ambiguity/not-found
/// error. See module docs for the tiered matching order.
pub fn resolve_session_id(
    query: &str,
    state_layout: &StateLayout,
) -> Result<SessionId, ResolveError> {
    // Tier 1: exact SessionId match with existing session dir.
    if let Ok(id) = SessionId::parse(query) {
        let dir = state_layout.session_dir(&id);
        if dir.is_dir() {
            return Ok(id);
        }
    }

    // Enumerate all session ids once; reused by the remaining tiers.
    let ids = list_session_ids(state_layout)?;

    // Tier 2: prefix match on the SessionId string.
    let prefix_matches: Vec<SessionId> = ids
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

    // Tier 3: substring match on the SessionId string.
    let substring_matches: Vec<SessionId> = ids
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
    let named: Vec<(SessionId, String)> = ids
        .iter()
        .filter_map(|id| read_spec_name(state_layout, id).map(|n| (id.clone(), n)))
        .collect();

    let name_prefix: Vec<SessionId> = named
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

    let name_substring: Vec<SessionId> = named
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

    fn layout_with_base(base: PathBuf) -> StateLayout {
        let runtime = base.join("runtime");
        let config = base.join("config");
        StateLayout::new(base, runtime, config)
    }

    fn mkdir(p: &Path) {
        fs::create_dir_all(p).expect("mkdir");
    }

    fn seed_session_dir(layout: &StateLayout, id: &SessionId) {
        mkdir(&layout.session_dir(id));
    }

    fn seed_session_dir_with_spec(layout: &StateLayout, id: &SessionId, name: &str) {
        let dir = layout.session_dir(id);
        mkdir(&dir);
        let spec = serde_json::json!({ "name": name });
        fs::write(
            layout.session_spec_path(id),
            serde_json::to_vec_pretty(&spec).expect("serialize"),
        )
        .expect("write spec");
    }

    #[test]
    fn empty_state_dir_returns_not_found() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let err = resolve_session_id("foo", &layout).expect_err("should not find");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn missing_state_dir_returns_not_found() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().join("does-not-exist"));
        let err = resolve_session_id("foo", &layout).expect_err("should not find");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn list_session_ids_missing_root_returns_empty() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let ids = list_session_ids(&layout).expect("list");
        assert!(ids.is_empty());
    }

    #[test]
    fn list_session_ids_skips_invalid_dirnames() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        mkdir(&layout.sessions_root());
        // Valid — freshly minted SessionId.
        let id = SessionId::new("auth");
        seed_session_dir(&layout, &id);
        // Invalid dirname (no ulid suffix).
        mkdir(&layout.sessions_root().join("not-a-session-id"));
        // Non-directory file.
        fs::write(layout.sessions_root().join("stray.txt"), b"hi").expect("write");

        let ids = list_session_ids(&layout).expect("list");
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].name, id.name);
        assert_eq!(ids[0].ulid, id.ulid);
    }

    #[test]
    fn exact_session_id_match_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        seed_session_dir(&layout, &id);

        let resolved = resolve_session_id(&id.as_str(), &layout).expect("resolve");
        assert_eq!(resolved.as_str(), id.as_str());
    }

    #[test]
    fn exact_session_id_without_dir_falls_through() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        seed_session_dir(&layout, &id);

        let phantom = SessionId::new("other");
        let err = resolve_session_id(&phantom.as_str(), &layout).expect_err("no match");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn prefix_match_unique_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        seed_session_dir(&layout, &id);

        // `auth-` is a prefix of the leaf `auth-<ulid>`.
        let resolved = resolve_session_id("auth-", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), id.as_str());
    }

    #[test]
    fn prefix_match_ambiguous_returns_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = SessionId::new("auth");
        let b = SessionId::new("auth");
        seed_session_dir(&layout, &a);
        seed_session_dir(&layout, &b);

        let err = resolve_session_id("auth-", &layout).expect_err("ambiguous");
        match err {
            ResolveError::AmbiguousPrefix { query, candidates } => {
                assert_eq!(query, "auth-");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected AmbiguousPrefix, got {other:?}"),
        }
    }

    #[test]
    fn substring_match_unique_non_prefix_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("authsvc");
        seed_session_dir(&layout, &id);

        let resolved = resolve_session_id("thsv", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), id.as_str());
    }

    #[test]
    fn name_field_fallback_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("svc");
        seed_session_dir_with_spec(&layout, &id, "myfeature");

        let resolved = resolve_session_id("myfeature", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), id.as_str());
    }

    #[test]
    fn name_field_prefix_fallback_returns_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("svc");
        seed_session_dir_with_spec(&layout, &id, "myfeature");

        let resolved = resolve_session_id("myfeat", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), id.as_str());
    }

    #[test]
    fn malformed_spec_json_is_skipped_cleanly() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = SessionId::new("broken");
        mkdir(&layout.session_dir(&a));
        fs::write(layout.session_spec_path(&a), b"{ not valid json").expect("write garbage");
        let b = SessionId::new("good");
        seed_session_dir_with_spec(&layout, &b, "target");

        let resolved = resolve_session_id("target", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), b.as_str());
    }

    #[test]
    fn missing_spec_json_is_skipped_cleanly() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = SessionId::new("nospec");
        seed_session_dir(&layout, &a);
        let b = SessionId::new("withspec");
        seed_session_dir_with_spec(&layout, &b, "target");

        let resolved = resolve_session_id("target", &layout).expect("resolve");
        assert_eq!(resolved.as_str(), b.as_str());
    }

    #[test]
    fn not_found_when_no_tier_matches() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let a = SessionId::new("auth");
        seed_session_dir_with_spec(&layout, &a, "auth");

        let err = resolve_session_id("completely-unrelated", &layout).expect_err("no match");
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn list_session_ids_sorted() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let b = SessionId::new("bbb");
        let a = SessionId::new("aaa");
        let c = SessionId::new("ccc");
        seed_session_dir(&layout, &b);
        seed_session_dir(&layout, &a);
        seed_session_dir(&layout, &c);

        let ids = list_session_ids(&layout).expect("list");
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "bbb", "ccc"]);
    }

    // ---------- Display impl ----------

    #[test]
    fn display_not_found() {
        let e = ResolveError::NotFound {
            query: "foo".to_string(),
        };
        assert_eq!(format!("{e}"), "no session found matching 'foo'");
    }

    #[test]
    fn display_io_error() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "nope");
        let e = ResolveError::Io(io_err);
        let s = format!("{e}");
        assert!(s.starts_with("io error resolving session id:"));
    }
}
