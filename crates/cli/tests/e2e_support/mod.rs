//! T-128 — shared e2e test helpers (cavekit-testing R4).
//!
//! Supplies:
//!
//! * [`require_e2e`] — the `ARK_E2E=1` skip gate lifted out of each
//!   scenario (DRY for T-127's inline `should_skip`).
//! * [`E2eEnv`] — RAII guard owning three tempdirs (state / runtime /
//!   config), stamping the matching `ARK_*_DIR` env vars, and — on drop
//!   — signalling every tracked child pid then restoring the previous
//!   env. Drop runs even on panic so `cargo test` panics never leak
//!   supervisor processes or stale tempdirs into the next scenario.
//! * [`ark_cmd`] / [`mock_claude_cmd`] — thin `Command` factories keyed
//!   to `env!("CARGO_BIN_EXE_*")` so scenarios don't re-derive paths.
//! * [`track_pid`] — register a spawned pid with the guard so Drop
//!   tears it down.
//!
//! Used as an in-crate integration-test module: `e2e.rs` declares
//! `mod e2e_support;` and cargo does not treat subdirectories of
//! `tests/` as separate test binaries, so this file never compiles on
//! its own.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tempfile::TempDir;

// ---- ARK_E2E gate ---------------------------------------------------------

/// Returns `true` if `ARK_E2E=1`. Otherwise prints a stable `SKIP:` line
/// on stderr and returns `false`. Scenarios call this at the top of the
/// test body and early-return on `false`:
///
/// ```ignore
/// if !e2e_support::require_e2e() { return; }
/// ```
pub fn require_e2e() -> bool {
    match std::env::var("ARK_E2E").ok().as_deref() {
        Some("1") => true,
        _ => {
            eprintln!("SKIP: set ARK_E2E=1 to run e2e tests");
            false
        }
    }
}

// ---- binary discovery -----------------------------------------------------

/// Absolute path to the compiled `ark` binary, injected by cargo at
/// build time via the `CARGO_BIN_EXE_<name>` convention.
pub fn ark_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ark"))
}

/// Best-effort path to the `mock-claude` binary built out of
/// `ark-test-fixtures`. Cargo does not inject `CARGO_BIN_EXE_*` for
/// sibling-crate bins, so we look next to the `ark` binary in the
/// shared target dir. Returns `None` when the sibling has not been
/// produced — callers should treat that as a skip.
pub fn mock_claude_bin() -> Option<PathBuf> {
    let ark = ark_bin();
    let parent = ark.parent()?;
    let candidates = [
        parent.join("mock-claude"),
        parent
            .parent()
            .map(|p| p.join("mock-claude"))
            .unwrap_or_default(),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Build a `Command` targeting the `ark` binary. `E2eEnv` already
/// stamps the required `ARK_*_DIR` env vars on the *process*, so any
/// child inherits them automatically; callers only need to append
/// sub-command arguments. `NO_COLOR=1` keeps stdout deterministic.
pub fn ark_cmd() -> Command {
    let mut c = Command::new(ark_bin());
    c.env("NO_COLOR", "1");
    c
}

/// Build a `Command` targeting the `mock-claude` shim. Caller is
/// responsible for appending `--script` / `--output` flags as needed.
/// Returns `None` when `mock_claude_bin()` cannot locate the binary
/// (scenario should skip).
pub fn mock_claude_cmd() -> Option<Command> {
    mock_claude_bin().map(Command::new)
}

// ---- RAII environment guard ----------------------------------------------

/// RAII bundle of the three ark directories (`state`, `runtime`,
/// `config`) plus the previous values of their env vars so Drop can
/// restore them. Also tracks pids of children that a scenario wants
/// torn down on failure.
///
/// ## Drop ordering
///
/// 1. Signal every tracked pid with SIGTERM; if still alive after ~1s
///    escalate to SIGKILL. Panics in the SIGKILL path are swallowed —
///    drop must not double-panic during an already-panicking test.
/// 2. Restore each saved env var (unset if it was absent, overwrite
///    otherwise).
/// 3. `TempDir` drops auto-delete the directory trees.
///
/// Fields are public-within-crate only via accessor methods; scenarios
/// should use [`E2eEnv::state_dir`], [`E2eEnv::runtime_dir`], etc.
pub struct E2eEnv {
    state_dir: TempDir,
    runtime_dir: TempDir,
    config_dir: TempDir,
    /// Snapshots of env-var values present before `new()` ran. Each
    /// entry is `(NAME, prior_value_or_none)`.
    prev_env: Vec<(String, Option<String>)>,
    /// Child pids registered via [`track_pid`]. `Mutex` because Drop
    /// and test-body calls share the guard and a scenario might spawn
    /// from multiple threads (uncommon but defensively supported).
    spawned_pids: Mutex<Vec<u32>>,
}

impl E2eEnv {
    /// Create three fresh tempdirs, set the `ARK_*_DIR` env vars to
    /// point inside them, and return the guard. Pre-creates the
    /// standard subdirs so sub-processes don't race on first-use
    /// mkdir.
    pub fn new() -> Self {
        // Keep prefixes short: the runtime dir hosts unix sockets and
        // the total path length must stay under SUN_LEN (104 bytes on
        // macOS). Long tempdir prefixes plus the `agents/<long-id>.sock`
        // suffix can trip that limit even on short agent names.
        //
        // macOS's default `$TMPDIR` resolves to `/var/folders/<long>/T/`
        // — already 50+ bytes before the prefix. With a 36-char agent
        // id and a 5-char `.sock` extension the total blows past 104.
        // We force `/tmp` (a short, always-writable path) for the
        // runtime dir specifically. State + config dirs can stay on
        // the default tempdir — they don't host sockets.
        let state_dir = tempfile::Builder::new()
            .prefix("ark-s-")
            .tempdir()
            .expect("state tempdir");
        let runtime_dir = tempfile::Builder::new()
            .prefix("ark-r-")
            .tempdir_in("/tmp")
            .expect("runtime tempdir");
        let config_dir = tempfile::Builder::new()
            .prefix("ark-c-")
            .tempdir()
            .expect("config tempdir");

        // Pre-create standard subdirs — `ark list` walks `state/agents`
        // and `ark doctor` walks `runtime/agents`; both tolerate the
        // dir missing, but creating them up front avoids spurious
        // "directory not found" logging in test output.
        std::fs::create_dir_all(state_dir.path().join("agents")).unwrap();
        std::fs::create_dir_all(runtime_dir.path().join("agents")).unwrap();

        // Snapshot + set env vars. `set_var` / `remove_var` are unsafe
        // in Rust 2024; stay on the safe side by running sequentially
        // in the test thread (cargo test spawns tests on its own
        // threads but we never fan out inside a single E2eEnv).
        let mut prev_env = Vec::with_capacity(3);
        for (name, value) in [
            ("ARK_STATE_DIR", state_dir.path()),
            ("ARK_RUNTIME_DIR", runtime_dir.path()),
            ("ARK_CONFIG_DIR", config_dir.path()),
        ] {
            prev_env.push((name.to_string(), std::env::var(name).ok()));
            // SAFETY: tests on the same ark-cli test binary that touch
            // these vars must go through `E2eEnv`, so there is no
            // concurrent reader.
            unsafe { std::env::set_var(name, value) };
        }

        Self {
            state_dir,
            runtime_dir,
            config_dir,
            prev_env,
            spawned_pids: Mutex::new(Vec::new()),
        }
    }

    pub fn state_dir(&self) -> &Path {
        self.state_dir.path()
    }

    pub fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    pub fn config_dir(&self) -> &Path {
        self.config_dir.path()
    }

    /// Convenience: a `Command` for the `ark` binary with the three
    /// `ARK_*_DIR` vars stamped directly on the Command (in addition
    /// to the process-wide env var set by [`E2eEnv::new`]). Stamping
    /// on the Command is belt-and-braces — it guarantees the child
    /// sees the same paths even if something else in the test process
    /// mutates the vars between construction and spawn.
    pub fn ark(&self) -> Command {
        let mut c = ark_cmd();
        c.env("ARK_STATE_DIR", self.state_dir.path());
        c.env("ARK_RUNTIME_DIR", self.runtime_dir.path());
        c.env("ARK_CONFIG_DIR", self.config_dir.path());
        c
    }

    /// Register a child pid so Drop signals it at teardown. Safe to
    /// call repeatedly; callers typically do this immediately after
    /// `Command::spawn()` returns a `Child`.
    pub fn track_pid(&self, pid: u32) {
        self.spawned_pids.lock().expect("pid mutex").push(pid);
    }
}

/// Standalone helper mirroring [`E2eEnv::track_pid`] for parity with
/// the kit's function signature. Thin wrapper; prefer the method on
/// the guard when available.
pub fn track_pid(env: &E2eEnv, pid: u32) {
    env.track_pid(pid);
}

impl Default for E2eEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for E2eEnv {
    fn drop(&mut self) {
        // 1. Terminate tracked children. SIGTERM first, give them ~1s
        //    to clean up, then SIGKILL any stragglers. We swallow all
        //    errors — Drop must not panic during an already-panicking
        //    test or it aborts the whole test binary with a confusing
        //    "panicked during drop" diagnostic.
        let pids: Vec<u32> = match self.spawned_pids.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for pid in &pids {
            let _ = send_signal(*pid, nix::sys::signal::Signal::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_millis(1000);
        let mut remaining: Vec<u32> = pids.clone();
        while !remaining.is_empty() && Instant::now() < deadline {
            remaining.retain(|p| is_alive(*p));
            if remaining.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in &remaining {
            let _ = send_signal(*pid, nix::sys::signal::Signal::SIGKILL);
        }

        // 2. Restore env vars. Swapping order with (1) would be fine
        //    since the pids carry their own inherited env, but doing
        //    it after pid shutdown keeps any lingering child-facing
        //    env visible for the grace window.
        for (name, prior) in self.prev_env.drain(..) {
            // SAFETY: same contract as `new()` — single-threaded
            // access to these vars within the ark-cli test binary.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(&name, v),
                    None => std::env::remove_var(&name),
                }
            }
        }

        // 3. TempDir fields auto-drop here and delete the dirs.
    }
}

fn send_signal(pid: u32, sig: nix::sys::signal::Signal) -> nix::Result<()> {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), sig)
}

fn is_alive(pid: u32) -> bool {
    // Signal 0 probes without delivering — ESRCH means the pid is
    // gone, EPERM means it exists but we lack perms (treat as alive
    // to be safe).
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::ESRCH) => false,
        Err(_) => true,
    }
}

// ---- self-tests -----------------------------------------------------------
//
// These run as part of whichever integration-test binary embeds
// `mod e2e_support;` (currently `e2e.rs`). They are intentionally
// cheap and independent of `ARK_E2E` so they always execute — the
// point is to exercise the helpers themselves, not the scenarios.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_e2e_false_when_unset() {
        // Save + clear so the assertion is deterministic even when the
        // outer runner sets ARK_E2E=1. Restored at end.
        let prior = std::env::var("ARK_E2E").ok();
        // SAFETY: single-threaded test body, no concurrent readers of
        // the ARK_E2E var within this integration-test binary.
        unsafe { std::env::remove_var("ARK_E2E") };
        assert!(
            !require_e2e(),
            "require_e2e must return false with ARK_E2E unset"
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("ARK_E2E", v),
                None => std::env::remove_var("ARK_E2E"),
            }
        }
    }

    #[test]
    fn e2e_env_sets_and_restores_env_vars() {
        // Snapshot existing values so we can assert Drop restores them.
        let before: Vec<Option<String>> = ["ARK_STATE_DIR", "ARK_RUNTIME_DIR", "ARK_CONFIG_DIR"]
            .iter()
            .map(|k| std::env::var(k).ok())
            .collect();

        {
            let env = E2eEnv::new();
            // While the guard is alive, vars must point at tempdirs.
            assert_eq!(
                std::env::var("ARK_STATE_DIR").ok().as_deref(),
                Some(env.state_dir().to_str().unwrap()),
            );
            assert_eq!(
                std::env::var("ARK_RUNTIME_DIR").ok().as_deref(),
                Some(env.runtime_dir().to_str().unwrap()),
            );
            assert_eq!(
                std::env::var("ARK_CONFIG_DIR").ok().as_deref(),
                Some(env.config_dir().to_str().unwrap()),
            );
            // Tempdirs exist on disk.
            assert!(env.state_dir().is_dir());
            assert!(env.runtime_dir().is_dir());
            assert!(env.config_dir().is_dir());
        }

        // After drop, env vars are back to their pre-guard values.
        let after: Vec<Option<String>> = ["ARK_STATE_DIR", "ARK_RUNTIME_DIR", "ARK_CONFIG_DIR"]
            .iter()
            .map(|k| std::env::var(k).ok())
            .collect();
        assert_eq!(before, after, "Drop must restore prior env var values");
    }
}
