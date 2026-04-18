//! `ZELLIJ_SOCKET_DIR` bootstrapping.
//!
//! On macOS, `$TMPDIR` resolves to `/var/folders/<two-char>/<28-char>/T/`
//! (~49 bytes). Zellij appends `zellij-<uid>/contract_version_1/<session-name>`
//! (~30 bytes) plus the session name to produce a unix-domain socket path.
//! ark's session names carry a ulid (`<name>-<26-char-ulid>`) plus an `ark-`
//! prefix, so the total socket path regularly exceeds the 103-byte `sun_path`
//! cap on darwin and zellij fails to bind.
//!
//! The fix is to point zellij at a short directory. `/tmp/ark-<uid>` is
//! ~14 bytes and leaves plenty of headroom for the session name plus
//! zellij's own `contract_version_1/` prefix. ark already uses `/tmp` for
//! the supervisor's control socket, the hook bridge socket, and the kill
//! tempdir for the same reason — see `crates/supervisor/src/control_socket.rs`,
//! `crates/hook/src/bridge.rs`, and `crates/cli/src/commands/kill.rs`.
//!
//! This module is called once at `ark` binary entry. All subsequent
//! zellij spawns (supervisor's `ensure_session`, the outside-zellij PTY
//! spawn in `pty::spawn_zellij_with_pty`, every `zellij action …`) inherit
//! the env var via the normal Unix child-env inheritance rules.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Env var zellij honors for its socket directory root.
const ENV_KEY: &str = "ZELLIJ_SOCKET_DIR";

/// If `ZELLIJ_SOCKET_DIR` is already set in the process env, leave it
/// alone — the user (or a wrapper script) has an opinion and we respect
/// it so that ark-inside-user's-zellij can find the same server.
///
/// Otherwise compute `/tmp/ark-<uid>`, `mkdir -p` it with `0700` perms,
/// and set the env var. Idempotent: repeated calls on the same process
/// are cheap no-ops after the first.
///
/// Returns the resolved path regardless of which branch ran, so callers
/// can log it.
pub fn ensure_short_socket_dir() -> PathBuf {
    if let Some(existing) = std::env::var_os(ENV_KEY) {
        return PathBuf::from(existing);
    }

    let uid = nix::unistd::Uid::current().as_raw();
    let path = PathBuf::from(format!("/tmp/ark-{uid}"));

    if let Err(e) = fs::create_dir_all(&path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "failed to create ZELLIJ_SOCKET_DIR; falling back to zellij default",
        );
        return path;
    }

    // Best-effort 0700. If another user pre-created the directory with
    // looser perms we leave them: `set_permissions` will fail and we log,
    // but we still set the env var so zellij can try.
    if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
        tracing::debug!(
            path = %path.display(),
            error = %e,
            "could not tighten ZELLIJ_SOCKET_DIR perms to 0700",
        );
    }

    // SAFETY: setting an env var early in `main` before any thread has
    // been spawned is sound. ark's binary entry calls this before any
    // tokio runtime / tracing subscriber construction beyond the initial
    // stderr writer.
    unsafe { std::env::set_var(ENV_KEY, &path) };
    tracing::debug!(path = %path.display(), "set {ENV_KEY}");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: run `f` with `ZELLIJ_SOCKET_DIR` unset, then restore the
    /// previous value. Tests in this module touch process env, so we
    /// serialize via a module-level mutex to avoid races with other
    /// env-touching tests running in parallel.
    fn with_env_unset<F: FnOnce()>(f: F) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let prior = std::env::var_os(ENV_KEY);
        unsafe { std::env::remove_var(ENV_KEY) };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prior {
            Some(v) => unsafe { std::env::set_var(ENV_KEY, v) },
            None => unsafe { std::env::remove_var(ENV_KEY) },
        }
        if let Err(p) = r {
            std::panic::resume_unwind(p);
        }
    }

    #[test]
    fn respects_existing_env() {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap();
        let prior = std::env::var_os(ENV_KEY);
        unsafe { std::env::set_var(ENV_KEY, "/does/not/matter") };
        let got = ensure_short_socket_dir();
        assert_eq!(got, PathBuf::from("/does/not/matter"));
        match prior {
            Some(v) => unsafe { std::env::set_var(ENV_KEY, v) },
            None => unsafe { std::env::remove_var(ENV_KEY) },
        }
    }

    #[test]
    fn creates_and_sets_when_unset() {
        with_env_unset(|| {
            let got = ensure_short_socket_dir();
            let uid = nix::unistd::Uid::current().as_raw();
            assert_eq!(got, PathBuf::from(format!("/tmp/ark-{uid}")));
            assert!(got.is_dir(), "{} should exist", got.display());
            assert_eq!(std::env::var(ENV_KEY).unwrap(), got.to_string_lossy());
        });
    }

    #[test]
    fn idempotent_on_repeat_call() {
        with_env_unset(|| {
            let a = ensure_short_socket_dir();
            let b = ensure_short_socket_dir();
            assert_eq!(a, b);
        });
    }
}
