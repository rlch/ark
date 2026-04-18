use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::id::SessionId;

/// On-disk layout for ark state. Resolves XDG base directories with
/// macOS-correct fallbacks. See cavekit-types-state-events.md R5 and
/// cavekit-hook-ipc.md R4 for the runtime-dir macOS note.
///
/// Runtime path precedence (option D2): `$ARK_RUNTIME_DIR` (verbatim) →
/// `$XDG_RUNTIME_DIR/ark-{uid}` (Linux systemd) → `$TMPDIR/ark` (macOS,
/// no uid since `$TMPDIR` is already per-user) → `/tmp/ark-{uid}`
/// (bare-Linux last resort). See `env_paths.rs` for the full rationale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateLayout {
    base: PathBuf,
    runtime: PathBuf,
    config: PathBuf,
}

#[derive(Debug, Error)]
pub enum StateLayoutError {
    #[error("cannot resolve XDG path: {0}")]
    XdgUnresolvable(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl StateLayout {
    /// Resolve from environment using XDG conventions and ARK_* overrides.
    ///
    /// Thin wrapper around [`crate::env_paths::EnvPaths::resolve`] — that is
    /// the single source of truth for ark path resolution. See its docs for
    /// the full precedence order.
    pub fn from_env() -> Result<Self, StateLayoutError> {
        crate::env_paths::EnvPaths::resolve().map_err(|e| match e {
            crate::env_paths::EnvPathsError::HomeUnset => {
                StateLayoutError::XdgUnresolvable("HOME not set".to_string())
            }
            crate::env_paths::EnvPathsError::InvalidUtf8 => {
                StateLayoutError::XdgUnresolvable("env var not valid utf-8".to_string())
            }
            crate::env_paths::EnvPathsError::Io(e) => StateLayoutError::Io(e),
        })
    }

    /// Explicit constructor for tests and `ARK_STATE_DIR` overrides.
    pub fn new(base: PathBuf, runtime: PathBuf, config: PathBuf) -> Self {
        Self {
            base,
            runtime,
            config,
        }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    /// `$base/sessions/` — root directory that contains one subdirectory per session.
    pub fn sessions_root(&self) -> PathBuf {
        self.base.join("sessions")
    }

    /// `$base/sessions/{id}/`
    pub fn session_dir(&self, id: &SessionId) -> PathBuf {
        self.sessions_root().join(id.as_path_leaf())
    }

    /// `$base/sessions/{id}/spec.json`
    pub fn session_spec_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("spec.json")
    }

    /// `$base/sessions/{id}/status.json`
    pub fn session_status_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("status.json")
    }

    /// `$base/sessions/{id}/events.jsonl`
    pub fn session_events_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("events.jsonl")
    }

    /// `$base/sessions/{id}/pid`
    pub fn session_pid_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("pid")
    }

    /// `$base/sessions/{id}/supervisor.log`
    pub fn session_supervisor_log_path(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("supervisor.log")
    }

    /// `$base/sessions/{id}/hooks/`
    pub fn session_hooks_dir(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("hooks")
    }

    /// `$base/sessions/{id}/artifacts/`
    pub fn session_artifacts_dir(&self, id: &SessionId) -> PathBuf {
        self.session_dir(id).join("artifacts")
    }

    /// `$base/archive/{YYYY-MM-DD}/{id}/`
    pub fn archive_dir(&self, date: chrono::NaiveDate, id: &SessionId) -> PathBuf {
        self.base
            .join("archive")
            .join(date.format("%Y-%m-%d").to_string())
            .join(id.as_path_leaf())
    }

    /// `$base/locks/` — directory that holds `{id}.lock` files.
    pub fn locks_dir(&self) -> PathBuf {
        self.base.join("locks")
    }

    /// `$base/locks/{id}.lock`
    pub fn lock_path(&self, id: &SessionId) -> PathBuf {
        self.locks_dir().join(format!("{}.lock", id.as_path_leaf()))
    }

    /// `$runtime/sessions/{id}.sock` — per-supervisor control socket.
    /// See cavekit-hook-ipc.md R4.
    pub fn session_socket_path(&self, id: &SessionId) -> PathBuf {
        self.runtime
            .join("sessions")
            .join(format!("{}.sock", id.as_path_leaf()))
    }

    /// Idempotently create `path` (and its parents) with mode 0700 on any
    /// freshly-created directory. Already-existing directories keep their
    /// current mode; this never widens permissions on pre-existing paths
    /// but always enforces 0700 on the leaf.
    pub fn ensure_dir_0700(path: &Path) -> io::Result<()> {
        create_dir_all_0700(path)?;
        set_mode_0700(path)?;
        Ok(())
    }
}

/// Walk upward to find the highest missing ancestor, then create each missing
/// segment with mode 0700. Existing directories are untouched.
fn create_dir_all_0700(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    // Find the first ancestor that already exists.
    let mut to_create: Vec<&Path> = Vec::new();
    let mut cursor = path;
    loop {
        if cursor.is_dir() {
            break;
        }
        to_create.push(cursor);
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    // Create from highest missing ancestor down to `path`.
    for dir in to_create.iter().rev() {
        match fs::create_dir(dir) {
            Ok(()) => set_mode_0700(dir)?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn set_mode_0700(path: &Path) -> io::Result<()> {
    let perms = fs::Permissions::from_mode(0o700);
    fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn layout_with_base(base: PathBuf) -> StateLayout {
        let runtime = base.join("runtime");
        let config = base.join("config");
        StateLayout::new(base, runtime, config)
    }

    #[test]
    fn accessors_return_constructed_paths() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().join("state"),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        assert_eq!(layout.base(), tmp.path().join("state"));
        assert_eq!(layout.runtime(), tmp.path().join("rt"));
        assert_eq!(layout.config(), tmp.path().join("cfg"));
    }

    #[test]
    fn sessions_root_is_base_sessions() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        assert_eq!(layout.sessions_root(), tmp.path().join("sessions"));
    }

    #[test]
    fn per_session_paths_match_schema() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        let session = layout.session_dir(&id);

        assert_eq!(session, tmp.path().join("sessions").join(id.as_path_leaf()));
        assert_eq!(layout.session_spec_path(&id), session.join("spec.json"));
        assert_eq!(layout.session_status_path(&id), session.join("status.json"));
        assert_eq!(
            layout.session_events_path(&id),
            session.join("events.jsonl")
        );
        assert_eq!(layout.session_pid_path(&id), session.join("pid"));
        assert_eq!(
            layout.session_supervisor_log_path(&id),
            session.join("supervisor.log")
        );
        assert_eq!(layout.session_hooks_dir(&id), session.join("hooks"));
        assert_eq!(layout.session_artifacts_dir(&id), session.join("artifacts"));
    }

    #[test]
    fn archive_path_includes_date_and_id() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 14).expect("date");
        let expected = tmp
            .path()
            .join("archive")
            .join("2026-04-14")
            .join(id.as_path_leaf());
        assert_eq!(layout.archive_dir(date, &id), expected);
    }

    #[test]
    fn lock_path_under_locks_dir() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        assert_eq!(
            layout.lock_path(&id),
            tmp.path()
                .join("locks")
                .join(format!("{}.lock", id.as_path_leaf()))
        );
    }

    #[test]
    fn locks_dir_is_base_locks() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        assert_eq!(layout.locks_dir(), tmp.path().join("locks"));
    }

    #[test]
    fn session_socket_path_under_runtime() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = SessionId::new("auth");
        let expected = tmp
            .path()
            .join("runtime")
            .join("sessions")
            .join(format!("{}.sock", id.as_path_leaf()));
        assert_eq!(layout.session_socket_path(&id), expected);
    }

    #[test]
    fn ensure_dir_0700_creates_nested_and_sets_mode() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("a").join("b").join("c");
        StateLayout::ensure_dir_0700(&target).expect("ensure");
        assert!(target.is_dir());
        let mode = target.metadata().expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "leaf should be 0700, got {:o}", mode);
    }

    #[test]
    fn ensure_dir_0700_is_idempotent() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("idem");
        StateLayout::ensure_dir_0700(&target).expect("first");
        StateLayout::ensure_dir_0700(&target).expect("second");
        let mode = target.metadata().expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn ensure_dir_0700_enforces_mode_on_existing_dir() {
        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("existing");
        fs::create_dir(&target).expect("create");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("chmod");
        StateLayout::ensure_dir_0700(&target).expect("ensure");
        let mode = target.metadata().expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn macos_fallback_uses_tmp_when_xdg_runtime_unset() {
        // Explicit constructor path — avoids touching process env from tests.
        let uid = nix::unistd::Uid::current().as_raw();
        let runtime = PathBuf::from(format!("/tmp/ark-{uid}"));
        let layout = StateLayout::new(
            PathBuf::from("/state"),
            runtime.clone(),
            PathBuf::from("/cfg"),
        );
        assert_eq!(layout.runtime(), runtime);
        assert!(layout.runtime().to_string_lossy().starts_with("/tmp/ark-"));
    }
}
