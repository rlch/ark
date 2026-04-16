//! ClaudeCodeOrchestrator — methodology-free passthrough orchestrator.
//!
//! Implements cavekit-orchestrator-claude-code.md R1 (detection), R2 (minimal
//! tab graph — builder-only) and R3 (Done/cancel handling). It owns exactly
//! one tab ("builder"), forwards engine events as-is via the shared bus,
//! waits on the engine's `Done` event or supervisor cancel, and returns an
//! `Outcome`.
//!
//! ## Orchestrator selection ordering
//!
//! `detect` here is a last-resort match: it returns `true` if the `claude`
//! binary is on `PATH`. The rule "does not steal from cavekit" is enforced
//! by the orchestrator selection order at the CLI layer (Tier 4): the CLI
//! runs `CavekitOrchestrator::detect` first and only falls back to
//! `ClaudeCodeOrchestrator::detect` when cavekit does not match.
//!
//! The PATH walk mirrors `ark_supervisor::engine_stub::preflight` (T-ACP.7
//! retired the former `ark_engines_claude_code::preflight`). We deliberately
//! avoid pulling in the `which` crate.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use ark_core::Orchestrator;
use ark_core::World;
use ark_types::{AgentEvent, AgentSpec, Outcome, TabRole};
use async_trait::async_trait;

// ------------------------------------------------------------------ detect --

/// Last-resort detect: returns `true` when a `claude` binary is on `PATH`.
///
/// See module docs for the "does not steal from cavekit" rule. This function
/// looks at the real process `PATH`; tests should use [`detect_with`] to
/// inject a synthetic `PATH`.
pub fn detect(_cwd: &Path) -> bool {
    match std::env::var_os("PATH") {
        Some(p) => detect_with(&p),
        None => false,
    }
}

/// Test-friendly detection: walk the provided `PATH` env value, look for an
/// executable named `claude`.
pub fn detect_with(path_env: &OsStr) -> bool {
    let name = OsStr::new("claude");
    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let mut with_ext = candidate.clone();
                with_ext.set_extension(ext);
                if is_executable_file(&with_ext) {
                    return true;
                }
            }
        }
    }
    false
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
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

// ------------------------------------------------------------- artifact diff --

/// Helper: list the files changed in `cwd` relative to `HEAD`, via
/// `git diff --name-only HEAD`.
///
/// Returns an empty `Vec` if `cwd` is not a git repo, if `git` isn't
/// available, or if the command fails for any reason. This matches the
/// non-git-cwd-is-valid requirement (R3).
pub fn artifact_diff_paths(cwd: &Path) -> Vec<PathBuf> {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("diff")
        .arg("--name-only")
        .arg("HEAD")
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(stdout) = std::str::from_utf8(&output.stdout) else {
        return Vec::new();
    };
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

// -------------------------------------------------------------- orchestrator --

/// Methodology-free passthrough orchestrator. See module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeOrchestrator;

impl ClaudeCodeOrchestrator {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Orchestrator for ClaudeCodeOrchestrator {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn engine(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, cwd: &Path) -> bool {
        detect(cwd)
    }

    async fn run(&self, spec: AgentSpec, world: World) -> Result<Outcome> {
        // R2: classic layout (config plumb-through not available here yet —
        // `default_layout()` below hardcodes the kit default).
        let layout_path = PathBuf::from(self.default_layout());

        // Open the builder tab. Mux is shared; we pass a stem-ish path and
        // rely on the mux to resolve it (the v1 ZellijMux rendered-KDL
        // writer does the resolution). See cavekit-mux-zellij R2 / T-026.
        let tab_handle = world
            .mux
            .create_tab(&spec.session, "builder", &layout_path)
            .await?;

        // Emit TabOpened so the bus / state writer sees the builder tab.
        let _ = world.events.send(AgentEvent::TabOpened {
            id: spec.id.clone(),
            parent: None,
            role: TabRole::Builder,
            tab_handle: tab_handle.clone(),
            label: "builder".to_string(),
        });

        // R2: forward nothing — engine events already flow through the bus.
        // Subscribe so the orchestrator can detect engine-emitted `Done`.
        let mut events = world.events.subscribe();

        // R3: wait on engine Done/Stop or supervisor cancel.
        let outcome = loop {
            tokio::select! {
                biased;
                _ = world.cancel.cancelled() => {
                    // Cancel → close builder tab, return Killed.
                    let _ = world.mux.close_tab(&tab_handle).await;
                    return Ok(Outcome::Killed);
                }
                res = events.recv() => {
                    match res {
                        Ok(AgentEvent::Done { id, outcome }) if id == spec.id => {
                            // Delegate to engine's Done outcome for the
                            // failure / crash / timeout branches; only
                            // Success gets the diff-artifact enrichment.
                            break outcome;
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Bus closed — treat as success with no artifacts.
                            break Outcome::Success { artifacts: Vec::new() };
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "claude-code orchestrator lagged on event bus");
                            continue;
                        }
                    }
                }
            }
        };

        // R3: On Success, enrich artifacts with the post-run git diff.
        let outcome = match outcome {
            Outcome::Success {
                artifacts: mut existing,
            } => {
                let diff = artifact_diff_paths(&spec.cwd);
                // Merge while preserving order and avoiding duplicates.
                for p in diff {
                    if !existing.iter().any(|e| e == &p) {
                        existing.push(p);
                    }
                }
                Outcome::Success {
                    artifacts: existing,
                }
            }
            other => other,
        };

        Ok(outcome)
    }
}

impl ClaudeCodeOrchestrator {
    /// Default layout stem. Kit default is `"classic"`. If a future version
    /// wires the full `Config` into this module, this should read
    /// `config.orchestrator.claude_code.default_layout`.
    pub fn default_layout(&self) -> &'static str {
        "classic"
    }
}

// -------------------------------------------------------------------- tests --

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::Config;
    use ark_mux_zellij::ZellijMux;
    use ark_mux_zellij::executor::{CommandOutput, StubExecutor};
    use ark_types::{AgentId, CancellationToken, EventSink, StateLayout};
    use std::ffi::OsString;
    use std::sync::Arc;
    use tempfile::TempDir;

    // --- detect -----------------------------------------------------------

    fn make_exec(dir: &Path, name: &str) {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&path, p).expect("chmod");
        }
    }

    #[test]
    fn detect_with_finds_claude_on_path() {
        let dir = TempDir::new().unwrap();
        make_exec(dir.path(), "claude");
        let path_env: OsString = dir.path().as_os_str().to_os_string();
        assert!(detect_with(&path_env));
    }

    #[test]
    fn detect_with_missing_returns_false() {
        let dir = TempDir::new().unwrap();
        // no claude binary here
        let path_env: OsString = dir.path().as_os_str().to_os_string();
        assert!(!detect_with(&path_env));
    }

    #[test]
    fn detect_with_non_executable_returns_false() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("claude");
        std::fs::write(&path, b"not executable").expect("write");
        // No chmod +x — should be rejected on unix.
        let path_env: OsString = dir.path().as_os_str().to_os_string();
        #[cfg(unix)]
        assert!(!detect_with(&path_env));
        #[cfg(not(unix))]
        {
            let _ = path_env; // windows accepts any regular file here
        }
    }

    #[test]
    fn detect_with_walks_multiple_entries() {
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        make_exec(b.path(), "claude");
        let joined = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert!(detect_with(&joined));
    }

    // --- artifact_diff_paths ---------------------------------------------

    #[test]
    fn artifact_diff_non_git_cwd_is_empty() {
        let dir = TempDir::new().unwrap();
        let paths = artifact_diff_paths(dir.path());
        assert!(paths.is_empty(), "expected empty, got {paths:?}");
    }

    #[test]
    fn artifact_diff_lists_modified_tracked_file() {
        let dir = TempDir::new().unwrap();
        let ok = run_git(dir.path(), &["init", "-q"]);
        if !ok {
            eprintln!("git not available — skipping artifact_diff_lists_modified_tracked_file");
            return;
        }
        // identity for commit
        run_git(dir.path(), &["config", "user.email", "t@t.test"]);
        run_git(dir.path(), &["config", "user.name", "T"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);

        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"hello\n").unwrap();
        run_git(dir.path(), &["add", "a.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);

        // modify
        std::fs::write(&file, b"hello\nworld\n").unwrap();

        let paths = artifact_diff_paths(dir.path());
        assert_eq!(paths, vec![PathBuf::from("a.txt")]);
    }

    fn run_git(cwd: &Path, args: &[&str]) -> bool {
        let status = Command::new("git").arg("-C").arg(cwd).args(args).status();
        matches!(status, Ok(s) if s.success())
    }

    // --- orchestrator trait surface --------------------------------------

    #[test]
    fn engine_returns_claude_code() {
        let o = ClaudeCodeOrchestrator::new();
        assert_eq!(o.engine(), "claude-code");
    }

    #[test]
    fn name_returns_claude_code() {
        let o = ClaudeCodeOrchestrator::new();
        assert_eq!(o.name(), "claude-code");
    }

    #[test]
    fn default_layout_is_classic() {
        let o = ClaudeCodeOrchestrator::new();
        assert_eq!(o.default_layout(), "classic");
    }

    // --- run() integration with ZellijMux(StubExecutor) ------------------

    /// Construct a test ZellijMux (inside-zellij variant so `create_tab`
    /// routes through the executor rather than the outside-zellij pty
    /// path that would try to spawn a real zellij binary).
    async fn test_mux(n: usize) -> (Arc<ZellijMux>, Arc<StubExecutor>) {
        let ok_status = tokio::process::Command::new("true")
            .status()
            .await
            .unwrap();
        let responses: Vec<CommandOutput> = (0..n)
            .map(|_| CommandOutput {
                status: ok_status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .collect();
        let (mux, stub) = ZellijMux::for_test_in_zellij(responses);
        (Arc::new(mux), stub)
    }

    fn count_create_tab_calls(stub: &StubExecutor) -> usize {
        stub.recorded_calls()
            .iter()
            .filter(|(_, args)| {
                let has_switch = args.iter().any(|a| a == "switch-session");
                let has_new_tab = args.iter().any(|a| a == "new-tab");
                has_switch || has_new_tab
            })
            .count()
    }

    fn close_argvs(stub: &StubExecutor) -> Vec<Vec<String>> {
        stub.recorded_calls()
            .into_iter()
            .filter(|(_, args)| args.iter().any(|a| a == "close-tab-at-index"))
            .map(|(_, args)| args)
            .collect()
    }

    fn make_spec(cwd: PathBuf) -> AgentSpec {
        AgentSpec::new(
            AgentId::new("claude-code", "run"),
            "run",
            "claude-code",
            "claude-code",
            cwd,
            vec!["claude".into()],
        )
    }

    fn make_world(
        spec: AgentSpec,
        mux: Arc<ZellijMux>,
    ) -> (World, EventSink, CancellationToken) {
        let (events, _rx) = ark_types::channel(16);
        let cancel = CancellationToken::new();
        let hooks_dir = PathBuf::from("/tmp/hooks");
        let state = Arc::new(StateLayout::new(
            PathBuf::from("/tmp/state"),
            PathBuf::from("/tmp/runtime"),
            PathBuf::from("/tmp/cfg"),
        ));
        let config = Arc::new(Config::placeholder());
        let world = World::new(
            spec,
            mux,
            events.clone(),
            cancel.clone(),
            hooks_dir,
            state,
            config,
        );
        (world, events, cancel)
    }

    #[tokio::test]
    async fn run_returns_success_on_engine_done() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        // Spawn a task that sends Done after a tiny delay.
        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let outcome = ClaudeCodeOrchestrator::new()
            .run(spec.clone(), world)
            .await
            .expect("run");

        // Success with no artifacts (non-git cwd).
        match outcome {
            Outcome::Success { artifacts } => {
                assert!(
                    artifacts.is_empty(),
                    "expected empty artifacts, got {artifacts:?}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }

        // Builder tab was created with classic layout. Argv inspection.
        assert_eq!(count_create_tab_calls(&stub), 1);
        let calls = stub.recorded_calls();
        let (_, argv) = calls
            .iter()
            .find(|(_, args)| args.iter().any(|a| a == "switch-session"))
            .expect("expected switch-session call");
        assert!(argv.iter().any(|a| a == spec.session.as_str()));
        let layout_pos = argv.iter().position(|a| a == "--layout").unwrap();
        assert_eq!(argv[layout_pos + 1], "classic");
    }

    #[tokio::test]
    async fn run_emits_tab_opened() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());
        let mut rx = events.subscribe();

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let run_handle = tokio::spawn(async move {
            ClaudeCodeOrchestrator::new()
                .run(spec.clone(), world)
                .await
                .expect("run");
        });

        let mut saw_tab_opened = false;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
                Ok(Ok(AgentEvent::TabOpened { role, label, .. })) => {
                    assert_eq!(role, TabRole::Builder);
                    assert_eq!(label, "builder");
                    saw_tab_opened = true;
                    break;
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => continue,
            }
        }
        assert!(saw_tab_opened, "did not observe TabOpened event");

        run_handle.await.expect("join");
    }

    #[tokio::test]
    async fn run_returns_killed_on_cancel() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, stub) = test_mux(4).await;
        let (world, _events, cancel) = make_world(spec.clone(), mux.clone());

        let handle = tokio::spawn(async move {
            ClaudeCodeOrchestrator::new()
                .run(spec, world)
                .await
                .expect("run")
        });

        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancel.cancel();

        let outcome = handle.await.expect("join");
        assert_eq!(outcome, Outcome::Killed);

        // Builder tab lives at index 0 (inside-zellij first tab). Assert
        // the close argv carries that index.
        let closes = close_argvs(&stub);
        assert_eq!(
            closes.len(),
            1,
            "expected one close_tab call, got {closes:?}"
        );
        let argv = &closes[0];
        let pos = argv.iter().position(|a| a == "close-tab-at-index").unwrap();
        assert_eq!(argv[pos + 1], "0");
    }

    #[tokio::test]
    async fn run_forwards_engine_outcome_non_success_untouched() {
        let cwd = TempDir::new().unwrap();
        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Failed {
                    reason: "boom".into(),
                },
            });
        });

        let outcome = ClaudeCodeOrchestrator::new()
            .run(spec, world)
            .await
            .expect("run");

        match outcome {
            Outcome::Failed { reason } => assert_eq!(reason, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_success_with_git_diff_fills_artifacts() {
        let cwd = TempDir::new().unwrap();
        // Setup a git repo with a modified tracked file — matches
        // artifact_diff_lists_modified_tracked_file setup.
        if !run_git(cwd.path(), &["init", "-q"]) {
            eprintln!("git not available — skipping run_success_with_git_diff_fills_artifacts");
            return;
        }
        run_git(cwd.path(), &["config", "user.email", "t@t.test"]);
        run_git(cwd.path(), &["config", "user.name", "T"]);
        run_git(cwd.path(), &["config", "commit.gpgsign", "false"]);
        let file = cwd.path().join("a.txt");
        std::fs::write(&file, b"hello\n").unwrap();
        run_git(cwd.path(), &["add", "a.txt"]);
        run_git(cwd.path(), &["commit", "-q", "-m", "init"]);
        std::fs::write(&file, b"hello\nworld\n").unwrap();

        let spec = make_spec(cwd.path().to_path_buf());
        let (mux, _stub) = test_mux(4).await;
        let (world, events, _cancel) = make_world(spec.clone(), mux.clone());

        let id = spec.id.clone();
        let sender = events.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = sender.send(AgentEvent::Done {
                id,
                outcome: Outcome::Success {
                    artifacts: Vec::new(),
                },
            });
        });

        let outcome = ClaudeCodeOrchestrator::new()
            .run(spec, world)
            .await
            .expect("run");

        match outcome {
            Outcome::Success { artifacts } => {
                assert_eq!(artifacts, vec![PathBuf::from("a.txt")]);
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }
}
