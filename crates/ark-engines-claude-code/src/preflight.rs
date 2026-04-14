//! Preflight environment validation (cavekit-engine-claude-code R7).
//!
//! Called by the supervisor *before* `install_observability` runs. Each
//! check returns a structured [`PreflightError`] whose `Display` impl
//! includes a human-readable remediation hint, so a CLI can surface a
//! single useful sentence to the operator.
//!
//! Checks performed (in order):
//!
//! 1. `claude` binary is on `PATH`.
//! 2. `~/.claude/` directory exists.
//! 3. `{cwd}` (the worktree root) is writable. Note we explicitly do
//!    **not** require `{cwd}/.claude/` to exist yet —
//!    `install_observability` will create it.
//! 4. `ark-hook` binary is discoverable (same directory as the running
//!    `ark` executable, falling back to `PATH`).
//!
//! ## Testability
//!
//! [`preflight`] is the production entry point and resolves the
//! environment itself. [`preflight_with`] takes injected closures and
//! paths so unit tests can exercise every failure path without polluting
//! the real `~/.claude` or `PATH`.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use ark_types::AgentSpec;
use thiserror::Error;

/// Result of [`preflight`]: either all four checks pass (`Ok`) or a
/// single, specific failure with remediation hint.
#[derive(Debug, Error)]
pub enum PreflightError {
    /// `claude` binary not found on `PATH`.
    #[error("claude CLI not on PATH — install from https://claude.com/claude-code or add to PATH")]
    ClaudeNotOnPath,

    /// `~/.claude/` directory does not exist.
    #[error("~/.claude not found at {path:?} — run claude once to initialize")]
    ClaudeHomeMissing { path: PathBuf },

    /// `{cwd}` is not writable by the current process.
    #[error("cannot write to {cwd:?} — check permissions or choose another worktree: {source}")]
    CwdNotWritable {
        cwd: PathBuf,
        #[source]
        source: io::Error,
    },

    /// `ark-hook` binary not found alongside `ark` or on `PATH`.
    #[error(
        "ark-hook binary not found alongside ark or on PATH — ensure the ark installation is complete"
    )]
    ArkHookNotFound,
}

/// Validate the environment for a Claude Code spawn against `spec`.
///
/// Returns `Ok(())` when every check passes. On failure, returns a
/// [`PreflightError`] whose `Display` impl is the remediation hint.
pub fn preflight(spec: &AgentSpec) -> Result<(), PreflightError> {
    let home = resolve_home();
    preflight_with(
        home.as_deref(),
        &spec.cwd,
        claude_on_path,
        ark_hook_discoverable,
    )
}

/// Test-friendly variant of [`preflight`] with all four resolvers
/// injected. Production code calls [`preflight`] which wires the real
/// resolvers; unit tests pass stubs.
///
/// `home` is `None` when `$HOME` is unset; in that case we report
/// [`PreflightError::ClaudeHomeMissing`] with an empty path so the user
/// still gets a sensible message.
pub fn preflight_with(
    home: Option<&Path>,
    cwd: &Path,
    claude_checker: impl Fn() -> bool,
    ark_hook_checker: impl Fn() -> bool,
) -> Result<(), PreflightError> {
    // 1. `claude` on PATH.
    if !claude_checker() {
        return Err(PreflightError::ClaudeNotOnPath);
    }

    // 2. `~/.claude/` exists as a directory.
    let claude_home = match home {
        Some(h) => h.join(".claude"),
        None => PathBuf::from(".claude"),
    };
    if !claude_home.is_dir() {
        return Err(PreflightError::ClaudeHomeMissing { path: claude_home });
    }

    // 3. cwd is writable. We probe by creating + removing a unique file
    //    at `{cwd}/.ark-preflight-probe-{pid}` rather than touching
    //    `.claude/` (which install_observability will create later).
    let probe = cwd.join(format!(".ark-preflight-probe-{}", process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            // Best-effort cleanup; ignore failure (the file is tiny and
            // user can sweep it manually if removal racey-fails).
            let _ = fs::remove_file(&probe);
        }
        Err(source) => {
            return Err(PreflightError::CwdNotWritable {
                cwd: cwd.to_path_buf(),
                source,
            });
        }
    }

    // 4. `ark-hook` next to ark, or on PATH.
    if !ark_hook_checker() {
        return Err(PreflightError::ArkHookNotFound);
    }

    Ok(())
}

// --- production resolvers -------------------------------------------------

/// Returns `$HOME` as a `PathBuf` if set and non-empty.
fn resolve_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `true` iff a binary named `claude` is found on `$PATH` and is
/// executable by the current process.
fn claude_on_path() -> bool {
    find_on_path("claude").is_some()
}

/// `true` iff `ark-hook` is discoverable next to the current executable
/// or anywhere on `$PATH`.
fn ark_hook_discoverable() -> bool {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for name in ARK_HOOK_NAMES {
                let candidate = parent.join(name);
                if is_executable_file(&candidate) {
                    return true;
                }
            }
        }
    }
    find_on_path("ark-hook").is_some()
}

#[cfg(windows)]
const ARK_HOOK_NAMES: &[&str] = &["ark-hook.exe", "ark-hook"];
#[cfg(not(windows))]
const ARK_HOOK_NAMES: &[&str] = &["ark-hook"];

/// Walk `$PATH` for a binary named `name` (with platform-appropriate
/// extension on Windows) and return the first executable match.
fn find_on_path<S: AsRef<OsStr>>(name: S) -> Option<PathBuf> {
    let name = name.as_ref();
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let mut with_ext = candidate.clone();
                with_ext.set_extension(ext);
                if is_executable_file(&with_ext) {
                    return Some(with_ext);
                }
            }
        }
    }
    None
}

/// `true` if `p` is a regular file that the current process can
/// (probably) execute. On Unix we check the user-execute bit. On
/// Windows we accept any regular file (the path-extension dance in
/// [`find_on_path`] is what filters by executability there).
fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// --- tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use tempfile::TempDir;

    /// Make a tempdir that doubles as a fake `$HOME` containing
    /// `.claude/`, so `claude_home_missing` can be controlled per test.
    fn fake_home_with_claude() -> TempDir {
        let home = TempDir::new().expect("home tempdir");
        fs::create_dir(home.path().join(".claude")).expect("mkdir .claude");
        home
    }

    #[test]
    fn cwd_writable_passes() {
        let home = fake_home_with_claude();
        let cwd = TempDir::new().expect("cwd tempdir");
        let result = preflight_with(Some(home.path()), cwd.path(), || true, || true);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn cwd_not_writable_returns_cwd_error() {
        let home = fake_home_with_claude();
        let cwd = TempDir::new().expect("cwd tempdir");

        // Strip write permission from the directory itself.
        let mut perms = fs::metadata(cwd.path()).expect("meta").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o555);
        }
        fs::set_permissions(cwd.path(), perms).expect("chmod");

        let result = preflight_with(Some(home.path()), cwd.path(), || true, || true);

        // Restore perms so TempDir cleanup works.
        let mut restore = fs::metadata(cwd.path()).expect("meta").permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            restore.set_mode(0o755);
        }
        fs::set_permissions(cwd.path(), restore).expect("chmod restore");

        match result {
            Err(PreflightError::CwdNotWritable { cwd: reported, .. }) => {
                assert_eq!(reported, cwd.path());
            }
            other => panic!("expected CwdNotWritable, got {other:?}"),
        }
    }

    #[test]
    fn claude_home_missing_when_no_dot_claude() {
        // A fresh tempdir with no `.claude/` subdir.
        let home = TempDir::new().expect("home tempdir");
        let cwd = TempDir::new().expect("cwd tempdir");
        let result = preflight_with(Some(home.path()), cwd.path(), || true, || true);
        match result {
            Err(PreflightError::ClaudeHomeMissing { path }) => {
                assert_eq!(path, home.path().join(".claude"));
            }
            other => panic!("expected ClaudeHomeMissing, got {other:?}"),
        }
    }

    #[test]
    fn claude_not_on_path_returns_first() {
        let home = fake_home_with_claude();
        let cwd = TempDir::new().expect("cwd tempdir");
        let result = preflight_with(Some(home.path()), cwd.path(), || false, || true);
        assert!(matches!(result, Err(PreflightError::ClaudeNotOnPath)));
    }

    #[test]
    fn ark_hook_not_found_returns_last() {
        let home = fake_home_with_claude();
        let cwd = TempDir::new().expect("cwd tempdir");
        let result = preflight_with(Some(home.path()), cwd.path(), || true, || false);
        assert!(matches!(result, Err(PreflightError::ArkHookNotFound)));
    }

    #[test]
    fn all_checks_pass_with_injected_resolvers() {
        let home = fake_home_with_claude();
        let cwd = TempDir::new().expect("cwd tempdir");
        let result = preflight_with(Some(home.path()), cwd.path(), || true, || true);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn display_hints_contain_keywords() {
        let claude_not_on_path = PreflightError::ClaudeNotOnPath.to_string();
        assert!(
            claude_not_on_path.contains("PATH"),
            "missing PATH: {claude_not_on_path}"
        );
        assert!(
            claude_not_on_path.contains("install"),
            "missing install: {claude_not_on_path}"
        );

        let claude_home_missing = PreflightError::ClaudeHomeMissing {
            path: PathBuf::from("/tmp/nope/.claude"),
        }
        .to_string();
        assert!(
            claude_home_missing.contains(".claude"),
            "missing .claude: {claude_home_missing}"
        );
        assert!(
            claude_home_missing.contains("initialize"),
            "missing initialize: {claude_home_missing}"
        );

        let cwd_not_writable = PreflightError::CwdNotWritable {
            cwd: PathBuf::from("/tmp/locked"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        }
        .to_string();
        assert!(
            cwd_not_writable.contains("permissions"),
            "missing permissions: {cwd_not_writable}"
        );
        assert!(
            cwd_not_writable.contains("worktree"),
            "missing worktree: {cwd_not_writable}"
        );

        let ark_hook = PreflightError::ArkHookNotFound.to_string();
        assert!(
            ark_hook.contains("ark-hook"),
            "missing ark-hook: {ark_hook}"
        );
        assert!(
            ark_hook.contains("PATH"),
            "missing PATH in ark-hook: {ark_hook}"
        );
    }
}
