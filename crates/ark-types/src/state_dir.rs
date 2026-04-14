use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::id::AgentId;

/// On-disk layout for ark state. Resolves XDG base directories with
/// macOS-correct fallbacks. See cavekit-types-state-events.md R5 and
/// cavekit-hook-ipc.md R4 for the runtime-dir macOS note.
///
/// Runtime paths always sit under an `ark-{uid}` segment, either as
/// `$XDG_RUNTIME_DIR/ark-{uid}/` on Linux where `XDG_RUNTIME_DIR` is set, or
/// as `/tmp/ark-{uid}/` on macOS / any host without `XDG_RUNTIME_DIR`.
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
    /// Resolve from environment using XDG conventions.
    ///
    /// - `base`   = `$XDG_STATE_HOME/ark/`  or `$HOME/.local/state/ark/`
    /// - `config` = `$XDG_CONFIG_HOME/ark/` or `$HOME/.config/ark/`
    /// - `runtime` = `$XDG_RUNTIME_DIR/ark-{uid}/` when set (Linux) or
    ///   `/tmp/ark-{uid}/` otherwise (macOS default — `XDG_RUNTIME_DIR` is
    ///   typically unset there; see cavekit-hook-ipc.md R4).
    pub fn from_env() -> Result<Self, StateLayoutError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| StateLayoutError::XdgUnresolvable("HOME not set".to_string()))?;

        let base = xdg_dir("XDG_STATE_HOME", &home, ".local/state").join("ark");
        let config = xdg_dir("XDG_CONFIG_HOME", &home, ".config").join("ark");

        let uid = nix::unistd::Uid::current().as_raw();
        let runtime_leaf = format!("ark-{uid}");
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(v).join(&runtime_leaf),
            _ => PathBuf::from("/tmp").join(&runtime_leaf),
        };

        Ok(Self {
            base,
            runtime,
            config,
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

    /// `$base/agents/{id}/`
    pub fn agent_dir(&self, id: &AgentId) -> PathBuf {
        id.state_dir(&self.base)
    }

    /// `$base/agents/{id}/spec.json`
    pub fn spec_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("spec.json")
    }

    /// `$base/agents/{id}/status.json`
    pub fn status_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("status.json")
    }

    /// `$base/agents/{id}/events.jsonl`
    pub fn events_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("events.jsonl")
    }

    /// `$base/agents/{id}/pid`
    pub fn pid_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("pid")
    }

    /// `$base/agents/{id}/supervisor.log`
    pub fn supervisor_log_path(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("supervisor.log")
    }

    /// `$base/agents/{id}/hooks/`
    pub fn hooks_dir(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("hooks")
    }

    /// `$base/agents/{id}/artifacts/`
    pub fn artifacts_dir(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join("artifacts")
    }

    /// `$base/archive/{YYYY-MM-DD}/{id}/`
    pub fn archive_dir(&self, date: chrono::NaiveDate, id: &AgentId) -> PathBuf {
        self.base
            .join("archive")
            .join(date.format("%Y-%m-%d").to_string())
            .join(id.as_str())
    }

    /// `$base/locks/{id}.lock`
    pub fn lock_path(&self, id: &AgentId) -> PathBuf {
        self.base
            .join("locks")
            .join(format!("{}.lock", id.as_str()))
    }

    /// `$runtime/agents/{id}.sock` — per-supervisor control socket.
    /// See cavekit-hook-ipc.md R4.
    pub fn agent_socket_path(&self, id: &AgentId) -> PathBuf {
        self.runtime
            .join("agents")
            .join(format!("{}.sock", id.as_str()))
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

fn xdg_dir(var: &str, home: &Path, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(fallback),
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
    fn per_agent_paths_match_schema() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let agent = layout.agent_dir(&id);

        assert_eq!(agent, tmp.path().join("agents").join(id.as_str()));
        assert_eq!(layout.spec_path(&id), agent.join("spec.json"));
        assert_eq!(layout.status_path(&id), agent.join("status.json"));
        assert_eq!(layout.events_path(&id), agent.join("events.jsonl"));
        assert_eq!(layout.pid_path(&id), agent.join("pid"));
        assert_eq!(
            layout.supervisor_log_path(&id),
            agent.join("supervisor.log")
        );
        assert_eq!(layout.hooks_dir(&id), agent.join("hooks"));
        assert_eq!(layout.artifacts_dir(&id), agent.join("artifacts"));
    }

    #[test]
    fn archive_path_includes_date_and_id() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 14).expect("date");
        let expected = tmp
            .path()
            .join("archive")
            .join("2026-04-14")
            .join(id.as_str());
        assert_eq!(layout.archive_dir(date, &id), expected);
    }

    #[test]
    fn lock_path_under_locks_dir() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        assert_eq!(
            layout.lock_path(&id),
            tmp.path()
                .join("locks")
                .join(format!("{}.lock", id.as_str()))
        );
    }

    #[test]
    fn agent_socket_path_under_runtime() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_with_base(tmp.path().to_path_buf());
        let id = AgentId::new("cavekit", "auth");
        let expected = tmp
            .path()
            .join("runtime")
            .join("agents")
            .join(format!("{}.sock", id.as_str()));
        assert_eq!(layout.agent_socket_path(&id), expected);
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
