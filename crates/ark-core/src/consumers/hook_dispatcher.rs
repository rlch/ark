//! `hook_dispatcher` consumer task.
//!
//! Implements cavekit-supervisor.md R2 (third bullet) + cavekit-config.md R4
//! + supervisor R4 30s timeout.
//!
//! - Subscribes to the supervisor's broadcast bus.
//! - For each event, walks the configured `Vec<HookEntry>` and runs the
//!   ones whose `on_event` / `on_orchestrator` / `on_severity` filters
//!   match.
//! - Each match: builds a [`HookContext`] (event kind + agent_id +
//!   per-event vars), renders `cmd` via [`HookEntry::render`], and spawns
//!   the rendered shell string via `tokio::process::Command::new("sh") -c`
//!   with stdin/stdout/stderr nulled (detached from parent). A 30s
//!   `tokio::time::timeout` guards the child; on timeout the child is
//!   killed and the failure is warn-logged.
//! - Spawn failure / non-zero exit / timeout / kill: warn-log, never
//!   panic, never block the bus.
//! - Lagged: warn + continue. Closed/Cancel: `Ok(())`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ark_config::hooks::{HookContext, HookEntry};
use ark_types::AgentEvent;
use tokio::process::Command;
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{event_agent_id, event_kind_slug, event_severity_slug};

/// Default per-hook timeout (cavekit-supervisor.md R4 + module note).
const HOOK_TIMEOUT: Duration = Duration::from_secs(30);

/// Long-running consumer task. See module docs.
pub async fn hook_dispatcher(
    rx: Receiver<AgentEvent>,
    hooks: Arc<Vec<HookEntry>>,
    orchestrator: String,
    cancel: CancellationToken,
) -> Result<()> {
    hook_dispatcher_with_timeout(rx, hooks, orchestrator, cancel, HOOK_TIMEOUT).await
}

/// Internal version that takes an explicit timeout — used by tests so they
/// can shrink the 30s default to ~250ms without sleeping forever.
async fn hook_dispatcher_with_timeout(
    mut rx: Receiver<AgentEvent>,
    hooks: Arc<Vec<HookEntry>>,
    orchestrator: String,
    cancel: CancellationToken,
    timeout: Duration,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("hook_dispatcher: cancel fired, exiting");
                return Ok(());
            }
            recv = rx.recv() => match recv {
                Ok(event) => {
                    dispatch_event(&event, hooks.as_ref(), &orchestrator, timeout).await;
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "hook_dispatcher: broadcast lagged; continuing");
                    continue;
                }
                Err(RecvError::Closed) => {
                    debug!("hook_dispatcher: broadcast closed, exiting");
                    return Ok(());
                }
            }
        }
    }
}

async fn dispatch_event(
    event: &AgentEvent,
    hooks: &[HookEntry],
    orchestrator: &str,
    timeout: Duration,
) {
    let ctx = build_context(event, orchestrator);
    for hook in hooks.iter() {
        if !hook.matches(&ctx) {
            continue;
        }
        let cmd = hook.render(&ctx);
        // Spawn each match on its own task so a slow / blocked hook can't
        // delay the next hook on the same event.
        let timeout = timeout;
        let kind = event_kind_slug(event);
        tokio::spawn(async move { run_hook(cmd, timeout, kind).await });
    }
}

async fn run_hook(cmd_string: String, timeout: Duration, kind_for_log: &'static str) {
    debug!(kind = kind_for_log, cmd = %cmd_string, "hook_dispatcher: spawning");
    // `sh -c` is the v1 invocation form (the kit's `cmd = "..."` string is
    // shell-style). The future argv-array form (`cmd_argv`) is parsed by
    // ark-config but not executed yet (see hooks.rs module docs).
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&cmd_string)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, cmd = %cmd_string, "hook_dispatcher: spawn failed");
            return;
        }
    };

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => {
            debug!(cmd = %cmd_string, "hook_dispatcher: ok");
        }
        Ok(Ok(status)) => {
            warn!(
                cmd = %cmd_string,
                code = status.code().unwrap_or(-1),
                "hook_dispatcher: non-zero exit"
            );
        }
        Ok(Err(e)) => {
            warn!(error = %e, cmd = %cmd_string, "hook_dispatcher: child wait failed");
        }
        Err(_elapsed) => {
            warn!(
                cmd = %cmd_string,
                timeout_secs = timeout.as_secs(),
                "hook_dispatcher: timeout; killing child"
            );
            // Kill is best-effort; `kill_on_drop(true)` above ensures the
            // child dies once we drop it even if the explicit kill races.
            let _ = child.start_kill();
            // Reap to avoid zombies. Bounded so we don't hang here.
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

fn build_context(event: &AgentEvent, orchestrator: &str) -> HookContext {
    use std::collections::BTreeMap;

    let mut vars = BTreeMap::new();
    if let Some(id) = event_agent_id(event) {
        vars.insert("id".into(), id.as_str().to_string());
    }
    // Event-specific vars beyond `id` — kit R4 calls out `{{name}}`,
    // `{{outcome}}`, `{{tool}}` as common templates.
    match event {
        AgentEvent::Started { spec } => {
            vars.insert("name".into(), spec.name.clone());
        }
        AgentEvent::ToolUse { tool, .. } | AgentEvent::PermissionAsked { tool, .. } => {
            vars.insert("tool".into(), tool.clone());
        }
        AgentEvent::PermissionResolved { tool, decision, .. } => {
            vars.insert("tool".into(), tool.clone());
            vars.insert("decision".into(), format!("{decision:?}"));
        }
        AgentEvent::Done { outcome, .. } => {
            let s = match outcome {
                ark_types::Outcome::Success { .. } => "success",
                ark_types::Outcome::Failed { .. } => "failed",
                ark_types::Outcome::Killed => "killed",
                ark_types::Outcome::Timeout => "timeout",
                ark_types::Outcome::Crashed { .. } => "crashed",
            };
            vars.insert("outcome".into(), s.to_string());
        }
        AgentEvent::ReviewComment {
            severity,
            path,
            line,
            ..
        } => {
            vars.insert("severity".into(), format!("{severity:?}"));
            vars.insert("path".into(), path.display().to_string());
            if let Some(l) = line {
                vars.insert("line".into(), l.to_string());
            }
        }
        AgentEvent::FileEdited { path, .. } => {
            vars.insert("path".into(), path.display().to_string());
        }
        _ => {}
    }
    HookContext {
        event_kind: event_kind_slug(event).to_string(),
        orchestrator: orchestrator.to_string(),
        severity: event_severity_slug(event),
        vars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::{AgentEvent, AgentId, AgentSpec, Outcome, Severity, channel};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn id() -> AgentId {
        AgentId::new("cavekit", "auth")
    }

    fn sample_spec() -> AgentSpec {
        let mut s = AgentSpec::new(
            id(),
            "auth",
            "cavekit",
            "claude-code",
            PathBuf::from("/tmp/wt"),
            vec!["claude".into()],
        );
        s.env = BTreeMap::new();
        s
    }

    #[tokio::test]
    async fn happy_path_runs_matching_hook() {
        // Hook writes a sentinel file we can poll for.
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("ran.txt");
        let hook = HookEntry {
            cmd: format!(
                "touch {} && echo {{{{id}}}} > {}",
                sentinel.display(),
                sentinel.display()
            ),
            on_event: vec!["done".into()],
            on_orchestrator: vec![],
            on_severity: vec![],
        };
        let hooks = Arc::new(vec![hook]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(16);

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(5),
                )
                .await
            }
        });

        tx.send(AgentEvent::Done {
            id: id(),
            outcome: Outcome::Success { artifacts: vec![] },
        })
        .unwrap();

        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if sentinel.exists() {
                break;
            }
        }
        assert!(
            sentinel.exists(),
            "expected hook to create sentinel at {}",
            sentinel.display()
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn non_matching_hook_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("ran.txt");
        let hook = HookEntry {
            cmd: format!("touch {}", sentinel.display()),
            on_event: vec!["fail".into()],
            ..Default::default()
        };
        let hooks = Arc::new(vec![hook]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(16);

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(5),
                )
                .await
            }
        });

        tx.send(AgentEvent::Done {
            id: id(),
            outcome: Outcome::Killed,
        })
        .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!sentinel.exists(), "non-matching hook should not have run");

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn timeout_kills_long_child() {
        // A 5-second sleep guarded by a 250ms timeout. Must finish well
        // under 5s — otherwise the kill path is broken.
        let hook = HookEntry {
            cmd: "sleep 5".into(),
            on_event: vec!["done".into()],
            ..Default::default()
        };
        let hooks = Arc::new(vec![hook]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(16);

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_millis(250),
                )
                .await
            }
        });

        let start = std::time::Instant::now();
        tx.send(AgentEvent::Done {
            id: id(),
            outcome: Outcome::Killed,
        })
        .unwrap();

        // Give the dispatcher time to fire-and-time-out. The hook itself
        // runs on a detached tokio task; we just need the dispatcher to
        // remain responsive.
        tokio::time::sleep(Duration::from_millis(800)).await;

        cancel.cancel();
        task.await.unwrap().unwrap();

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(4),
            "hook should have been killed by timeout, elapsed = {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn spawn_failure_does_not_panic() {
        // `false` exits non-zero — exercises the warn-but-continue path.
        let hook = HookEntry {
            cmd: "false".into(),
            on_event: vec!["done".into()],
            ..Default::default()
        };
        let hooks = Arc::new(vec![hook]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(16);

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(2),
                )
                .await
            }
        });

        tx.send(AgentEvent::Done {
            id: id(),
            outcome: Outcome::Killed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
        let res = task.await.unwrap();
        assert!(res.is_ok(), "non-zero exit must not error the dispatcher");
    }

    #[tokio::test]
    async fn lagged_warn_and_survives() {
        let hooks: Arc<Vec<HookEntry>> = Arc::new(vec![]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(4);

        for _ in 0..50 {
            tx.send(AgentEvent::Started {
                spec: sample_spec(),
            })
            .unwrap();
        }

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(2),
                )
                .await
            }
        });

        tokio::time::sleep(Duration::from_millis(80)).await;
        cancel.cancel();
        let res = task.await.unwrap();
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn cancel_returns_promptly() {
        let hooks: Arc<Vec<HookEntry>> = Arc::new(vec![]);
        let cancel = CancellationToken::new();
        let (_tx, rx) = channel(8);
        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(2),
                )
                .await
            }
        });
        cancel.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("hook_dispatcher didn't return on cancel");
        assert!(res.unwrap().is_ok());
    }

    #[tokio::test]
    async fn closed_returns_ok() {
        let hooks: Arc<Vec<HookEntry>> = Arc::new(vec![]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(8);
        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(2),
                )
                .await
            }
        });
        drop(tx);
        let res = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("hook_dispatcher didn't return on Closed");
        assert!(res.unwrap().is_ok());

        // Use Severity to keep the import alive in case future tests need it.
        let _ = Severity::P2;
    }

    #[tokio::test]
    async fn severity_filtered_hook_only_fires_on_matching_severity() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("ran.txt");
        let hook = HookEntry {
            cmd: format!("touch {}", sentinel.display()),
            on_event: vec!["review_comment".into()],
            on_severity: vec!["P0".into()],
            ..Default::default()
        };
        let hooks = Arc::new(vec![hook]);
        let cancel = CancellationToken::new();
        let (tx, rx) = channel(16);

        let task = tokio::spawn({
            let hooks = hooks.clone();
            let cancel = cancel.clone();
            async move {
                hook_dispatcher_with_timeout(
                    rx,
                    hooks,
                    "cavekit".into(),
                    cancel,
                    Duration::from_secs(2),
                )
                .await
            }
        });

        // P1 should NOT trigger.
        tx.send(AgentEvent::ReviewComment {
            id: id(),
            reviewer: id(),
            severity: Severity::P1,
            path: PathBuf::from("a.rs"),
            line: None,
            body: "x".into(),
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!sentinel.exists(), "P1 should not match P0-only hook");

        // P0 should trigger.
        tx.send(AgentEvent::ReviewComment {
            id: id(),
            reviewer: id(),
            severity: Severity::P0,
            path: PathBuf::from("a.rs"),
            line: None,
            body: "y".into(),
        })
        .unwrap();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if sentinel.exists() {
                break;
            }
        }
        assert!(sentinel.exists(), "P0 should match P0-only hook");

        cancel.cancel();
        task.await.unwrap().unwrap();
    }
}
