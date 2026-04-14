//! Environment-variable path resolver for ark.
//!
//! Provides a thin layer on top of [`StateLayout`] that honors
//! `ARK_STATE_DIR` / `ARK_RUNTIME_DIR` / `ARK_CONFIG_DIR` overrides in addition
//! to the XDG base-directory conventions already implemented by
//! [`StateLayout::from_env`].
//!
//! Precedence per path:
//!   1. `ARK_STATE_DIR` / `ARK_RUNTIME_DIR` / `ARK_CONFIG_DIR`
//!   2. `XDG_STATE_HOME` / `XDG_RUNTIME_DIR` / `XDG_CONFIG_HOME`
//!   3. Platform fallback (`$HOME/.local/state/ark`, `/tmp/ark-{uid}`,
//!      `$HOME/.config/ark`).
//!
//! Runtime dir scheme per cavekit-hook-ipc.md R4: always ends in `ark-{uid}`
//! relative to the base runtime root. When `XDG_RUNTIME_DIR` IS set, runtime =
//! `$XDG_RUNTIME_DIR/ark-{uid}/`; otherwise `/tmp/ark-{uid}/`. On macOS
//! `XDG_RUNTIME_DIR` is never set by default so the `/tmp` branch is taken.
//! `ARK_RUNTIME_DIR`, when set, is used verbatim without appending the
//! `ark-{uid}` segment (caller is assumed to have picked an already-isolated
//! path — e.g. a test tempdir).
//!
//! **Naming note:** the kit references `ARK_CONFIG_PATH`, but a single-file
//! "path" env is a poor fit for a multi-file config directory. We expose
//! `ARK_CONFIG_DIR` here (consistent with the other two). If a future
//! config-loader (T-018) wants to honor a single `ARK_CONFIG_PATH` pointing
//! at one TOML file, that's the loader's concern, not this resolver's.
//!
//! Tests inject env values via the [`Env`] trait rather than mutating
//! `std::env`, avoiding the parallelism trap.

use std::path::PathBuf;

use thiserror::Error;

use crate::id::AgentId;
use crate::state_dir::StateLayout;

/// Abstraction over `std::env::var_os` so tests can inject values without
/// touching the process environment.
pub trait Env {
    fn var(&self, key: &str) -> Option<String>;
}

/// Reads from the real process environment via `std::env::var_os`. Returns
/// `None` on missing, empty, or invalid-utf-8 values.
pub struct StdEnv;

impl Env for StdEnv {
    fn var(&self, key: &str) -> Option<String> {
        match std::env::var_os(key) {
            Some(v) if !v.is_empty() => v.into_string().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum EnvPathsError {
    #[error("HOME is unset and no XDG_*_HOME provided fallback")]
    HomeUnset,
    #[error("path env var contains invalid utf-8")]
    InvalidUtf8,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct EnvPaths;

impl EnvPaths {
    /// Resolve a [`StateLayout`] from the real process environment.
    pub fn resolve() -> Result<StateLayout, EnvPathsError> {
        Self::resolve_with(&StdEnv, current_uid())
    }

    /// Compute just the runtime directory. Equivalent to
    /// `resolve()?.runtime().to_path_buf()`.
    pub fn runtime_dir() -> Result<PathBuf, EnvPathsError> {
        Ok(resolve_runtime(&StdEnv, current_uid()))
    }

    /// Per-agent control-socket path: `{runtime_dir}/agents/{id}.sock`.
    /// See cavekit-hook-ipc.md R4.
    pub fn agent_socket_path(id: &AgentId) -> Result<PathBuf, EnvPathsError> {
        let rt = Self::runtime_dir()?;
        Ok(rt.join("agents").join(format!("{}.sock", id.as_str())))
    }

    /// Injectable resolver — the single source of truth. All other entry
    /// points (including [`StateLayout::from_env`]) route through this.
    pub fn resolve_with<E: Env>(env: &E, uid: u32) -> Result<StateLayout, EnvPathsError> {
        let base = resolve_state(env)?;
        let config = resolve_config(env)?;
        let runtime = resolve_runtime(env, uid);
        Ok(StateLayout::new(base, runtime, config))
    }
}

fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

fn resolve_state<E: Env>(env: &E) -> Result<PathBuf, EnvPathsError> {
    if let Some(v) = env.var("ARK_STATE_DIR") {
        return Ok(PathBuf::from(v));
    }
    if let Some(v) = env.var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(v).join("ark"));
    }
    let home = env.var("HOME").ok_or(EnvPathsError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".local/state/ark"))
}

fn resolve_config<E: Env>(env: &E) -> Result<PathBuf, EnvPathsError> {
    if let Some(v) = env.var("ARK_CONFIG_DIR") {
        return Ok(PathBuf::from(v));
    }
    if let Some(v) = env.var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(v).join("ark"));
    }
    let home = env.var("HOME").ok_or(EnvPathsError::HomeUnset)?;
    Ok(PathBuf::from(home).join(".config/ark"))
}

/// Runtime resolution. `ARK_RUNTIME_DIR` is taken verbatim. Otherwise the
/// scheme is `{XDG_RUNTIME_DIR or /tmp}/ark-{uid}/` per hook-ipc R4.
fn resolve_runtime<E: Env>(env: &E, uid: u32) -> PathBuf {
    if let Some(v) = env.var("ARK_RUNTIME_DIR") {
        return PathBuf::from(v);
    }
    let leaf = format!("ark-{uid}");
    match env.var("XDG_RUNTIME_DIR") {
        Some(v) => PathBuf::from(v).join(leaf),
        None => PathBuf::from("/tmp").join(leaf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MapEnv(HashMap<String, String>);

    impl MapEnv {
        fn with(pairs: &[(&str, &str)]) -> Self {
            let mut m = HashMap::new();
            for (k, v) in pairs {
                m.insert((*k).to_string(), (*v).to_string());
            }
            Self(m)
        }
    }

    impl Env for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    const UID: u32 = 1000;

    #[test]
    fn ark_state_dir_overrides_xdg_and_home() {
        let env = MapEnv::with(&[
            ("ARK_STATE_DIR", "/explicit/state"),
            ("XDG_STATE_HOME", "/xdg/state"),
            ("HOME", "/home/u"),
        ]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.base(), PathBuf::from("/explicit/state"));
    }

    #[test]
    fn ark_runtime_dir_overrides_xdg() {
        let env = MapEnv::with(&[
            ("ARK_RUNTIME_DIR", "/explicit/rt"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("HOME", "/home/u"),
        ]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.runtime(), PathBuf::from("/explicit/rt"));
    }

    #[test]
    fn ark_config_dir_overrides_xdg_and_home() {
        let env = MapEnv::with(&[
            ("ARK_CONFIG_DIR", "/explicit/cfg"),
            ("XDG_CONFIG_HOME", "/xdg/cfg"),
            ("HOME", "/home/u"),
        ]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.config(), PathBuf::from("/explicit/cfg"));
    }

    #[test]
    fn xdg_takes_precedence_over_home_fallback() {
        let env = MapEnv::with(&[
            ("XDG_STATE_HOME", "/xdg/state"),
            ("XDG_CONFIG_HOME", "/xdg/cfg"),
            ("HOME", "/home/u"),
        ]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.base(), PathBuf::from("/xdg/state/ark"));
        assert_eq!(layout.config(), PathBuf::from("/xdg/cfg/ark"));
    }

    #[test]
    fn home_fallback_used_when_no_xdg_or_ark_set() {
        let env = MapEnv::with(&[("HOME", "/home/u")]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.base(), PathBuf::from("/home/u/.local/state/ark"));
        assert_eq!(layout.config(), PathBuf::from("/home/u/.config/ark"));
    }

    #[test]
    fn missing_home_and_xdg_errors() {
        let env = MapEnv::default();
        let err = EnvPaths::resolve_with(&env, UID).unwrap_err();
        assert!(matches!(err, EnvPathsError::HomeUnset));
    }

    #[test]
    fn runtime_contains_ark_uid_segment_with_xdg() {
        let env = MapEnv::with(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("HOME", "/home/u")]);
        let layout = EnvPaths::resolve_with(&env, 1000).expect("resolve");
        assert_eq!(layout.runtime(), PathBuf::from("/run/user/1000/ark-1000"));
        assert!(layout.runtime().to_string_lossy().contains("ark-"));
    }

    #[test]
    fn runtime_falls_back_to_tmp_when_xdg_unset() {
        // Simulates macOS default (XDG_RUNTIME_DIR unset).
        let env = MapEnv::with(&[("HOME", "/home/u")]);
        let layout = EnvPaths::resolve_with(&env, 1000).expect("resolve");
        assert_eq!(layout.runtime(), PathBuf::from("/tmp/ark-1000"));
    }

    #[test]
    fn ark_precedence_over_xdg() {
        let env = MapEnv::with(&[
            ("ARK_STATE_DIR", "/a/state"),
            ("ARK_RUNTIME_DIR", "/a/rt"),
            ("ARK_CONFIG_DIR", "/a/cfg"),
            ("XDG_STATE_HOME", "/x/state"),
            ("XDG_RUNTIME_DIR", "/x/rt"),
            ("XDG_CONFIG_HOME", "/x/cfg"),
            ("HOME", "/home/u"),
        ]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        assert_eq!(layout.base(), PathBuf::from("/a/state"));
        assert_eq!(layout.runtime(), PathBuf::from("/a/rt"));
        assert_eq!(layout.config(), PathBuf::from("/a/cfg"));
    }

    #[test]
    fn resolve_runtime_direct_helper() {
        let env = MapEnv::with(&[("XDG_RUNTIME_DIR", "/run/user/42")]);
        assert_eq!(
            resolve_runtime(&env, 42),
            PathBuf::from("/run/user/42/ark-42")
        );

        let env = MapEnv::default();
        assert_eq!(resolve_runtime(&env, 42), PathBuf::from("/tmp/ark-42"));

        let env = MapEnv::with(&[("ARK_RUNTIME_DIR", "/verbatim")]);
        assert_eq!(resolve_runtime(&env, 42), PathBuf::from("/verbatim"));
    }

    #[test]
    fn agent_socket_path_under_runtime_agents() {
        // Exercise the composition: runtime_dir() + /agents/{id}.sock.
        // Use explicit injection via resolve_with to avoid touching process env.
        let env = MapEnv::with(&[("ARK_RUNTIME_DIR", "/rt"), ("HOME", "/home/u")]);
        let layout = EnvPaths::resolve_with(&env, UID).expect("resolve");
        let id = AgentId::new("cavekit", "auth");
        let expected = PathBuf::from("/rt")
            .join("agents")
            .join(format!("{}.sock", id.as_str()));
        assert_eq!(layout.agent_socket_path(&id), expected);
    }

    #[test]
    fn std_env_reads_process_env() {
        // Smoke test: HOME is virtually always set in test environments.
        let env = StdEnv;
        // Don't assert a specific value; just ensure the implementation
        // returns something for a common var without panicking.
        let _ = env.var("PATH");
    }
}
