//! `ark ext remove` — uninstall an extension.
//!
//! T-12.10 (cavekit-scene R13). Deletes
//! `${XDG_DATA_HOME}/ark/extensions/<name>/` recursively. The operation
//! is deliberately narrow: it ONLY touches the user-tier directory, so
//! system-installed or built-in extensions can't be removed by this
//! command. That mirrors the `ark ext add` contract — installs go into
//! the user tier and removals come back out of the user tier.
//!
//! # Safety
//!
//! * If the directory doesn't exist the command returns
//!   [`CliError::NotFound`] with a hint pointing at `ark ext list`.
//! * `fs::remove_dir_all` is used directly — no confirmation prompt
//!   because this maps 1:1 to `rm -rf <dir>` and the user already
//!   typed the extension name. Pair with `ark ext info <name>` before
//!   invoking when uncertain.
//! * A summary line ("removed `<name>` at <path>") is printed on
//!   success so the user has a durable record of what disappeared.

use std::fs;
use std::path::PathBuf;

use clap::Args;

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark ext remove`.
#[derive(Debug, Args)]
#[command(
    about = "Uninstall an ark extension from the user tier",
    long_about = "Remove an extension previously installed via\n\
                  `ark ext add`. Deletes\n\
                  `${XDG_DATA_HOME}/ark/extensions/<name>/` recursively.\n\
                  \n\
                  Only user-tier installs are affected — system and\n\
                  project-scoped extensions must be removed manually.\n\
                  \n\
                  Examples:\n  \
                  ark ext remove picker"
)]
pub struct RemoveArgs {
    /// Name of the extension to remove.
    #[arg(required = true, value_name = "NAME")]
    pub name: String,
}

/// Dispatch handler for `ark ext remove`.
pub fn run(args: RemoveArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let xdg_data_home = resolve_xdg_data_home().map_err(|reason| CliError::Generic {
        reason: format!("ext/remove: {reason}"),
    })?;
    let target = xdg_data_home.join("ark/extensions").join(&args.name);
    remove_extension(&target, &args.name)
}

/// Pure helper: remove `target` and print a summary. Exposed for tests.
pub fn remove_extension(target: &std::path::Path, name: &str) -> Result<(), CliError> {
    if !target.exists() {
        return Err(CliError::NotFound {
            what: format!(
                "extension `{name}` at {} (run `ark ext list` to see what's installed)",
                target.display()
            ),
        });
    }
    if !target.is_dir() {
        return Err(CliError::Generic {
            reason: format!(
                "ext/remove: target {} is not a directory — refusing to remove",
                target.display()
            ),
        });
    }
    fs::remove_dir_all(target).map_err(|e| CliError::Generic {
        reason: format!("ext/remove: failed to delete {}: {e}", target.display()),
    })?;
    println!("removed extension `{name}` at {}", target.display());
    Ok(())
}

/// Resolve `${XDG_DATA_HOME}` with the standard XDG fallback.
///
/// Duplicated from `ext::add` (rather than exposing that internal) so
/// the ext module hierarchy stays additive — a future `ark-state` crate
/// can absorb both without a breaking refactor.
pub(crate) fn resolve_xdg_data_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_ext(root: &std::path::Path, name: &str) -> PathBuf {
        let dir = root.join("ark/extensions").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extension.kdl"), "extension { name \"x\" }").unwrap();
        fs::write(dir.join(".ark-install"), "source: path:/tmp/x\n").unwrap();
        dir
    }

    #[test]
    fn remove_extension_deletes_dir_recursively() {
        let tmp = TempDir::new().unwrap();
        let dir = seed_ext(tmp.path(), "picker");
        assert!(dir.join("extension.kdl").exists());
        remove_extension(&dir, "picker").expect("remove");
        assert!(!dir.exists(), "extension dir should be gone");
    }

    #[test]
    fn remove_extension_missing_returns_not_found() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("ark/extensions/ghost");
        let err = remove_extension(&missing, "ghost").unwrap_err();
        assert!(matches!(err, CliError::NotFound { .. }), "{err}");
    }

    #[test]
    fn remove_extension_rejects_non_directory() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        fs::write(&file_path, b"hi").unwrap();
        let err = remove_extension(&file_path, "not-a-dir").unwrap_err();
        match err {
            CliError::Generic { reason } => {
                assert!(reason.contains("not a directory"), "{reason}");
            }
            other => panic!("expected Generic, got {other:?}"),
        }
    }

    #[test]
    fn remove_full_run_through_xdg() {
        // End-to-end: seed XDG_DATA_HOME, dispatch via `run`, assert the
        // extension directory is gone.
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        fs::create_dir_all(&xdg).unwrap();
        seed_ext(&xdg, "kill-me");

        let _lock = crate::test_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("XDG_DATA_HOME");
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &xdg);
        }

        let result = run(
            RemoveArgs {
                name: "kill-me".into(),
            },
            &Ctx::default(),
        );

        unsafe {
            match prior {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        result.expect("remove should succeed");
        assert!(!xdg.join("ark/extensions/kill-me").exists());
    }
}
