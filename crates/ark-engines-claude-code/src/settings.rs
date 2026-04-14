//! `.claude/settings.local.json` hook injection (cavekit-engine-claude-code R1).
//!
//! Manages the worktree-local Claude Code settings file:
//!
//! - locates or creates `{cwd}/.claude/settings.local.json`,
//! - creates `.claude/` with mode `0700` if missing,
//! - deep-merges an injected `hooks` block on top of any pre-existing
//!   settings (without clobbering unrelated keys),
//! - tags the injection with a `_ark_marker` + `_ark_checksum` so it can
//!   be detected on subsequent runs and is idempotent,
//! - backs the original file up to `settings.local.json.ark-backup`
//!   exactly once (first install wins; subsequent installs reuse the
//!   existing backup),
//! - exposes [`restore_settings`] so the engine handle teardown (T-057)
//!   can restore the pre-install state.
//!
//! ## Deep-merge algorithm
//!
//! [`deep_merge`] walks the destination JSON value and recursively merges
//! the source value on top:
//!
//! - if both sides are JSON objects, the merge recurses key-by-key;
//! - otherwise (arrays, strings, numbers, booleans, nulls) the source
//!   value **replaces** the destination value at that key path.
//!
//! That means injected hook arrays replace any pre-existing array at the
//! same hook-event key, while sibling keys (`permissions`, `model`, …)
//! are preserved verbatim.
//!
//! ## Marker shape
//!
//! Two top-level keys are added inside the injected `hooks` block:
//!
//! ```json
//! {
//!   "_ark_marker": "injected by ark v0.1.0",
//!   "_ark_checksum": "sha256:…"
//! }
//! ```
//!
//! The checksum covers the canonical JSON serialization of the injected
//! hook events plus the `AgentId` plus the ark version, so re-running
//! [`inject_hooks`] with the same agent/events/version is a no-op, while
//! upgrades or event-list changes force a re-injection.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ark_types::AgentId;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SETTINGS_FILE: &str = "settings.local.json";
const BACKUP_FILE: &str = "settings.local.json.ark-backup";
const CLAUDE_DIR: &str = ".claude";
const MARKER_KEY: &str = "_ark_marker";
const CHECKSUM_KEY: &str = "_ark_checksum";

/// Outcome of a successful [`inject_hooks`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectReport {
    pub action: InjectAction,
    pub checksum: String,
}

/// Whether [`inject_hooks`] wrote new settings or skipped because the
/// existing file already carried the same checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectAction {
    Injected,
    SkippedIdempotent,
}

#[derive(Debug, Error)]
pub enum InjectError {
    #[error("failed to create .claude directory at {path:?}: {source}")]
    CreateClaudeDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to set 0700 perms on {path:?}: {source}")]
    SetClaudeDirPerms {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read {path:?}: {source}")]
    ReadSettings {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse existing settings JSON at {path:?}: {source}")]
    ParseSettings {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("settings root must be a JSON object at {path:?}")]
    NotAnObject { path: PathBuf },
    #[error("failed to write {path:?}: {source}")]
    WriteSettings {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write backup {path:?}: {source}")]
    WriteBackup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum RestoreError {
    #[error("failed to read backup {path:?}: {source}")]
    ReadBackup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write {path:?}: {source}")]
    WriteSettings {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to remove {path:?}: {source}")]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Inject ark-hook entries into `{cwd}/.claude/settings.local.json`.
///
/// See module docs for the merge algorithm, marker shape, and idempotency
/// rules. Idempotent on re-runs with the same `agent_id` / `events` /
/// `ark_version`.
pub fn inject_hooks(
    cwd: &Path,
    agent_id: &AgentId,
    events: &[&str],
    ark_version: &str,
) -> Result<InjectReport, InjectError> {
    let claude_dir = cwd.join(CLAUDE_DIR);
    ensure_claude_dir(&claude_dir)?;

    let settings_path = claude_dir.join(SETTINGS_FILE);
    let backup_path = claude_dir.join(BACKUP_FILE);

    let (existing_value, settings_existed) = read_existing(&settings_path)?;
    let checksum = compute_checksum(agent_id, events, ark_version);
    let stored_marker = format!("sha256:{checksum}");

    // Idempotency: existing checksum on the live file matches → no-op.
    if existing_value
        .as_ref()
        .and_then(|v| v.get("hooks"))
        .and_then(|hooks| hooks.get(CHECKSUM_KEY))
        .and_then(|c| c.as_str())
        == Some(stored_marker.as_str())
    {
        return Ok(InjectReport {
            action: InjectAction::SkippedIdempotent,
            checksum,
        });
    }

    // Backup once: only when an original file existed AND no backup yet.
    // First install wins — subsequent re-injections (e.g. version bump)
    // do NOT overwrite the backup.
    if settings_existed && !backup_path.exists() {
        let raw = fs::read(&settings_path).map_err(|source| InjectError::ReadSettings {
            path: settings_path.clone(),
            source,
        })?;
        fs::write(&backup_path, &raw).map_err(|source| InjectError::WriteBackup {
            path: backup_path.clone(),
            source,
        })?;
    }

    // Strip any prior ark-injected marker/checksum/hook entries from the
    // existing hooks object before re-merging — otherwise stale events
    // from a previous version would linger.
    let mut base = existing_value.unwrap_or_else(|| Value::Object(Map::new()));
    if !base.is_object() {
        return Err(InjectError::NotAnObject {
            path: settings_path,
        });
    }
    if let Some(hooks) = base.get_mut("hooks").and_then(Value::as_object_mut) {
        hooks.remove(MARKER_KEY);
        hooks.remove(CHECKSUM_KEY);
        for ev in events {
            hooks.remove(*ev);
        }
    }

    let injection = build_injection(agent_id, events, ark_version, &checksum);
    deep_merge(&mut base, injection);

    let serialized = serde_json::to_vec_pretty(&base).expect("serialize JSON value never fails");
    fs::write(&settings_path, &serialized).map_err(|source| InjectError::WriteSettings {
        path: settings_path.clone(),
        source,
    })?;

    Ok(InjectReport {
        action: InjectAction::Injected,
        checksum,
    })
}

/// Restore `.claude/settings.local.json` from the `.ark-backup` companion.
///
/// - If the backup exists: copy it back over the live settings file and
///   delete the backup.
/// - If the backup does not exist but the live settings file does: this
///   means the install ran on a fresh cwd (no prior settings to backup) —
///   remove the live settings file so the cwd returns to its original
///   pristine state.
/// - If neither exists: no-op.
pub fn restore_settings(cwd: &Path) -> Result<(), RestoreError> {
    let claude_dir = cwd.join(CLAUDE_DIR);
    let settings_path = claude_dir.join(SETTINGS_FILE);
    let backup_path = claude_dir.join(BACKUP_FILE);

    if backup_path.exists() {
        let raw = fs::read(&backup_path).map_err(|source| RestoreError::ReadBackup {
            path: backup_path.clone(),
            source,
        })?;
        fs::write(&settings_path, &raw).map_err(|source| RestoreError::WriteSettings {
            path: settings_path.clone(),
            source,
        })?;
        fs::remove_file(&backup_path).map_err(|source| RestoreError::RemoveFile {
            path: backup_path.clone(),
            source,
        })?;
    } else if settings_path.exists() {
        fs::remove_file(&settings_path).map_err(|source| RestoreError::RemoveFile {
            path: settings_path.clone(),
            source,
        })?;
    }

    Ok(())
}

fn ensure_claude_dir(claude_dir: &Path) -> Result<(), InjectError> {
    if !claude_dir.exists() {
        fs::create_dir_all(claude_dir).map_err(|source| InjectError::CreateClaudeDir {
            path: claude_dir.to_path_buf(),
            source,
        })?;
    }
    let perms = fs::Permissions::from_mode(0o700);
    fs::set_permissions(claude_dir, perms).map_err(|source| InjectError::SetClaudeDirPerms {
        path: claude_dir.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn read_existing(path: &Path) -> Result<(Option<Value>, bool), InjectError> {
    if !path.exists() {
        return Ok((None, false));
    }
    let raw = fs::read(path).map_err(|source| InjectError::ReadSettings {
        path: path.to_path_buf(),
        source,
    })?;
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok((None, true));
    }
    let value: Value =
        serde_json::from_slice(&raw).map_err(|source| InjectError::ParseSettings {
            path: path.to_path_buf(),
            source,
        })?;
    if !value.is_object() {
        return Err(InjectError::NotAnObject {
            path: path.to_path_buf(),
        });
    }
    Ok((Some(value), true))
}

fn build_injection(
    agent_id: &AgentId,
    events: &[&str],
    ark_version: &str,
    checksum: &str,
) -> Value {
    let mut hooks = Map::new();
    hooks.insert(
        MARKER_KEY.to_string(),
        Value::String(format!("injected by ark v{ark_version}")),
    );
    hooks.insert(
        CHECKSUM_KEY.to_string(),
        Value::String(format!("sha256:{checksum}")),
    );
    for ev in events {
        let cmd = format!("ark-hook --id {} --event {}", agent_id.as_str(), ev);
        hooks.insert(
            (*ev).to_string(),
            Value::Array(vec![json!({ "command": cmd })]),
        );
    }
    Value::Object({
        let mut m = Map::new();
        m.insert("hooks".to_string(), Value::Object(hooks));
        m
    })
}

/// Recursively merge `src` into `dst`.
///
/// - object + object → key-wise recurse,
/// - anything else → `src` replaces `dst`.
pub(crate) fn deep_merge(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                match d.get_mut(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        d.insert(k, v);
                    }
                }
            }
        }
        (slot, src) => {
            *slot = src;
        }
    }
}

/// SHA-256 of (sorted event list, agent id, ark version), formatted hex.
fn compute_checksum(agent_id: &AgentId, events: &[&str], ark_version: &str) -> String {
    // Use a BTreeMap to give the inputs a deterministic order.
    let mut sorted_events: Vec<&str> = events.iter().copied().collect();
    sorted_events.sort_unstable();
    sorted_events.dedup();
    let canonical: BTreeMap<&str, Value> = BTreeMap::from([
        ("agent_id", Value::String(agent_id.as_str().to_string())),
        ("ark_version", Value::String(ark_version.to_string())),
        (
            "events",
            Value::Array(
                sorted_events
                    .iter()
                    .map(|e| Value::String((*e).to_string()))
                    .collect(),
            ),
        ),
    ]);
    let bytes = serde_json::to_vec(&canonical).expect("serialize BTreeMap");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    fn fake_agent_id() -> AgentId {
        AgentId::parse("cavekit-auth-01jx7z8k6x9y2zt4abcdef0123").expect("parse fixture")
    }

    const EVENTS: &[&str] = &[
        "PostToolUse",
        "Stop",
        "PermissionRequest",
        "Notification",
        "TaskCompleted",
        "SessionEnd",
    ];

    fn read_json(path: &Path) -> Value {
        let raw = fs::read(path).expect("read");
        serde_json::from_slice(&raw).expect("parse JSON")
    }

    #[test]
    fn fresh_cwd_creates_dir_0700_and_writes_hooks() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let id = fake_agent_id();

        let report = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("inject");
        assert_eq!(report.action, InjectAction::Injected);

        let claude_dir = cwd.join(".claude");
        let mode = fs::metadata(&claude_dir).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o700, "claude dir must be 0700");

        let settings = read_json(&claude_dir.join(SETTINGS_FILE));
        let hooks = settings.get("hooks").expect("hooks key");
        for ev in EVENTS {
            let arr = hooks.get(*ev).expect("event").as_array().expect("array");
            assert_eq!(arr.len(), 1);
            let cmd = arr[0].get("command").and_then(Value::as_str).unwrap();
            assert_eq!(cmd, format!("ark-hook --id {} --event {}", id.as_str(), ev),);
        }
        assert!(hooks.get(MARKER_KEY).is_some());
        assert_eq!(
            hooks.get(CHECKSUM_KEY).and_then(Value::as_str),
            Some(format!("sha256:{}", report.checksum).as_str()),
        );
    }

    #[test]
    fn deep_merge_preserves_unrelated_keys() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let pre = json!({
            "permissions": { "allow": ["Read", "Write"] },
            "model": "claude-opus",
            "hooks": {
                "OtherEvent": [{ "command": "user-defined" }]
            }
        });
        fs::write(
            claude_dir.join(SETTINGS_FILE),
            serde_json::to_vec_pretty(&pre).unwrap(),
        )
        .unwrap();

        let id = fake_agent_id();
        inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("inject");

        let settings = read_json(&claude_dir.join(SETTINGS_FILE));
        assert_eq!(
            settings.get("permissions").and_then(|v| v.get("allow")),
            Some(&json!(["Read", "Write"])),
        );
        assert_eq!(
            settings.get("model").and_then(Value::as_str),
            Some("claude-opus"),
        );
        let hooks = settings.get("hooks").expect("hooks");
        assert!(
            hooks.get("OtherEvent").is_some(),
            "user-defined hook entry must be preserved",
        );
        for ev in EVENTS {
            assert!(hooks.get(*ev).is_some(), "ark hook {ev} must be present");
        }
    }

    #[test]
    fn idempotent_when_checksum_matches() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let id = fake_agent_id();

        let first = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("first");
        assert_eq!(first.action, InjectAction::Injected);

        let second = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("second");
        assert_eq!(second.action, InjectAction::SkippedIdempotent);
        assert_eq!(first.checksum, second.checksum);
    }

    #[test]
    fn re_injects_when_events_change() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let id = fake_agent_id();

        let first = inject_hooks(cwd, &id, &["Stop"], "0.1.0").expect("first");
        assert_eq!(first.action, InjectAction::Injected);

        let second = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("second");
        assert_eq!(second.action, InjectAction::Injected);
        assert_ne!(first.checksum, second.checksum);

        let claude_dir = cwd.join(".claude");
        let settings = read_json(&claude_dir.join(SETTINGS_FILE));
        let hooks = settings.get("hooks").expect("hooks");
        for ev in EVENTS {
            assert!(
                hooks.get(*ev).is_some(),
                "{ev} must be present after re-inject"
            );
        }
    }

    #[test]
    fn re_injects_when_ark_version_changes() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let id = fake_agent_id();

        let first = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("first");
        let second = inject_hooks(cwd, &id, EVENTS, "0.2.0").expect("second");
        assert_eq!(first.action, InjectAction::Injected);
        assert_eq!(second.action, InjectAction::Injected);
        assert_ne!(first.checksum, second.checksum);
    }

    #[test]
    fn backup_created_once_and_not_overwritten() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let original = json!({ "permissions": { "allow": ["Read"] } });
        let original_bytes = serde_json::to_vec_pretty(&original).unwrap();
        fs::write(claude_dir.join(SETTINGS_FILE), &original_bytes).unwrap();

        let id = fake_agent_id();
        inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("first inject");

        let backup_after_first = fs::read(claude_dir.join(BACKUP_FILE)).expect("backup exists");
        assert_eq!(backup_after_first, original_bytes);

        // Re-inject with a *different* version → forces re-write of the
        // settings file, but backup must NOT be touched.
        inject_hooks(cwd, &id, EVENTS, "0.9.9").expect("second inject");

        let backup_after_second = fs::read(claude_dir.join(BACKUP_FILE)).expect("backup still");
        assert_eq!(
            backup_after_second, original_bytes,
            "backup must reflect the original first-install settings",
        );
    }

    #[test]
    fn restore_round_trip_with_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let original = json!({
            "permissions": { "allow": ["Read", "Write"] },
            "hooks": { "OtherEvent": [{ "command": "user-defined" }] }
        });
        let original_bytes = serde_json::to_vec_pretty(&original).unwrap();
        fs::write(claude_dir.join(SETTINGS_FILE), &original_bytes).unwrap();

        let id = fake_agent_id();
        inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("inject");
        restore_settings(cwd).expect("restore");

        let restored = fs::read(claude_dir.join(SETTINGS_FILE)).expect("settings restored");
        assert_eq!(restored, original_bytes);
        assert!(
            !claude_dir.join(BACKUP_FILE).exists(),
            "backup must be removed on restore",
        );
    }

    #[test]
    fn restore_round_trip_with_no_pre_existing_settings() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let id = fake_agent_id();

        inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("inject");
        let claude_dir = cwd.join(".claude");
        assert!(claude_dir.join(SETTINGS_FILE).exists());
        assert!(
            !claude_dir.join(BACKUP_FILE).exists(),
            "no backup should exist when no original settings existed",
        );

        restore_settings(cwd).expect("restore");
        assert!(
            !claude_dir.join(SETTINGS_FILE).exists(),
            "fresh-cwd restore should remove the injected settings file",
        );
    }

    #[test]
    fn restore_is_noop_when_neither_exists() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        // Don't even create .claude/.
        restore_settings(cwd).expect("noop");
    }

    #[test]
    fn deep_merge_replaces_arrays_and_scalars() {
        let mut dst = json!({ "a": [1, 2, 3], "b": "old", "nested": { "k": "v" } });
        let src = json!({ "a": [9], "b": "new", "nested": { "j": "w" } });
        deep_merge(&mut dst, src);
        assert_eq!(
            dst,
            json!({
                "a": [9],
                "b": "new",
                "nested": { "k": "v", "j": "w" }
            })
        );
    }

    #[test]
    fn rejects_non_object_root() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join(SETTINGS_FILE), b"[1, 2, 3]").unwrap();

        let id = fake_agent_id();
        let err = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect_err("non-object");
        assert!(matches!(err, InjectError::NotAnObject { .. }));
    }

    #[test]
    fn empty_settings_file_treated_as_fresh() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        let claude_dir = cwd.join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(claude_dir.join(SETTINGS_FILE), b"   \n").unwrap();

        let id = fake_agent_id();
        let report = inject_hooks(cwd, &id, EVENTS, "0.1.0").expect("inject");
        assert_eq!(report.action, InjectAction::Injected);
    }
}
