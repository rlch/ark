//! Atomic `status.json` writer + reader.
//!
//! Implements cavekit-types-state-events.md R6 (atomic publish via
//! temp-file + rename) layered on R5 (state directory schema). Readers can
//! consume `status.json` without locking because `rename(2)` is atomic on
//! POSIX — they always see either the previous bytes or the new bytes,
//! never a partial write.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};

use ark_types::{AgentId, AgentStatus, StateLayout};

/// Write `status` to `{layout.status_path(id)}` atomically.
///
/// Strategy:
/// 1. Serialize to bytes.
/// 2. Create the agent directory (idempotent, 0700).
/// 3. Write to `{status_path}.tmp`. Prefer `create_new(true)` so we detect
///    a stale tmp from a dead writer; if it exists, log a warning and
///    overwrite via `create(true).truncate(true)` — a leftover tmp is not
///    load-bearing.
/// 4. `sync_all()` to flush file contents before publishing the rename.
/// 5. `rename(tmp -> status_path)` — atomic on POSIX. Readers either see
///    the old file or the new file, never a partial write.
pub fn write_status_atomic(
    layout: &StateLayout,
    id: &AgentId,
    status: &AgentStatus,
) -> io::Result<()> {
    let agent_dir = layout.agent_dir(id);
    StateLayout::ensure_dir_0700(&agent_dir)?;

    let final_path = layout.status_path(id);
    let tmp_path = {
        let mut p = final_path.clone();
        let mut name = p
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("status.json"));
        name.push(".tmp");
        p.set_file_name(name);
        p
    };

    let bytes =
        serde_json::to_vec(status).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Try to claim the tmp path exclusively; fall back to overwriting if a
    // stale one exists (a dead writer's tmp is not load-bearing — readers
    // only ever consume the published `status.json`).
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            tracing::warn!(
                path = %tmp_path.display(),
                "stale status.json.tmp from previous writer; overwriting"
            );
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?
        }
        Err(e) => return Err(e),
    };

    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    // Atomic publish.
    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Read `status.json` for `id`. Returns:
/// - `Ok(None)` if the file does not exist (agent never wrote yet).
/// - `Ok(Some(status))` on a successful parse.
/// - `Err(_)` on any other IO failure or parse failure.
pub fn read_status(layout: &StateLayout, id: &AgentId) -> io::Result<Option<AgentStatus>> {
    let path = layout.status_path(id);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let status: AgentStatus = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentSpec, Findings, Phase, Severity, TabHandle};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn layout_with_base(base: PathBuf) -> StateLayout {
        let runtime = base.join("runtime");
        let config = base.join("config");
        StateLayout::new(base, runtime, config)
    }

    fn sample_spec() -> AgentSpec {
        let id = AgentId::new("cavekit", "auth");
        let mut spec = AgentSpec::new(
            id,
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/worktree"),
            vec!["claude".into()],
        );
        spec.env = BTreeMap::new();
        spec
    }

    fn sample_status() -> (AgentId, AgentStatus) {
        let spec = sample_spec();
        let id = spec.id.clone();
        let mut findings = Findings::default();
        findings.record(Severity::P0);
        findings.record(Severity::P2);
        let status = AgentStatus {
            spec,
            phase: Phase::Reviewing,
            progress: Some((3, 10)),
            last_event_at: Utc::now(),
            last_event_summary: "reviewing pr".into(),
            tab_handles: vec![
                TabHandle::new("ark-cavekit-auth", 1, "builder"),
                TabHandle::new("ark-cavekit-auth", 2, "reviewer"),
            ],
            supervisor_pid: 12345,
            stalled_since: Some(Utc::now()),
            findings,
            hide: false,
        };
        (id, status)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        write_status_atomic(&layout, &id, &status).expect("write");
        let read = read_status(&layout, &id).expect("read").expect("some");
        assert_eq!(read, status);
    }

    #[test]
    fn read_missing_returns_ok_none() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = AgentId::new("cavekit", "missing");
        let res = read_status(&layout, &id).expect("ok");
        assert!(res.is_none());
    }

    #[test]
    fn successive_writes_produce_only_complete_files() {
        // Simulate a reader racing two writes: both intermediate states must
        // deserialize cleanly. Because rename is atomic, at any sample point
        // the file is either the previous full bytes or the new full bytes.
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, mut status) = sample_status();

        write_status_atomic(&layout, &id, &status).expect("write1");
        let r1 = read_status(&layout, &id).expect("r1").expect("some1");
        assert_eq!(r1.phase, Phase::Reviewing);

        status.phase = Phase::Done;
        status.last_event_summary = "done".into();
        write_status_atomic(&layout, &id, &status).expect("write2");
        let r2 = read_status(&layout, &id).expect("r2").expect("some2");
        assert_eq!(r2.phase, Phase::Done);
        assert_eq!(r2.last_event_summary, "done");

        // No leftover tmp.
        let tmp_path = layout.status_path(&id).with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file should be renamed away, found {:?}",
            tmp_path
        );
    }

    #[test]
    fn write_overrides_stale_tmp() {
        // If a previous writer crashed and left `status.json.tmp`, a fresh
        // write should still succeed (and not be blocked by the stale tmp).
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        // Pre-create the agent dir and drop a stale tmp file.
        let agent_dir = layout.agent_dir(&id);
        StateLayout::ensure_dir_0700(&agent_dir).unwrap();
        let stale_tmp = layout.status_path(&id).with_extension("json.tmp");
        fs::write(&stale_tmp, b"garbage").unwrap();
        assert!(stale_tmp.exists());

        write_status_atomic(&layout, &id, &status).expect("write succeeds despite stale tmp");
        let read = read_status(&layout, &id).expect("read").expect("some");
        assert_eq!(read, status);
        assert!(!stale_tmp.exists(), "stale tmp must be renamed away");
    }
}
