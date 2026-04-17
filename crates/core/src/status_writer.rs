//! Atomic `status.json` writer + reader.
//!
//! Implements cavekit-soul-phase-1-types.md R4 + R6 (atomic publish via
//! temp-file + rename). Readers can consume `status.json` without
//! locking because `rename(2)` is atomic on POSIX — they always see
//! either the previous bytes or the new bytes, never a partial write.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};

use ark_types::{SessionId, SessionStatus, StateLayout};

/// Write `status` to `{layout.session_status_path(id)}` atomically.
///
/// Strategy:
/// 1. Serialize to bytes.
/// 2. Create the session directory (idempotent, 0700).
/// 3. Write to `{status_path}.tmp`. Prefer `create_new(true)` so we
///    detect a stale tmp from a dead writer; if it exists, log a warning
///    and overwrite via `create(true).truncate(true)` — a leftover tmp
///    is not load-bearing.
/// 4. `sync_all()` to flush file contents before publishing the rename.
/// 5. `rename(tmp -> status_path)` — atomic on POSIX. Readers either see
///    the old file or the new file, never a partial write.
pub fn write_session_status_atomic(
    layout: &StateLayout,
    id: &SessionId,
    status: &SessionStatus,
) -> io::Result<()> {
    let session_dir = layout.session_dir(id);
    StateLayout::ensure_dir_0700(&session_dir)?;

    let final_path = layout.session_status_path(id);
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

    fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Read `status.json` for `id`. Returns:
/// - `Ok(None)` if the file does not exist (session never wrote yet).
/// - `Ok(Some(status))` on a successful parse.
/// - `Err(_)` on any other IO failure or parse failure.
pub fn read_status(layout: &StateLayout, id: &SessionId) -> io::Result<Option<SessionStatus>> {
    let path = layout.session_status_path(id);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let status: SessionStatus = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn layout_with_base(base: PathBuf) -> StateLayout {
        let runtime = base.join("runtime");
        let config = base.join("config");
        StateLayout::new(base, runtime, config)
    }

    fn sample_status() -> (SessionId, SessionStatus) {
        let id = SessionId::new("auth");
        let mut ext_state = BTreeMap::new();
        ext_state.insert(
            "claude-code".to_string(),
            serde_json::json!({ "phase": "running" }),
        );
        let status = SessionStatus {
            id: id.clone(),
            started_at: Utc::now(),
            terminated_at: None,
            ext_state,
        };
        (id, status)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        write_session_status_atomic(&layout, &id, &status).expect("write");
        let read = read_status(&layout, &id).expect("read").expect("some");
        assert_eq!(read.id, status.id);
        assert_eq!(read.started_at, status.started_at);
        assert_eq!(read.terminated_at, status.terminated_at);
        assert_eq!(read.ext_state, status.ext_state);
    }

    #[test]
    fn read_missing_returns_ok_none() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("missing");
        let res = read_status(&layout, &id).expect("ok");
        assert!(res.is_none());
    }

    #[test]
    fn successive_writes_produce_only_complete_files() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, mut status) = sample_status();

        write_session_status_atomic(&layout, &id, &status).expect("write1");
        let r1 = read_status(&layout, &id).expect("r1").expect("some1");
        assert!(r1.terminated_at.is_none());

        status.terminated_at = Some(Utc::now());
        write_session_status_atomic(&layout, &id, &status).expect("write2");
        let r2 = read_status(&layout, &id).expect("r2").expect("some2");
        assert!(r2.terminated_at.is_some());

        let tmp_path = layout.session_status_path(&id).with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "tmp file should be renamed away, found {:?}",
            tmp_path
        );
    }

    #[test]
    fn write_overrides_stale_tmp() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        let session_dir = layout.session_dir(&id);
        StateLayout::ensure_dir_0700(&session_dir).unwrap();
        let stale_tmp = layout.session_status_path(&id).with_extension("json.tmp");
        fs::write(&stale_tmp, b"garbage").unwrap();
        assert!(stale_tmp.exists());

        write_session_status_atomic(&layout, &id, &status)
            .expect("write succeeds despite stale tmp");
        let read = read_status(&layout, &id).expect("read").expect("some");
        assert_eq!(read.id, status.id);
        assert!(!stale_tmp.exists(), "stale tmp must be renamed away");
    }

    #[test]
    fn write_creates_missing_session_dir() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        let session_dir = layout.session_dir(&id);
        assert!(!session_dir.exists());

        write_session_status_atomic(&layout, &id, &status).expect("write");

        assert!(session_dir.is_dir());
        assert!(layout.session_status_path(&id).is_file());
    }

    #[test]
    #[cfg(unix)]
    fn write_creates_session_dir_with_mode_0700() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        write_session_status_atomic(&layout, &id, &status).expect("write");

        let mode = layout
            .session_dir(&id)
            .metadata()
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn published_file_is_complete_json() {
        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, status) = sample_status();

        write_session_status_atomic(&layout, &id, &status).expect("write");

        let bytes = fs::read(layout.session_status_path(&id)).expect("read bytes");
        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).expect("file must be valid JSON");
        assert!(parsed.is_object());
        assert!(parsed["id"].is_object());
    }

    #[test]
    fn concurrent_reader_never_sees_partial_write() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let tmp = tempdir().unwrap();
        let layout = layout_with_base(tmp.path().to_path_buf());
        let (id, mut status) = sample_status();

        write_session_status_atomic(&layout, &id, &status).expect("seed");

        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = stop.clone();
        let reader_layout = layout.clone();
        let reader_id = id.clone();

        let reader = std::thread::spawn(move || {
            let mut reads = 0usize;
            while !reader_stop.load(Ordering::Relaxed) {
                match read_status(&reader_layout, &reader_id) {
                    Ok(Some(_)) => reads += 1,
                    Ok(None) => panic!("status vanished mid-burst"),
                    Err(e) => panic!("reader saw partial/invalid bytes: {e}"),
                }
            }
            reads
        });

        for i in 0..200 {
            status.ext_state.insert(
                "claude-code".to_string(),
                serde_json::json!({ "progress": i }),
            );
            write_session_status_atomic(&layout, &id, &status).expect("write");
        }

        stop.store(true, Ordering::Relaxed);
        let reads = reader.join().expect("reader thread");
        assert!(reads > 0);
    }
}
