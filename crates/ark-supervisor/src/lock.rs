//! Per-agent file lock — cavekit-supervisor R3 step 2 + R5.
//!
//! Each agent gets an advisory flock on `$STATE/locks/{id}.lock`. The
//! lock is held for the lifetime of the supervisor process; releasing
//! the last handle in-process drops the flock AND unlinks the file so
//! `$STATE/locks/` doesn't accumulate stale entries.
//!
//! We use [`fd_lock::RwLock::try_write`] — a non-blocking call that
//! returns immediately with a `WouldBlock` error when another process
//! already holds the lock. We map that to [`LockError::AlreadyLocked`]
//! and expose the pid written into the lock file when available.
//!
//! ## Idempotency
//!
//! `flock(2)` is file-descriptor-scoped — a second open from the same
//! process creates a NEW description and would block even when the
//! caller "already holds" the lock. Since the cavekit spec requires
//! same-process reacquisition to succeed, we layer a small in-process
//! registry above the kernel flock: the first acquire opens the file
//! and takes the flock, subsequent acquires for the same path return
//! an additional [`LockGuard`] that shares the same underlying
//! `Arc<LockInner>`. Only the last handle drop releases the flock and
//! unlinks the file.
//!
//! ## Contention
//!
//! A call from a DIFFERENT process sees `WouldBlock` from
//! `flock(LOCK_EX | LOCK_NB)` — we map that to
//! [`LockError::AlreadyLocked`], reading back the pid stored in the
//! file when available.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use ark_types::{AgentId, StateLayout};
use fd_lock::RwLock;
use thiserror::Error;

/// Holds the acquired lock for the caller.
///
/// Cheap to move; cloneable-within-process via another [`acquire_lock`]
/// call for the same id (which returns a fresh `LockGuard` sharing the
/// same underlying `Arc<LockInner>`). The last `LockGuard` drop in the
/// process releases the advisory flock and unlinks the lock file.
pub struct LockGuard {
    inner: Arc<LockInner>,
}

/// Owns the locked file. Dropping closes the fd (releases flock) and
/// unlinks the lock file from disk.
struct LockInner {
    path: PathBuf,
    // The RwLock owns the File; dropping the RwLock closes the fd,
    // which releases the advisory flock. The field is never read but
    // its Drop side effect is load-bearing.
    #[allow(dead_code)]
    lock: RwLock<File>,
}

impl Drop for LockInner {
    fn drop(&mut self) {
        // Drop the RwLock (and thus close the fd / release flock)
        // BEFORE unlinking so a sibling observer never sees the file
        // gone while still flocked.
        //
        // We can't explicitly drop `self.lock` (can't move out during
        // Drop), but `fd_lock::RwLock` drops the wrapped File on its
        // own Drop, which runs when `self` drops. We unlink after
        // `self` drops — to guarantee ordering we do the unlink here,
        // before the struct Drop completes; Rust drops fields in
        // declaration order after the Drop::drop body returns, which
        // means the File is still open when we unlink. That is fine:
        // unlinking an open file is POSIX-standard and leaves the
        // flock held on the now-anonymous inode until close, which is
        // microseconds later.
        let _ = std::fs::remove_file(&self.path);
        // Also scrub the registry entry so a future acquire doesn't
        // try to Arc::upgrade a dead weak.
        if let Some(reg) = REGISTRY.get() {
            if let Ok(mut map) = reg.lock() {
                if let Some(weak) = map.get(&self.path) {
                    if weak.strong_count() == 0 {
                        map.remove(&self.path);
                    }
                }
            }
        }
    }
}

impl LockGuard {
    /// Path to the lock file on disk (for diagnostics / tests).
    pub fn path(&self) -> &Path {
        &self.inner.path
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

/// Process-wide registry of live locks keyed by canonical path. We use
/// `Weak` so entries die naturally when the last `LockGuard` drops.
static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<LockInner>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<LockInner>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Error)]
pub enum LockError {
    /// Another process holds the lock. `existing_pid` is populated when
    /// we can read a PID from the lock file; `None` when the file is
    /// empty or unparseable.
    #[error("agent lock already held by pid {existing_pid:?}")]
    AlreadyLocked { existing_pid: Option<i32> },

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Acquire (or re-acquire) the file lock for `agent_id`.
///
/// Creates `$STATE/locks/` with mode 0700 if missing, touches the lock
/// file if absent, takes an advisory `LOCK_EX | LOCK_NB` flock on it,
/// and writes the current PID into the file for observability.
///
/// Returns [`LockError::AlreadyLocked`] if another process holds the
/// lock. Returns Ok immediately if the SAME process re-acquires —
/// idempotency is enforced via an in-process registry (see module
/// docs), since flock(2) itself is file-descriptor-scoped.
pub fn acquire_lock(
    state_layout: &StateLayout,
    agent_id: &AgentId,
) -> Result<LockGuard, LockError> {
    let locks_dir = state_layout.locks_dir();
    if !locks_dir.exists() {
        std::fs::create_dir_all(&locks_dir)?;
        // Narrow perms to 0700 on the freshly-created leaf.
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&locks_dir, perms)?;
    }

    let lock_path = state_layout.lock_path(agent_id);

    // --- In-process registry fast path ---------------------------------
    //
    // If a live `LockInner` already exists for this path, hand the
    // caller a fresh `LockGuard` that shares the same Arc. This
    // satisfies the cavekit-spec requirement that a second call from
    // the same process succeeds even though flock(2) would normally
    // block a second fd.
    {
        let reg = registry().lock().expect("lock registry poisoned");
        if let Some(weak) = reg.get(&lock_path) {
            if let Some(inner) = weak.upgrade() {
                return Ok(LockGuard { inner });
            }
        }
    }

    // --- Fresh acquire path --------------------------------------------
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)?;

    let mut rw = RwLock::new(file);

    // Attempt the flock in a sub-scope that forgets the guard on
    // success. After this scope returns the `rw` binding is no longer
    // borrowed and can be moved into `LockInner`.
    //
    // We forget the guard because `fd_lock::RwLockWriteGuard`'s Drop
    // eagerly releases the advisory flock; we want the flock to persist
    // for the lifetime of the returned `LockGuard`. The kernel-level
    // flock is released on fd close, which happens when the RwLock
    // (owned by `LockInner`) drops.
    let acquire_result: Result<(), io::Error> = (|| {
        let mut guard = rw.try_write()?;
        // Overwrite the file with our pid so observers
        // (ark list / ark doctor) can see who claimed it.
        guard.seek(SeekFrom::Start(0))?;
        guard.set_len(0)?;
        let pid = std::process::id();
        writeln!(&mut *guard, "{pid}")?;
        guard.flush()?;
        std::mem::forget(guard);
        Ok(())
    })();

    match acquire_result {
        Ok(()) => {
            let inner = Arc::new(LockInner {
                path: lock_path.clone(),
                lock: rw,
            });
            // Register weak so future in-process acquires share it.
            {
                let mut reg = registry().lock().expect("lock registry poisoned");
                reg.insert(lock_path, Arc::downgrade(&inner));
            }
            Ok(LockGuard { inner })
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            // Another process holds the flock; read back the pid.
            let existing_pid = read_pid_from(&lock_path);
            Err(LockError::AlreadyLocked { existing_pid })
        }
        Err(e) => Err(LockError::Io(e)),
    }
}

fn read_pid_from(path: &std::path::Path) -> Option<i32> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    buf.trim().parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout_at(base: &std::path::Path) -> StateLayout {
        StateLayout::new(base.join("state"), base.join("rt"), base.join("cfg"))
    }

    #[test]
    fn fresh_acquire_creates_lock_file_with_pid() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "fresh");
        let guard = acquire_lock(&layout, &id).expect("acquire");
        let path = guard.path().to_path_buf();
        assert!(path.exists(), "lock file should exist");
        let mut buf = String::new();
        File::open(&path)
            .expect("open lock")
            .read_to_string(&mut buf)
            .expect("read lock");
        let pid: i32 = buf.trim().parse().expect("parse pid");
        assert_eq!(pid as u32, std::process::id());
        drop(guard);
    }

    #[test]
    fn second_acquire_same_process_is_idempotent() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "idem");
        let g1 = acquire_lock(&layout, &id).expect("first acquire");
        let g2 = acquire_lock(&layout, &id).expect("second acquire");
        // Both guards point at the same on-disk path.
        assert_eq!(g1.path(), g2.path());
        drop(g2);
        // First guard still holds — file should still exist on disk.
        assert!(g1.path().exists(), "file should survive first drop");
        drop(g1);
    }

    #[test]
    fn lock_path_nested_in_missing_state_dir_is_created() {
        let tmp = tempdir().expect("tempdir");
        // Note: we deliberately do NOT create tmp/state beforehand.
        let layout = layout_at(tmp.path());
        assert!(!layout.locks_dir().exists(), "precondition");
        let id = AgentId::new("cavekit", "mkdirs");
        let _guard = acquire_lock(&layout, &id).expect("acquire creates dirs");
        assert!(layout.locks_dir().is_dir(), "locks dir created");
        // Sanity: mode 0700 on the newly-created locks_dir.
        let mode = layout
            .locks_dir()
            .metadata()
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn drop_unlinks_lock_file_when_last_handle_released() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_at(tmp.path());
        let id = AgentId::new("cavekit", "dropunlink");
        let guard = acquire_lock(&layout, &id).expect("acquire");
        let path = guard.path().to_path_buf();
        assert!(path.exists(), "file present while held");
        drop(guard);
        assert!(
            !path.exists(),
            "lock file should be unlinked after last handle drop"
        );
    }

    // Cross-process tests live under `tests/subprocess_tests.rs` so that
    // cargo sets `CARGO_BIN_EXE_ark-supervisor-testhelper` and pre-builds
    // the helper binary.
}
