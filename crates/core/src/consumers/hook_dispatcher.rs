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
//!   per-event vars) and picks an execution form via
//!   [`HookEntry::render_form`]:
//!   - `cmd_argv` present → direct exec (`Command::new(argv[0]).args(&argv[1..])`,
//!     no shell involved). Safe for any `ctx.vars` value by construction
//!     (F-058 safe path).
//!   - else `cmd` → `sh -c <shell-escaped>`. Every interpolated
//!     `{{var}}` is passed through `shlex::try_quote` before being
//!     substituted so a crafted filename containing `;`, `$()`, backticks,
//!     `&&`, etc. cannot escape into a separate shell command (F-058
//!     hardening). The first time the dispatcher sees a `cmd` entry with
//!     templated variables, it logs a one-shot warning recommending
//!     `cmd_argv` as the safer alternative.
//! - Spawn failure / non-zero exit / timeout / kill: warn-log, never
//!   panic, never block the bus. A 30s `tokio::time::timeout` guards the
//!   child; on timeout the child is killed and the failure is warn-logged.
//! - Lagged: warn + continue. Closed/Cancel: `Ok(())`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use ark_config::hooks::{HookContext, HookEntry, RenderedCommand};
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

        // F-058: `cmd` (shell-string) entries that use `{{var}}`
        // interpolation get a one-shot warn-log suggesting `cmd_argv`,
        // the safer form. The warning fires once per HookEntry per
        // process via `AtomicBool::swap`.
        if hook.cmd_argv.is_empty() && hook.shell_cmd_has_template() {
            log_shell_interpolation_warning_once(&hook.cmd);
        }

        let rendered = hook.render_form(&ctx);
        // Spawn each match on its own task so a slow / blocked hook can't
        // delay the next hook on the same event.
        let timeout = timeout;
        let kind = event_kind_slug(event);
        tokio::spawn(async move { run_hook(rendered, timeout, kind).await });
    }
}

/// Latches to `true` the first time a shell-form hook with templated
/// variables fires. Used to rate-limit the "prefer cmd_argv" warning
/// to one emission per process lifetime so logs don't flood.
static SHELL_INTERPOLATION_WARNED: AtomicBool = AtomicBool::new(false);

fn log_shell_interpolation_warning_once(cmd: &str) {
    if !SHELL_INTERPOLATION_WARNED.swap(true, Ordering::Relaxed) {
        warn!(
            cmd = %cmd,
            "hook_dispatcher: `cmd` (shell form) with `{{variable}}` substitution \
             detected — interpolated values are shell-escaped via shlex, but \
             `cmd_argv` (direct-exec) is the safer form and should be preferred"
        );
    }
}

async fn run_hook(rendered: RenderedCommand, timeout: Duration, kind_for_log: &'static str) {
    match rendered {
        RenderedCommand::Argv(argv) => run_hook_argv(argv, timeout, kind_for_log).await,
        RenderedCommand::Shell(cmd_string) => {
            run_hook_shell(cmd_string, timeout, kind_for_log).await
        }
    }
}

async fn run_hook_argv(argv: Vec<String>, timeout: Duration, kind_for_log: &'static str) {
    debug!(kind = kind_for_log, argv = ?argv, "hook_dispatcher: spawning argv");
    let Some((program, args)) = argv.split_first() else {
        warn!("hook_dispatcher: empty cmd_argv; skipping");
        return;
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, argv = ?argv, "hook_dispatcher: argv spawn failed");
            return;
        }
    };
    await_child(child, timeout, format!("argv={argv:?}")).await;
}

async fn run_hook_shell(cmd_string: String, timeout: Duration, kind_for_log: &'static str) {
    debug!(kind = kind_for_log, cmd = %cmd_string, "hook_dispatcher: spawning sh -c");
    // Legacy `sh -c` path. F-058: every interpolated `{{var}}` value has
    // already been shell-escaped by `HookEntry::render_form` → Shell.
    // Literal shell syntax the author wrote (pipes, redirects, etc.)
    // is preserved verbatim.
    let child = match Command::new("sh")
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
    await_child(child, timeout, cmd_string).await;
}

/// Shared wait/timeout/log path for both `argv` and `sh -c` children.
async fn await_child(mut child: tokio::process::Child, timeout: Duration, log_descr: String) {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => {
            debug!(cmd = %log_descr, "hook_dispatcher: ok");
        }
        Ok(Ok(status)) => {
            warn!(
                cmd = %log_descr,
                code = status.code().unwrap_or(-1),
                "hook_dispatcher: non-zero exit"
            );
        }
        Ok(Err(e)) => {
            warn!(error = %e, cmd = %log_descr, "hook_dispatcher: child wait failed");
        }
        Err(_elapsed) => {
            warn!(
                cmd = %log_descr,
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
            cmd_argv: Vec::new(),
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

    // -----------------------------------------------------------------
    // F-058: shell-injection hardening.
    //
    // The two branches — cmd_argv (direct exec, no shell) and cmd (sh -c,
    // with every interpolated value shlex-escaped) — must both produce
    // files whose *names* exactly match the crafted `path` variable,
    // proving that (a) argv never hits a shell parser and (b) cmd's
    // rendering prevents the interpolated value from escaping its
    // argument slot.
    //
    // The "witness" pattern: the test pre-creates a sentinel file at a
    // path the attacker would target if injection succeeded (e.g.
    // `canary.txt`). If the attacker's `; rm <canary>` fragment runs,
    // the canary disappears. A surviving canary post-run proves the
    // injection did NOT execute.
    // -----------------------------------------------------------------

    /// Wait up to ~4s for `cond` to become true, polling every 20ms.
    async fn wait_for<F: Fn() -> bool>(cond: F) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cond()
    }

    /// Unit-level: `render_form` with `cmd_argv` populated produces an
    /// Argv variant whose elements carry the raw (un-escaped) variable
    /// values. No sh -c invocation is implied by construction.
    #[test]
    fn f058_cmd_argv_render_form_is_argv_with_raw_values() {
        use ark_config::hooks::{HookContext, RenderedCommand};
        let hook = HookEntry {
            cmd: String::new(),
            cmd_argv: vec!["/bin/touch".into(), "{{path}}".into()],
            ..Default::default()
        };
        let mut vars = std::collections::BTreeMap::new();
        // Metacharacters in the value — a shell would interpret these,
        // but direct exec just passes them as argv bytes.
        vars.insert("path".into(), "a;rm b".into());
        let ctx = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        match hook.render_form(&ctx) {
            RenderedCommand::Argv(argv) => {
                assert_eq!(argv.len(), 2);
                assert_eq!(argv[0], "/bin/touch");
                // Critical: the metacharacters are passed through as-is.
                // exec() does not care.
                assert_eq!(argv[1], "a;rm b");
            }
            RenderedCommand::Shell(s) => panic!("expected Argv, got Shell({s:?})"),
        }
    }

    /// Unit-level: `render` (shell form) with a metacharacter-laden value
    /// produces a shlex-quoted token — literal `;`, `rm`, etc. stay
    /// inside the quote and cannot escape into a new command.
    #[test]
    fn f058_cmd_shell_render_quotes_shell_metacharacters() {
        use ark_config::hooks::HookContext;
        let hook = HookEntry {
            cmd: "touch {{path}}".into(),
            ..Default::default()
        };
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("path".into(), "a; rm -rf /tmp/evil".into());
        let ctx = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        let rendered = hook.render(&ctx);
        // shlex wraps the whole value in single quotes; the literal `;`
        // is inside the quote, so `sh -c` treats it as one argument.
        // The exact form depends on shlex's quoting policy, but the key
        // invariant is that the `; rm` fragment is NOT followed by a
        // shell command terminator (no unquoted `;` between tokens).
        assert!(
            rendered.contains("'a; rm -rf /tmp/evil'"),
            "expected shlex to single-quote the full value, got: {rendered:?}"
        );
        // Defensive: the rendered command must NOT contain an unquoted
        // `; rm ` fragment that the shell would execute.
        assert!(
            !rendered.contains("touch a; rm"),
            "unquoted `; rm` would be executed by sh -c: {rendered:?}"
        );
    }

    /// Unit-level: `$(whoami)` command-substitution syntax in a variable
    /// value gets quoted to a literal string — no subshell expansion.
    #[test]
    fn f058_cmd_shell_render_quotes_command_substitution() {
        use ark_config::hooks::HookContext;
        let hook = HookEntry {
            cmd: "echo {{tool}}".into(),
            ..Default::default()
        };
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("tool".into(), "$(whoami)".into());
        let ctx = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        let rendered = hook.render(&ctx);
        // The literal `$(...)` must be inside shlex quotes so sh -c
        // doesn't expand it.
        assert!(
            rendered.contains("'$(whoami)'"),
            "expected quoted `$(whoami)`, got: {rendered:?}"
        );
    }

    /// Unit-level: safe alphanumeric values pass through shlex
    /// unquoted, keeping the rendered command human-readable.
    #[test]
    fn f058_cmd_shell_render_leaves_safe_values_unquoted() {
        use ark_config::hooks::HookContext;
        let hook = HookEntry {
            cmd: "echo {{tool}}".into(),
            ..Default::default()
        };
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("tool".into(), "Read".into());
        let ctx = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(hook.render(&ctx), "echo Read");
    }

    /// Integration: with `cmd_argv`, the hook runs via direct exec. A
    /// file whose name contains shell metacharacters is created with
    /// that exact name — proving no shell ever parsed the argv.
    #[tokio::test]
    async fn f058_cmd_argv_direct_exec_preserves_metacharacter_filename() {
        let dir = tempfile::tempdir().unwrap();
        // The literal name (with a semicolon) is what exec() will see.
        // A shell would split at the `;` and try to run a second command.
        let target_name = "a;b.txt";
        let target_path = dir.path().join(target_name);

        let hook = HookEntry {
            cmd: String::new(),
            cmd_argv: vec![
                "/usr/bin/touch".into(),
                target_path.to_string_lossy().into_owned(),
            ],
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

        let created = wait_for(|| target_path.exists()).await;
        assert!(
            created,
            "cmd_argv hook must have created file with literal metachar name: {target_path:?}"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    /// Integration: a cmd-form hook with a crafted `{{path}}` value
    /// containing `; rm -rf <canary>` MUST NOT delete the canary file.
    /// The shlex-quoted value is passed to `touch` as one literal arg
    /// (creating a file whose name contains the metachars), and the
    /// canary survives.
    #[tokio::test]
    async fn f058_cmd_shell_form_blocks_injected_rm() {
        let dir = tempfile::tempdir().unwrap();
        // Pre-create the canary file the attacker would try to delete.
        let canary = dir.path().join("canary.txt");
        std::fs::write(&canary, b"precious").unwrap();

        // Use ReviewComment so `{{path}}` is populated by build_context
        // with the PathBuf we control.
        let injected_name = format!("a;rm -rf {}", canary.display());
        let injected_path = std::path::PathBuf::from(&injected_name);

        // `cmd` is legacy shell form with a templated variable. With
        // naive substitution, sh -c would execute `touch a; rm -rf <canary>`.
        // With shlex-escaped substitution, it runs
        // `touch 'a;rm -rf <canary>'` — creates a file named literally
        // `a;rm -rf <canary>` in the test dir, canary untouched.
        let hook = HookEntry {
            cmd: format!("cd {} && touch {{{{path}}}}", dir.path().display()),
            cmd_argv: Vec::new(),
            on_event: vec!["review_comment".into()],
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

        tx.send(AgentEvent::ReviewComment {
            id: id(),
            reviewer: id(),
            severity: Severity::P0,
            path: injected_path,
            line: None,
            body: "x".into(),
        })
        .unwrap();

        // Give the dispatcher + hook time to run.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // The canary MUST still exist — that's the whole point.
        assert!(
            canary.exists(),
            "canary was deleted — shell injection succeeded! \
             The `; rm` fragment in the `path` variable was interpreted by sh -c. \
             Canary path: {canary:?}"
        );
        assert_eq!(
            std::fs::read(&canary).unwrap(),
            b"precious",
            "canary content must be untouched"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }

    /// Regression: the existing happy-path rendering continues to work
    /// with the shlex-escaped substitution. Tested via `render` output.
    #[test]
    fn f058_safe_happy_path_render_unchanged() {
        use ark_config::hooks::HookContext;
        // The doctest's canonical hook.
        let hook = HookEntry {
            cmd: "notify-send 'ark: {{name}} done'".into(),
            ..Default::default()
        };
        let mut vars = std::collections::BTreeMap::new();
        vars.insert("name".into(), "scout".into());
        let ctx = HookContext {
            event_kind: "done".into(),
            orchestrator: "cavekit".into(),
            severity: None,
            vars,
        };
        assert_eq!(hook.render(&ctx), "notify-send 'ark: scout done'");
    }
}
