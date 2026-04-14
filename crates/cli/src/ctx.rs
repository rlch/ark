//! Runtime context passed into every subcommand handler.
//!
//! T-084 scaffolded this with just `no_color`. T-093 (cavekit-cli R8)
//! extends it with env-var recognition: log level (`ARK_LOG` / `RUST_LOG`)
//! and resolved state paths (`ARK_STATE_DIR`, `ARK_CONFIG_DIR`,
//! `ARK_RUNTIME_DIR`, via ark-types `StateLayout::from_env`).
//!
//! NO_COLOR precedence for the scaffold:
//!   1. If `NO_COLOR` env var is set to a non-empty string → true.
//!   2. Otherwise → false (default).
//!
//! Log-level precedence:
//!   1. `ARK_LOG` if set and non-empty.
//!   2. Otherwise `RUST_LOG` if set and non-empty.
//!   3. Otherwise default `"info"`.
//!
//! State/config/runtime dirs are resolved through
//! [`ark_types::StateLayout::from_env`] — the single source of truth for
//! ark path resolution. We hold the three resolved `PathBuf`s directly.

use std::path::PathBuf;

use ark_types::StateLayout;

/// Pure helper: returns `true` when the env getter yields any non-empty
/// value for `NO_COLOR` (per <https://no-color.org>: any set value
/// disables color).
///
/// Mirrors the helper in `ark-pane` so both crates agree on semantics
/// without depending on each other just for this one check.
pub fn no_color_from_env<F>(getter: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    matches!(getter("NO_COLOR"), Some(v) if !v.is_empty())
}

/// Convenience: reads the process environment. Equivalent to calling
/// [`no_color_from_env`] with `|k| std::env::var(k).ok()`.
pub fn detect_no_color() -> bool {
    no_color_from_env(|k| std::env::var(k).ok())
}

/// Pure helper: resolve the log-level filter string from an env getter.
/// Prefers `ARK_LOG`, falls back to `RUST_LOG`, else `"info"`. Empty
/// values are treated as unset.
pub fn log_level_from_env<F>(getter: F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(v) = getter("ARK_LOG") {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(v) = getter("RUST_LOG") {
        if !v.is_empty() {
            return v;
        }
    }
    "info".to_string()
}

/// Convenience: reads the process environment for the log level.
pub fn detect_log_level() -> String {
    log_level_from_env(|k| std::env::var(k).ok())
}

/// Shared context threaded through subcommand dispatch.
///
/// Carries the color flag, the resolved log-level filter, and the
/// resolved state / config / runtime directories. Not `Copy` because it
/// owns `PathBuf`s — pass by reference or clone.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// Whether to suppress ANSI color in any custom output.
    pub no_color: bool,
    /// Tracing env-filter directive string (e.g. `"info"`, `"ark=debug"`).
    pub log_level: String,
    /// Resolved ark state base dir (honors `ARK_STATE_DIR`).
    pub state_dir: PathBuf,
    /// Resolved ark config dir (honors `ARK_CONFIG_DIR`).
    pub config_dir: PathBuf,
    /// Resolved ark runtime dir (honors `ARK_RUNTIME_DIR`).
    pub runtime_dir: PathBuf,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            no_color: false,
            log_level: "info".to_string(),
            state_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            runtime_dir: PathBuf::new(),
        }
    }
}

impl Ctx {
    /// Build a [`Ctx`] from the process environment.
    ///
    /// Path resolution goes through [`StateLayout::from_env`]; if that
    /// fails (e.g. `HOME` unset) we surface the error to the caller.
    pub fn from_env() -> Result<Self, ark_types::StateLayoutError> {
        let layout = StateLayout::from_env()?;
        Ok(Self {
            no_color: detect_no_color(),
            log_level: detect_log_level(),
            state_dir: layout.base().to_path_buf(),
            config_dir: layout.config().to_path_buf(),
            runtime_dir: layout.runtime().to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize all tests that mutate process env. Must be held for the
    /// duration of every test that calls `std::env::set_var` /
    /// `remove_var` or indirectly reads env via `detect_*` / `from_env`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn no_color_env_set_nonempty_is_true() {
        assert!(no_color_from_env(|k| if k == "NO_COLOR" {
            Some("1".to_string())
        } else {
            None
        }));
    }

    #[test]
    fn no_color_env_unset_is_false() {
        assert!(!no_color_from_env(|_| None));
    }

    #[test]
    fn no_color_env_empty_is_false() {
        // Per NO_COLOR spec only non-empty values disable color.
        assert!(!no_color_from_env(|k| if k == "NO_COLOR" {
            Some(String::new())
        } else {
            None
        }));
    }

    #[test]
    fn ctx_default_has_no_color_false() {
        assert!(!Ctx::default().no_color);
    }

    #[test]
    fn ctx_default_has_info_log_level() {
        assert_eq!(Ctx::default().log_level, "info");
    }

    #[test]
    fn log_level_prefers_ark_log() {
        let got = log_level_from_env(|k| match k {
            "ARK_LOG" => Some("ark=debug".to_string()),
            "RUST_LOG" => Some("trace".to_string()),
            _ => None,
        });
        assert_eq!(got, "ark=debug");
    }

    #[test]
    fn log_level_falls_back_to_rust_log() {
        let got = log_level_from_env(|k| match k {
            "RUST_LOG" => Some("warn".to_string()),
            _ => None,
        });
        assert_eq!(got, "warn");
    }

    #[test]
    fn log_level_defaults_to_info_when_neither_set() {
        let got = log_level_from_env(|_| None);
        assert_eq!(got, "info");
    }

    #[test]
    fn log_level_empty_ark_log_falls_back_to_rust_log() {
        let got = log_level_from_env(|k| match k {
            "ARK_LOG" => Some(String::new()),
            "RUST_LOG" => Some("debug".to_string()),
            _ => None,
        });
        assert_eq!(got, "debug");
    }

    /// RAII guard that restores an env var to its prior value on drop.
    /// Required because the process-env tests share global state and we
    /// don't want cross-test leakage even with the mutex.
    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prior = std::env::var(key).ok();
            // Safety: tests are serialized by ENV_LOCK.
            unsafe {
                std::env::set_var(key, val);
            }
            Self { key, prior }
        }

        fn unset(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn ctx_from_env_picks_up_ark_log() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ark = EnvGuard::set("ARK_LOG", "ark=trace");
        let _rust = EnvGuard::unset("RUST_LOG");
        // Ensure HOME is set so StateLayout::from_env succeeds on CI.
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.log_level, "ark=trace");
    }

    #[test]
    fn ctx_from_env_falls_back_to_rust_log() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ark = EnvGuard::unset("ARK_LOG");
        let _rust = EnvGuard::set("RUST_LOG", "warn");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.log_level, "warn");
    }

    #[test]
    fn ctx_from_env_defaults_log_level_to_info() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ark = EnvGuard::unset("ARK_LOG");
        let _rust = EnvGuard::unset("RUST_LOG");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.log_level, "info");
    }

    #[test]
    fn ctx_from_env_honors_ark_state_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _state = EnvGuard::set("ARK_STATE_DIR", "/explicit/state-t093");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.state_dir, PathBuf::from("/explicit/state-t093"));
    }

    #[test]
    fn ctx_from_env_honors_ark_config_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _cfg = EnvGuard::set("ARK_CONFIG_DIR", "/explicit/cfg-t093");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.config_dir, PathBuf::from("/explicit/cfg-t093"));
    }

    #[test]
    fn ctx_from_env_honors_ark_runtime_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _rt = EnvGuard::set("ARK_RUNTIME_DIR", "/explicit/rt-t093");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert_eq!(ctx.runtime_dir, PathBuf::from("/explicit/rt-t093"));
    }

    #[test]
    fn ctx_from_env_preserves_no_color() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _nc = EnvGuard::set("NO_COLOR", "1");
        let _home = match std::env::var("HOME") {
            Ok(_) => EnvGuard::set("HOME", &std::env::var("HOME").unwrap()),
            Err(_) => EnvGuard::set("HOME", "/tmp"),
        };
        let ctx = Ctx::from_env().expect("from_env");
        assert!(ctx.no_color);
    }
}
