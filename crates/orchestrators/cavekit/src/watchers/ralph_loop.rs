//! T-079: Ralph-loop watcher (cavekit-orchestrator-cavekit R5).
//!
//! Watches `{cwd}/.claude/ralph-loop.local.md` and extracts:
//! - `iteration: N`
//! - `max_iterations: N` (optional)
//! - `status: ...`
//! - `started_at: ...` (optional; parsed but only surfaced on state change)
//!
//! Emits `Iteration { n, max }` whenever `iteration` changes and
//! `PhaseTransition { from: prev_status, to: new_status }` whenever `status`
//! changes. Identical re-writes (no change to iteration or status) produce
//! no events.
//!
//! The parser is forgiving: the kit does not pin the exact file shape, so
//! the scanner accepts any line that starts with `key:` (with optional
//! leading whitespace and YAML-front-matter delimiters like `---`). Other
//! shapes (HTML comments, bullet lists, etc.) are simply ignored.
//!
//! Disabled by default; activated only when the `enabled` gate is true
//! (maps to `config.orchestrator.cavekit.watch_ralph_loop`).

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, CancellationToken, EventSink};
use notify::{RecursiveMode, Watcher};

/// Small debounce for editor save-bursts. Smaller than the impl-tracking
/// watcher's 500ms — ralph-loop writes are programmatic and predictable.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Public entry point — see module docs.
pub async fn watch_ralph_loop(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    let dir = cwd.join(".claude");
    let file = dir.join("ralph-loop.local.md");

    // Create `.claude/` so we can watch it even before ralph writes the
    // file. Missing permissions → log and exit quietly.
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::debug!(
            path = %dir.display(),
            error = %e,
            "watch_ralph_loop: could not create .claude/ — exiting"
        );
        return Ok(());
    }

    let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = std_tx.send(res);
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;

    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let target_name = file
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let reader_handle = std::thread::spawn(move || {
        for res in std_rx {
            let Ok(event) = res else { continue };
            // macOS returns `/private`-prefixed paths for events under `/tmp`
            // and `/var/folders/...`. Match by filename against our watched
            // directory's single file-of-interest — we only watch the
            // `.claude/` dir non-recursively, so filename is unique enough.
            if !event
                .paths
                .iter()
                .any(|p| p.file_name() == Some(target_name.as_os_str()))
            {
                continue;
            }
            if async_tx.send(()).is_err() {
                break;
            }
        }
    });

    let mut prev_iteration: Option<u32> = None;
    let mut prev_status: Option<String> = None;

    // Initial read so startup state is reported to the bus.
    maybe_emit(&file, &id, &tx, &mut prev_iteration, &mut prev_status).await;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break;
            }
            maybe = async_rx.recv() => {
                if maybe.is_none() {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => { break; }
                    _ = tokio::time::sleep(DEBOUNCE) => {}
                }
                while async_rx.try_recv().is_ok() {}
                maybe_emit(&file, &id, &tx, &mut prev_iteration, &mut prev_status).await;
            }
        }
    }

    drop(watcher);
    let _ = reader_handle.join();
    Ok(())
}

/// Read the ralph-loop file, parse fields, and emit events for changes.
async fn maybe_emit(
    file: &std::path::Path,
    id: &AgentId,
    tx: &EventSink,
    prev_iteration: &mut Option<u32>,
    prev_status: &mut Option<String>,
) {
    let Ok(contents) = tokio::fs::read_to_string(file).await else {
        return;
    };
    let parsed = parse_ralph_fields(&contents);

    if let Some(n) = parsed.iteration {
        if Some(n) != *prev_iteration {
            *prev_iteration = Some(n);
            let _ = tx.send(AgentEvent::Iteration {
                id: id.clone(),
                n,
                max: parsed.max_iterations,
            });
        }
    }

    if let Some(new_status) = parsed.status.clone() {
        if prev_status.as_deref() != Some(new_status.as_str()) {
            let from = prev_status.clone();
            *prev_status = Some(new_status.clone());
            let _ = tx.send(AgentEvent::PhaseTransition {
                id: id.clone(),
                from,
                to: new_status,
            });
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RalphFields {
    iteration: Option<u32>,
    max_iterations: Option<u32>,
    status: Option<String>,
    #[allow(dead_code)]
    started_at: Option<String>,
}

/// Pull the four known keys out of a ralph-loop file. Accepts YAML-style
/// `key: value`, including inside an optional `---` front-matter block.
/// Anything else is ignored.
fn parse_ralph_fields(contents: &str) -> RalphFields {
    let mut out = RalphFields::default();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line == "---" {
            continue;
        }
        // Strip a leading `-` bullet if present (some folks render it as a list).
        let line = line.strip_prefix('-').map(|s| s.trim()).unwrap_or(line);
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        // Value may be quoted; strip matching quotes.
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(value);
        match key.as_str() {
            "iteration" => {
                if let Ok(n) = value.parse::<u32>() {
                    out.iteration = Some(n);
                }
            }
            "max_iterations" => {
                if let Ok(n) = value.parse::<u32>() {
                    out.max_iterations = Some(n);
                }
            }
            "status" => {
                if !value.is_empty() {
                    out.status = Some(value.to_string());
                }
            }
            "started_at" => {
                if !value.is_empty() {
                    out.started_at = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::channel;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_id() -> AgentId {
        AgentId::new("cavekit", "ralph")
    }

    fn drain(rx: &mut ark_types::EventReceiver) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(_)) => continue,
            }
        }
        out
    }

    async fn wait_for<F: Fn(&AgentEvent) -> bool>(
        rx: &mut ark_types::EventReceiver,
        pred: F,
        timeout: StdDuration,
    ) -> Vec<AgentEvent> {
        let start = std::time::Instant::now();
        let mut collected = Vec::new();
        while start.elapsed() < timeout {
            match tokio::time::timeout(StdDuration::from_millis(50), rx.recv()).await {
                Ok(Ok(ev)) => {
                    let matched = pred(&ev);
                    collected.push(ev);
                    if matched {
                        collected.extend(drain(rx));
                        return collected;
                    }
                }
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        collected
    }

    #[tokio::test]
    async fn disabled_returns_ok_immediately() {
        let tmp = TempDir::new().unwrap();
        let (tx, _rx) = channel(16);
        let cancel = CancellationToken::new();
        watch_ralph_loop(tmp.path().to_path_buf(), make_id(), tx, cancel, false)
            .await
            .expect("disabled ok");
    }

    #[tokio::test]
    async fn initial_read_emits_iteration_and_phase_transition() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ralph-loop.local.md"),
            "iteration: 1\nstatus: building\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_ralph_loop(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let events = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::PhaseTransition { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        let iter = events.iter().find_map(|e| match e {
            AgentEvent::Iteration { n, max, .. } => Some((*n, *max)),
            _ => None,
        });
        assert_eq!(iter, Some((1u32, None)));

        let phase = events.iter().find_map(|e| match e {
            AgentEvent::PhaseTransition { from, to, .. } => Some((from.clone(), to.clone())),
            _ => None,
        });
        assert_eq!(phase, Some((None, "building".to_string())));

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn iteration_and_status_changes_emit() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ralph-loop.local.md");
        std::fs::write(&file, "iteration: 1\nstatus: building\n").unwrap();

        let (tx, mut rx) = channel(64);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_ralph_loop(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Consume the initial events.
        let _initial = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::PhaseTransition { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        // Bump iteration only.
        std::fs::write(&file, "iteration: 2\nstatus: building\n").unwrap();
        let got = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Iteration { n: 2, .. }),
            StdDuration::from_secs(5),
        )
        .await;
        assert!(
            got.iter()
                .any(|e| matches!(e, AgentEvent::Iteration { n: 2, .. })),
            "expected Iteration(n=2), got {got:?}"
        );
        assert!(
            !got.iter()
                .any(|e| matches!(e, AgentEvent::PhaseTransition { .. })),
            "status unchanged — no PhaseTransition expected, got {got:?}"
        );

        // Change status only.
        std::fs::write(&file, "iteration: 2\nstatus: reviewing\n").unwrap();
        let got = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::PhaseTransition { to, .. } if to == "reviewing"),
            StdDuration::from_secs(5),
        )
        .await;
        let phase = got.iter().find_map(|e| match e {
            AgentEvent::PhaseTransition { from, to, .. } => Some((from.clone(), to.clone())),
            _ => None,
        });
        assert_eq!(
            phase,
            Some((Some("building".to_string()), "reviewing".to_string()))
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn identical_rewrite_emits_nothing_new() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ralph-loop.local.md");
        std::fs::write(&file, "iteration: 3\nstatus: building\n").unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();
        let handle = tokio::spawn(watch_ralph_loop(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let _ = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::PhaseTransition { .. }),
            StdDuration::from_secs(2),
        )
        .await;
        // Fully drain anything else still buffered.
        let _ = drain(&mut rx);

        // Rewrite identical content.
        std::fs::write(&file, "iteration: 3\nstatus: building\n").unwrap();
        tokio::time::sleep(StdDuration::from_millis(500)).await;
        let events = drain(&mut rx);
        let relevant: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::Iteration { .. } | AgentEvent::PhaseTransition { .. }
                )
            })
            .collect();
        assert!(
            relevant.is_empty(),
            "expected no new events on identical rewrite, got {relevant:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn cancel_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let (tx, _rx) = channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(watch_ralph_loop(
            tmp.path().to_path_buf(),
            make_id(),
            tx,
            cancel.clone(),
            true,
        ));

        tokio::time::sleep(StdDuration::from_millis(50)).await;
        cancel.cancel();
        let result = tokio::time::timeout(StdDuration::from_secs(2), handle)
            .await
            .expect("join timeout")
            .expect("join");
        result.expect("watcher ok");
    }

    // ---- parser tests ----

    #[test]
    fn parse_ralph_fields_happy() {
        let md = "iteration: 4\nmax_iterations: 10\nstatus: \"building\"\nstarted_at: 2026-04-14T00:00:00Z\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.iteration, Some(4));
        assert_eq!(p.max_iterations, Some(10));
        assert_eq!(p.status.as_deref(), Some("building"));
        assert_eq!(p.started_at.as_deref(), Some("2026-04-14T00:00:00Z"));
    }

    #[test]
    fn parse_ralph_fields_accepts_frontmatter_and_bullets() {
        let md = "---\n- iteration: 2\n- status: reviewing\n---\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.iteration, Some(2));
        assert_eq!(p.status.as_deref(), Some("reviewing"));
    }

    #[test]
    fn parse_ralph_fields_ignores_unknown_keys() {
        let md = "foo: bar\niteration: 9\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.iteration, Some(9));
        assert!(p.status.is_none());
    }

    /// T-121: bullet-format `- iter: N` (or `- iteration:`) outside a
    /// frontmatter block is still extracted — the parser strips a leading
    /// `-` on every line, not only within `---` fences.
    #[test]
    fn parse_ralph_fields_bullet_format_outside_frontmatter() {
        let md = "# Ralph loop state\n\
                  \n\
                  - iteration: 3\n\
                  - status: reviewing\n\
                  some prose follows\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.iteration, Some(3));
        assert_eq!(p.status.as_deref(), Some("reviewing"));
    }

    /// T-121: single-quoted values are unwrapped just like double-quoted ones.
    #[test]
    fn parse_ralph_fields_single_quoted_values() {
        let md = "status: 'building'\nstarted_at: '2026-04-15T00:00:00Z'\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.status.as_deref(), Some("building"));
        assert_eq!(p.started_at.as_deref(), Some("2026-04-15T00:00:00Z"));
    }

    /// T-121: non-numeric iteration values (garbage or negative) are dropped,
    /// leaving `iteration = None` rather than crashing.
    #[test]
    fn parse_ralph_fields_invalid_iteration_is_none() {
        let md = "iteration: not-a-number\nmax_iterations: -5\nstatus: x\n";
        let p = parse_ralph_fields(md);
        assert!(p.iteration.is_none());
        assert!(p.max_iterations.is_none());
        assert_eq!(p.status.as_deref(), Some("x"));
    }

    /// T-121: an empty file yields an all-`None` `RalphFields` — no panic,
    /// no partial extraction of sentinel values.
    #[test]
    fn parse_ralph_fields_empty_is_all_none() {
        let p = parse_ralph_fields("");
        assert_eq!(p, RalphFields::default());
    }

    /// T-121: missing `max_iterations` is tolerated — the other fields still
    /// come out, and `max_iterations` stays `None`.
    #[test]
    fn parse_ralph_fields_tolerates_missing_max() {
        let md = "iteration: 7\nstatus: running\n";
        let p = parse_ralph_fields(md);
        assert_eq!(p.iteration, Some(7));
        assert!(p.max_iterations.is_none());
        assert_eq!(p.status.as_deref(), Some("running"));
    }
}
