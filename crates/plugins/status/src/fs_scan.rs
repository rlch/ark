//! Host-testable filesystem fallback scanning.
//!
//! Implements cavekit-plugin-status R4: on each 1 Hz timer tick, enumerate
//! `$XDG_STATE_HOME/ark/agents/*/status.json` and merge any parseable entries
//! into the plugin's cache using the same newer-wins upsert logic the pipe
//! path uses. The scan is best-effort — unreadable dirs, malformed JSON, and
//! missing env vars are all silently swallowed. This module is deliberately
//! std-only (no zellij-tile) so host tests can drive it with real tempdirs
//! under `cargo test -p ark-plugin-status` without touching wasm.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::StatusSummary;

/// Resolve the state directory root that contains `agents/` subdirs.
///
/// Per XDG base-dir spec: prefer `$XDG_STATE_HOME` if set, otherwise
/// `$HOME/.local/state`. In both cases we append `ark/` so callers land on
/// the ark-owned subtree directly. When neither env var is set (the env
/// injector returns `None`) we return `PathBuf::new()` — callers detect the
/// empty path and skip scanning, matching "best effort" semantics.
///
/// `env` is injected rather than read via `std::env::var` so host tests can
/// assert both the XDG-set and fallback branches without mutating the
/// process environment.
pub fn resolve_state_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(xdg) = env("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(xdg).join("ark");
    }
    if let Some(home) = env("HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(home).join(".local").join("state").join("ark");
    }
    PathBuf::new()
}

/// Enumerate `<state_dir>/agents/<id>/status.json` and return the parseable
/// entries.
///
/// Semantics (all "best effort" per R4):
/// - Missing `agents/` dir → empty Vec.
/// - Unreadable subdir or `status.json` → skipped silently.
/// - Malformed JSON → skipped silently.
/// - `agent_id` defaulted to empty → entry kept, matching what
///   [`crate::ingest_pipe_payload`] does (the caller enforces non-empty on
///   merge).
///
/// Deterministic ordering is not guaranteed — callers (`merge_fs_scan`) key
/// by `agent_id` so insertion order does not affect outcomes.
pub fn scan_state_dir(state_dir: &Path) -> Vec<StatusSummary> {
    let agents_root = state_dir.join("agents");
    let Ok(read_dir) = fs::read_dir(&agents_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        // Only descend into directories; skip stray files under agents/.
        if !path.is_dir() {
            continue;
        }
        let status_json = path.join("status.json");
        let Ok(contents) = fs::read_to_string(&status_json) else {
            continue;
        };
        let Ok(summary) = serde_json::from_str::<StatusSummary>(&contents) else {
            continue;
        };
        out.push(summary);
    }
    out
}

/// Merge a vec of filesystem-sourced summaries into the pipe cache using
/// newer-wins semantics on `updated_at`.
///
/// Mirrors [`crate::ingest_pipe_payload`]'s upsert but with an extra
/// freshness guard: if the cache already has a newer entry (e.g. a pipe
/// message landed this same tick), we leave it alone. Empty-`agent_id`
/// entries are dropped — consistent with the pipe path's
/// `IngestError::MissingAgentId` rejection.
///
/// Returns `true` if the cache changed (added or updated any entry), so the
/// caller can decide whether to request a redraw.
pub fn merge_fs_scan(
    cache: &mut BTreeMap<String, StatusSummary>,
    scanned: Vec<StatusSummary>,
) -> bool {
    let mut changed = false;
    for summary in scanned {
        if summary.agent_id.is_empty() {
            continue;
        }
        match cache.get(&summary.agent_id) {
            Some(existing) if existing.updated_at >= summary.updated_at => {
                // Cache already has same-or-newer — skip.
            }
            _ => {
                cache.insert(summary.agent_id.clone(), summary);
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_status(dir: &Path, agent_id: &str, updated_at: u64, phase: &str) {
        let agent_dir = dir.join("agents").join(agent_id);
        fs::create_dir_all(&agent_dir).unwrap();
        let summary = StatusSummary {
            agent_id: agent_id.to_string(),
            name: agent_id.to_string(),
            orchestrator: "cavekit".to_string(),
            phase: phase.to_string(),
            updated_at,
            last_event: String::new(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        fs::write(agent_dir.join("status.json"), json).unwrap();
    }

    /// Minimal tempdir helper — avoids pulling a `tempfile` dev-dep just for
    /// this module. Uses a per-test counter + PID to keep collisions off and
    /// cleans up on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "ark-status-fs-scan-{}-{}-{}",
                tag,
                std::process::id(),
                n
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_state_dir_returns_all_valid_entries() {
        let tmp = TempDir::new("valid");
        write_status(tmp.path(), "agent-1", 1_000, "running");
        write_status(tmp.path(), "agent-2", 2_000, "idle");
        write_status(tmp.path(), "agent-3", 3_000, "done");

        let mut results = scan_state_dir(tmp.path());
        results.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].agent_id, "agent-1");
        assert_eq!(results[1].updated_at, 2_000);
        assert_eq!(results[2].phase, "done");
    }

    #[test]
    fn scan_state_dir_skips_malformed_json_keeps_valid() {
        let tmp = TempDir::new("malformed");
        write_status(tmp.path(), "good", 1_000, "running");
        // Malformed entry: create dir + garbage status.json.
        let bad = tmp.path().join("agents").join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("status.json"), "{ not valid json ").unwrap();

        let results = scan_state_dir(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id, "good");
    }

    #[test]
    fn scan_state_dir_skips_subdir_without_status_json() {
        let tmp = TempDir::new("no-status");
        write_status(tmp.path(), "good", 1_000, "running");
        // Subdir exists but no status.json — simulates a half-initialised
        // agent state dir; treated as "unreadable" per R4.
        fs::create_dir_all(tmp.path().join("agents").join("empty-dir")).unwrap();

        let results = scan_state_dir(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id, "good");
    }

    #[test]
    fn scan_state_dir_empty_tempdir_returns_empty() {
        let tmp = TempDir::new("empty");
        // No agents/ subdir at all.
        let results = scan_state_dir(tmp.path());
        assert!(results.is_empty());
    }

    #[test]
    fn scan_state_dir_empty_agents_dir_returns_empty() {
        let tmp = TempDir::new("empty-agents");
        fs::create_dir_all(tmp.path().join("agents")).unwrap();
        let results = scan_state_dir(tmp.path());
        assert!(results.is_empty());
    }

    #[test]
    fn resolve_state_dir_uses_xdg_when_set() {
        let env = |k: &str| match k {
            "XDG_STATE_HOME" => Some("/custom/state".to_string()),
            "HOME" => Some("/home/user".to_string()),
            _ => None,
        };
        assert_eq!(resolve_state_dir(env), PathBuf::from("/custom/state/ark"));
    }

    #[test]
    fn resolve_state_dir_falls_back_to_home_local_state() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/user".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_state_dir(env),
            PathBuf::from("/home/user/.local/state/ark")
        );
    }

    #[test]
    fn resolve_state_dir_treats_empty_xdg_as_unset() {
        // Some shells export XDG_STATE_HOME="" rather than unsetting it;
        // treat the empty string as "not set" so we fall through to $HOME.
        let env = |k: &str| match k {
            "XDG_STATE_HOME" => Some(String::new()),
            "HOME" => Some("/home/user".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_state_dir(env),
            PathBuf::from("/home/user/.local/state/ark")
        );
    }

    #[test]
    fn resolve_state_dir_returns_empty_when_no_env() {
        let env = |_: &str| None;
        assert_eq!(resolve_state_dir(env), PathBuf::new());
    }

    #[test]
    fn merge_fs_scan_inserts_new_entries() {
        let mut cache = BTreeMap::new();
        let scanned = vec![StatusSummary {
            agent_id: "a".into(),
            updated_at: 1_000,
            phase: "running".into(),
            ..Default::default()
        }];
        let changed = merge_fs_scan(&mut cache, scanned);
        assert!(changed);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache["a"].phase, "running");
    }

    #[test]
    fn merge_fs_scan_newer_fs_wins_over_older_pipe() {
        // Pipe-set entry first, then FS scan brings newer snapshot → FS wins.
        let mut cache = BTreeMap::new();
        cache.insert(
            "a".into(),
            StatusSummary {
                agent_id: "a".into(),
                updated_at: 1_000,
                phase: "idle".into(),
                ..Default::default()
            },
        );
        let changed = merge_fs_scan(
            &mut cache,
            vec![StatusSummary {
                agent_id: "a".into(),
                updated_at: 2_000,
                phase: "running".into(),
                ..Default::default()
            }],
        );
        assert!(changed);
        assert_eq!(cache["a"].updated_at, 2_000);
        assert_eq!(cache["a"].phase, "running");
    }

    #[test]
    fn merge_fs_scan_older_fs_loses_to_newer_pipe() {
        // Pipe-set entry with a newer timestamp must not be clobbered by a
        // stale on-disk status.json (e.g. agent just pushed an update via
        // pipe before the 1 Hz scan ran).
        let mut cache = BTreeMap::new();
        cache.insert(
            "a".into(),
            StatusSummary {
                agent_id: "a".into(),
                updated_at: 5_000,
                phase: "running".into(),
                ..Default::default()
            },
        );
        let changed = merge_fs_scan(
            &mut cache,
            vec![StatusSummary {
                agent_id: "a".into(),
                updated_at: 1_000,
                phase: "idle".into(),
                ..Default::default()
            }],
        );
        assert!(!changed);
        assert_eq!(cache["a"].updated_at, 5_000);
        assert_eq!(cache["a"].phase, "running");
    }

    #[test]
    fn merge_fs_scan_skips_empty_agent_id() {
        let mut cache = BTreeMap::new();
        let scanned = vec![StatusSummary {
            agent_id: String::new(),
            updated_at: 1_000,
            ..Default::default()
        }];
        let changed = merge_fs_scan(&mut cache, scanned);
        assert!(!changed);
        assert!(cache.is_empty());
    }
}
