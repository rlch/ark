//! Per-user agent socket directory + path helpers
//! (cavekit-hook-ipc.md R4, cavekit-supervisor.md R7).
//!
//! The kakoune-model layout:
//!   - When `XDG_RUNTIME_DIR` is set (Linux typical):
//!     `{XDG_RUNTIME_DIR}/ark-{uid}/agents/{id}.sock`
//!   - When unset (macOS default): `/tmp/ark-{uid}/agents/{id}.sock`
//!
//! `EnvPaths::agent_socket_path` in `ark-types` is the canonical resolver.
//! [`runtime_root`] in this module is a lightweight convenience for code paths
//! that don't want to carry a full [`StateLayout`]. They must agree.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ark_types::AgentId;
use nix::unistd::Uid;

/// Ensure the per-user agents socket directory exists with mode 0700.
///
/// Caller supplies the pre-suffixed `ark-{uid}/` root (from
/// [`ark_types::EnvPaths::resolve`] → `runtime()`). This function owns only
/// the `/agents` leaf — both the leaf and the parent are tightened to mode
/// 0700 (idempotent, safe under concurrent invocation).
///
/// Returns the `/agents` directory path.
pub fn ensure_agents_dir(runtime_dir_root: &Path) -> std::io::Result<PathBuf> {
    let agents = runtime_dir_root.join("agents");
    fs::create_dir_all(&agents)?;
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o700))?;
    // Tighten parent too — `create_dir_all` may have created it with the
    // ambient umask. `set_permissions` is idempotent.
    fs::set_permissions(runtime_dir_root, fs::Permissions::from_mode(0o700))?;
    Ok(agents)
}

/// Compute the unix socket path for a specific agent.
///
/// Does NOT create the directory; caller invokes [`ensure_agents_dir`]
/// separately.
pub fn agent_socket_path(agents_dir: &Path, id: &AgentId) -> PathBuf {
    agents_dir.join(format!("{}.sock", id.as_str()))
}

/// Compute the current user's runtime root: `{XDG_RUNTIME_DIR or /tmp}/ark-{uid}`.
///
/// Lightweight convenience for code that doesn't want to construct a full
/// [`ark_types::StateLayout`]. Mirrors the scheme implemented by
/// [`ark_types::EnvPaths`]; both should agree.
pub fn runtime_root() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join(format!("ark-{}", Uid::current().as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_agent_id() -> AgentId {
        AgentId::new("cavekit", "test")
    }

    #[test]
    fn ensure_agents_dir_creates_leaf_with_0700() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark-1000");
        fs::create_dir_all(&root).unwrap();

        let agents = ensure_agents_dir(&root).unwrap();
        assert_eq!(agents, root.join("agents"));
        assert!(agents.is_dir());

        let mode = fs::metadata(&agents).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "leaf dir mode should be 0700, got {mode:o}");
    }

    #[test]
    fn ensure_agents_dir_tightens_parent_permissions_to_0700() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark-1000");
        fs::create_dir_all(&root).unwrap();
        // Deliberately loosen parent perms before the call.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();

        ensure_agents_dir(&root).unwrap();

        let mode = fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "parent mode should be 0700, got {mode:o}");
    }

    #[test]
    fn ensure_agents_dir_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ark-1000");
        fs::create_dir_all(&root).unwrap();

        let first = ensure_agents_dir(&root).unwrap();
        let second = ensure_agents_dir(&root).unwrap();
        assert_eq!(first, second);
        assert!(first.is_dir());
    }

    #[test]
    fn agent_socket_path_composes_to_id_sock() {
        let tmp = TempDir::new().unwrap();
        let id = new_agent_id();
        let sock = agent_socket_path(tmp.path(), &id);

        assert_eq!(sock.parent().unwrap(), tmp.path());
        let fname = sock.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(fname, format!("{}.sock", id.as_str()));
        assert!(!fname.ends_with("/"), "no trailing slash");
    }

    #[test]
    fn runtime_root_has_ark_uid_segment() {
        let root = runtime_root();
        let s = root.to_string_lossy();
        assert!(
            s.contains("ark-"),
            "runtime_root should contain ark-, got {s}"
        );
    }
}
