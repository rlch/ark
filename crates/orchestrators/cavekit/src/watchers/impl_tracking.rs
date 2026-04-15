//! T-077: Impl-tracking watcher (cavekit-orchestrator-cavekit R4).
//!
//! Watches `{cwd}/context/impl/impl-*.md` for create/modify/delete and emits:
//! - `TaskDone { task_id, label }` on a row's first transition to `DONE`.
//! - `Progress { done, total }` on any status change.
//!
//! Progress semantics (kit R4): `done = count(DONE) + 0.5 * count(PARTIAL)`
//! (rounded down to `u32`). The `total` per this packet comes from
//! [`super::build_site::extract_build_site_total`] — a parse of the
//! correlated `context/plans/build-site*.md` — falling back to the
//! in-memory count of parseable rows across `context/impl/impl-*.md` when
//! no build-site file is available or parseable (T-078).
//!
//! Build-site totals are re-read on every debounced re-parse. Build sites
//! change rarely and files are small (hundreds of rows max), so no caching
//! layer is warranted for v1.
//!
//! A 500ms debounce collapses bursts of filesystem events into a single
//! re-parse (many editors issue several writes in close succession when
//! saving a markdown file).
//!
//! Disabled by default; activated only when the `enabled` gate is true
//! (maps to `config.orchestrator.cavekit.watch_impl_tracking`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ark_types::{AgentEvent, AgentId, CancellationToken, EventSink};
use notify::{RecursiveMode, Watcher};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::build_site::extract_build_site_total;

/// Snapshot of impl-tracking state, published via a `watch::Sender` each time
/// the watcher re-parses. Consumed by the done-signal resolver in
/// `CavekitOrchestrator::run` (R9) to decide `Success` / `Failed` outcomes.
///
/// `pending_task_ids` contains every task whose current status is `PENDING`
/// or `IN PROGRESS`. These are the tasks that block a `Success` outcome:
/// when `Stop`/`SessionEnd` arrives the resolver waits up to 60s for them to
/// transition to `DONE` before giving up with `Failed`.
///
/// `total == 0` is the "no build-site observed" sentinel — the resolver
/// treats it as a trivial `Success` with empty artifacts (R9 case d).
#[derive(Clone, Debug)]
pub struct ImplTrackingSnapshot {
    pub done: u32,
    pub total: u32,
    pub pending_task_ids: Vec<String>,
    pub last_updated: Instant,
}

impl Default for ImplTrackingSnapshot {
    fn default() -> Self {
        Self {
            done: 0,
            total: 0,
            pending_task_ids: Vec::new(),
            last_updated: Instant::now(),
        }
    }
}

/// Spawn the impl-tracking watcher and return a `watch::Receiver` of its
/// latest snapshot plus the `JoinHandle` driving it.
///
/// Behaviour mirrors [`watch_impl_tracking`] — same event-bus emissions,
/// same 500ms debounce, same build-site total resolution — but additionally
/// publishes a [`ImplTrackingSnapshot`] on each re-parse. The orchestrator
/// uses the snapshot channel to decide whether to return `Success` or
/// `Failed` on engine `Stop`.
///
/// When `enabled = false` the watcher returns immediately and the snapshot
/// channel stays at its initial default (`total = 0`), which the resolver
/// interprets per R9 case d (unknown total → trivial Success).
pub fn spawn_impl_tracking_with_snapshot(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
) -> (
    watch::Receiver<ImplTrackingSnapshot>,
    JoinHandle<Result<()>>,
) {
    let (snap_tx, snap_rx) = watch::channel(ImplTrackingSnapshot::default());
    let handle = tokio::spawn(watch_impl_tracking_inner(
        cwd,
        id,
        tx,
        cancel,
        enabled,
        Some(snap_tx),
    ));
    (snap_rx, handle)
}

/// Debounce window for coalescing filesystem event bursts into one re-parse.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// Valid status tokens in the impl-tracking table.
const DONE: &str = "DONE";
const PARTIAL: &str = "PARTIAL";
const BLOCKED: &str = "BLOCKED";
const IN_PROGRESS: &str = "IN PROGRESS";
const PENDING: &str = "PENDING";

/// Public entry point — see module docs. Emits events on the bus only.
///
/// See [`spawn_impl_tracking_with_snapshot`] for the variant that also
/// publishes a [`ImplTrackingSnapshot`] consumed by the orchestrator's
/// done-signal resolver (R9).
pub async fn watch_impl_tracking(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
) -> Result<()> {
    watch_impl_tracking_inner(cwd, id, tx, cancel, enabled, None).await
}

/// Shared internal body. `snap_tx` is `Some` for
/// [`spawn_impl_tracking_with_snapshot`] callers and `None` for the legacy
/// bus-only [`watch_impl_tracking`] entry point.
async fn watch_impl_tracking_inner(
    cwd: PathBuf,
    id: AgentId,
    tx: EventSink,
    cancel: CancellationToken,
    enabled: bool,
    snap_tx: Option<watch::Sender<ImplTrackingSnapshot>>,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    let impl_dir = cwd.join("context").join("impl");
    // Ensure the directory exists so `notify` has something to watch. We
    // create it idempotently — the parent cwd is user-owned, and the
    // directory is an artifact of cavekit that may not yet exist on first
    // run. If creation fails we log and return gracefully.
    if let Err(e) = tokio::fs::create_dir_all(&impl_dir).await {
        tracing::debug!(
            path = %impl_dir.display(),
            error = %e,
            "watch_impl_tracking: could not create context/impl/ — exiting"
        );
        return Ok(());
    }

    // notify emits into a std::sync::mpsc channel. We bridge to a tokio
    // channel so the async select! below can poll both the bus and the
    // watcher uniformly.
    let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        // Intentionally ignore a send error: receiver dropped means the
        // watcher is being torn down.
        let _ = std_tx.send(res);
    })?;
    watcher.watch(&impl_dir, RecursiveMode::NonRecursive)?;

    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    // Blocking reader task: converts notify events (filtered by filename
    // shape `impl-*.md`) into tick signals on the async side. We use
    // filename-only matching because macOS FSEvents returns `/private`-
    // prefixed canonical paths that don't `==` the TempDir path we watched.
    let reader_handle = std::thread::spawn(move || {
        for res in std_rx {
            let Ok(event) = res else { continue };
            if !event.paths.iter().any(|p| is_tracked_impl_filename(p)) {
                continue;
            }
            if async_tx.send(()).is_err() {
                break;
            }
        }
    });

    // Initial parse so Progress reflects whatever's already on disk.
    let mut state: HashMap<String, String> = HashMap::new();
    reparse_and_emit(&cwd, &impl_dir, &id, &tx, &mut state, snap_tx.as_ref()).await;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break;
            }
            maybe = async_rx.recv() => {
                if maybe.is_none() {
                    // Watcher side dropped — exit gracefully.
                    break;
                }
                // Debounce: drain pending ticks + sleep. New ticks during
                // the sleep are swallowed by the drain on the next pass.
                tokio::select! {
                    _ = cancel.cancelled() => { break; }
                    _ = tokio::time::sleep(DEBOUNCE) => {}
                }
                // Drain any additional ticks queued up during the debounce
                // window; they collapse into the single re-parse below.
                while async_rx.try_recv().is_ok() {}
                reparse_and_emit(&cwd, &impl_dir, &id, &tx, &mut state, snap_tx.as_ref()).await;
            }
        }
    }

    drop(watcher);
    // reader_handle exits once std_rx is closed (watcher dropped above).
    let _ = reader_handle.join();
    Ok(())
}

/// Return `true` if `p` is an `impl-*.md` file immediately inside `impl_dir`.
/// Used by the directory-scanner (not the notify filter — see
/// [`is_tracked_impl_filename`] for filename-only matching on macOS).
fn is_tracked_impl_path(p: &Path, impl_dir: &Path) -> bool {
    if p.parent() != Some(impl_dir) {
        return false;
    }
    is_tracked_impl_filename(p)
}

/// Filename-only match for `impl-*.md`. Used by the notify event filter
/// because FSEvents on macOS canonicalizes TempDir paths through `/private`,
/// breaking parent-directory equality checks.
fn is_tracked_impl_filename(p: &Path) -> bool {
    if p.extension().and_then(|e| e.to_str()) != Some("md") {
        return false;
    }
    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    stem.starts_with("impl-")
}

/// Re-scan all `impl-*.md` files under `impl_dir`, diff against the prior
/// state map, and emit events for the observed transitions.
///
/// `Progress.total` is derived from the build-site file(s) correlated with
/// each impl filename via
/// [`super::build_site::extract_build_site_total`]. When no build-site is
/// resolvable we fall back to the in-memory row count (T-077 semantics).
async fn reparse_and_emit(
    cwd: &Path,
    impl_dir: &Path,
    id: &AgentId,
    tx: &EventSink,
    state: &mut HashMap<String, String>,
    snap_tx: Option<&watch::Sender<ImplTrackingSnapshot>>,
) {
    let mut rows: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut impl_filenames: Vec<String> = Vec::new();
    let Ok(mut read_dir) = tokio::fs::read_dir(impl_dir).await else {
        return;
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if !is_tracked_impl_path(&path, impl_dir) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            impl_filenames.push(name.to_string());
        }
        let Ok(contents) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        for (task_id, status, notes) in parse_rows(&contents) {
            rows.insert(task_id, (status, notes));
        }
    }

    // Emit TaskDone for new DONE transitions before mutating `state` so the
    // `label` reflects the current parse.
    for (task_id, (new_status, notes)) in &rows {
        let prev = state.get(task_id).cloned();
        if new_status == DONE && prev.as_deref() != Some(DONE) {
            let label = notes.as_deref().map(|n| truncate(n, 80));
            let _ = tx.send(AgentEvent::TaskDone {
                id: id.clone(),
                task_id: task_id.clone(),
                label,
            });
        }
    }

    // Update state and compute aggregate progress.
    for (task_id, (status, _)) in &rows {
        state.insert(task_id.clone(), status.clone());
    }

    let in_memory_total = rows.len() as u32;
    let total = resolve_build_site_total(cwd, &impl_filenames).unwrap_or(in_memory_total);
    let done_count = rows.values().filter(|(s, _)| s == DONE).count();
    let partial_count = rows.values().filter(|(s, _)| s == PARTIAL).count();
    // done = DONE + 0.5 * PARTIAL, floored to u32.
    let done_units = done_count as u32 + (partial_count as u32) / 2;
    let _ = tx.send(AgentEvent::Progress {
        id: id.clone(),
        done: done_units,
        total,
        label: None,
    });

    // R9: publish a fresh snapshot if a snapshot sender was wired. We send
    // unconditionally (even on "no change") so the resolver can observe
    // liveness via `last_updated`.
    if let Some(snap_tx) = snap_tx {
        // "Pending" for R9 purposes = not-yet-DONE work that the orchestrator
        // might still wait on. We treat PENDING and IN PROGRESS as blocking;
        // BLOCKED and PARTIAL are not pending — the former is an explicit
        // stop-the-line, the latter counts for half in Progress and isn't
        // something the orchestrator should wait on a status flip for.
        let mut pending: Vec<String> = rows
            .iter()
            .filter(|(_, (s, _))| s == PENDING || s == IN_PROGRESS)
            .map(|(tid, _)| tid.clone())
            .collect();
        pending.sort();
        let _ = snap_tx.send(ImplTrackingSnapshot {
            done: done_units,
            total,
            pending_task_ids: pending,
            last_updated: Instant::now(),
        });
    }
}

/// Resolve the build-site `Progress.total` for one-or-more observed impl
/// filenames.
///
/// The list is sorted for determinism, then the first filename that
/// resolves a non-`None` build-site total wins. This handles the canonical
/// single-impl case directly and gives predictable behaviour when multiple
/// impl files are present (e.g. an overview + domain splits): the first
/// alphabetical match that corresponds to an existing build-site drives
/// the total. Returns `None` when no impl filename resolves — the caller
/// falls back to the in-memory row count (T-077 semantics).
fn resolve_build_site_total(cwd: &Path, impl_filenames: &[String]) -> Option<u32> {
    if impl_filenames.is_empty() {
        return None;
    }
    let mut sorted: Vec<&str> = impl_filenames.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    for name in sorted {
        if let Some(n) = extract_build_site_total(cwd, name) {
            return Some(n);
        }
    }
    None
}

/// Parse `| T-XXX | STATUS | notes |` rows out of a markdown document.
///
/// Returns `(task_id, status, notes)` tuples. Rows whose status is not in
/// the valid set are silently skipped, as are non-table lines.
fn parse_rows(contents: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if !line.starts_with('|') || !line.ends_with('|') {
            continue;
        }
        // Strip leading and trailing pipes, split on the rest.
        let inner = &line[1..line.len() - 1];
        let parts: Vec<&str> = inner.split('|').map(|s| s.trim()).collect();
        if parts.len() < 2 {
            continue;
        }
        let task_id = parts[0];
        let status_raw = parts[1];
        // Guard the header row + the `| --- | --- |` separator row: a valid
        // task-id starts with "T-".
        if !task_id.starts_with("T-") {
            continue;
        }
        let status = normalize_status(status_raw);
        let Some(status) = status else { continue };
        let notes = parts
            .get(2)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        out.push((task_id.to_string(), status, notes));
    }
    out
}

/// Map a status cell to the canonical token set. Case-insensitive on the
/// letters but we preserve the canonical spelling we emit (`DONE`, etc.).
fn normalize_status(cell: &str) -> Option<String> {
    let upper = cell.to_ascii_uppercase();
    match upper.as_str() {
        "DONE" => Some(DONE.to_string()),
        "PARTIAL" => Some(PARTIAL.to_string()),
        "BLOCKED" => Some(BLOCKED.to_string()),
        "IN PROGRESS" => Some(IN_PROGRESS.to_string()),
        "PENDING" => Some(PENDING.to_string()),
        _ => None,
    }
}

/// Truncate `s` to at most `max` bytes on a char boundary, appending "…" if
/// truncation occurred. Used to keep TaskDone labels bounded.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::channel;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;
    use tokio::sync::broadcast::error::TryRecvError;

    fn make_id() -> AgentId {
        AgentId::new("cavekit", "impl-tracking")
    }

    /// Drain all currently-available events into a Vec.
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

    /// Wait until at least one matching event is present on `rx`, or timeout.
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
                        // Drain any remainder without blocking.
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
        let id = make_id();

        // Never cancels — if `enabled=false` doesn't early-exit, this hangs.
        watch_impl_tracking(tmp.path().to_path_buf(), id, tx, cancel, false)
            .await
            .expect("disabled ok");
    }

    #[tokio::test]
    async fn emits_task_done_and_progress_for_mixed_rows() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let impl_file = impl_dir.join("impl-alpha.md");
        std::fs::write(
            &impl_file,
            "# Alpha\n\n\
             | Task | Status | Notes |\n\
             | ---- | ------ | ----- |\n\
             | T-001 | DONE    | shipped |\n\
             | T-002 | PARTIAL | half  |\n\
             | T-003 | BLOCKED | waiting on X |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Initial parse fires immediately — wait for first Progress.
        let events = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Progress { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        let task_dones: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TaskDone { task_id, label, .. } => {
                    Some((task_id.clone(), label.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(task_dones.len(), 1, "expected exactly one TaskDone");
        assert_eq!(task_dones[0].0, "T-001");
        assert_eq!(task_dones[0].1.as_deref(), Some("shipped"));

        let progress: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Progress { done, total, .. } => Some((*done, *total)),
                _ => None,
            })
            .collect();
        assert!(!progress.is_empty(), "expected at least one Progress");
        let last = progress.last().unwrap();
        // done = 1 DONE + floor(0.5 * 1 PARTIAL) = 1; total = 3.
        assert_eq!(last.0, 1);
        assert_eq!(last.1, 3);

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn partial_to_done_transition_emits_task_done() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let impl_file = impl_dir.join("impl-beta.md");
        std::fs::write(
            &impl_file,
            "| Task | Status | Notes |\n\
             | --- | --- | --- |\n\
             | T-010 | PARTIAL | wip |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        // Drain the initial Progress. No TaskDone expected yet.
        let initial = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Progress { .. }),
            StdDuration::from_secs(2),
        )
        .await;
        assert!(
            !initial
                .iter()
                .any(|e| matches!(e, AgentEvent::TaskDone { .. })),
            "unexpected TaskDone on initial parse"
        );

        // Now transition PARTIAL -> DONE.
        std::fs::write(
            &impl_file,
            "| Task | Status | Notes |\n\
             | --- | --- | --- |\n\
             | T-010 | DONE    | finished at last |\n",
        )
        .unwrap();

        let transition = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::TaskDone { task_id, .. } if task_id == "T-010"),
            StdDuration::from_secs(5),
        )
        .await;
        assert!(
            transition
                .iter()
                .any(|e| matches!(e, AgentEvent::TaskDone { task_id, .. } if task_id == "T-010")),
            "expected TaskDone for T-010 after transition, got {transition:?}"
        );

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn malformed_file_does_not_panic() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let impl_file = impl_dir.join("impl-garbage.md");
        std::fs::write(
            &impl_file,
            "this is not a table\n\
             | header | only |\n\
             |---|---|---|\n\
             | T-001 | UNKNOWN | some note |\n\
             | T-002 | DONE | ok |\n\
             not a | table line | with pipes\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let events = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Progress { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        let progress = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Progress { done, total, .. } => Some((*done, *total)),
                _ => None,
            })
            .expect("expected a Progress");
        // Only T-002 DONE is parseable.
        assert_eq!(progress, (1, 1));

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    /// T-078 integration: Progress.total is sourced from the build-site
    /// file, not the in-memory impl row count.
    #[tokio::test]
    async fn progress_total_uses_build_site_when_present() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        let plans_dir = tmp.path().join("context").join("plans");
        std::fs::create_dir_all(&impl_dir).unwrap();
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Impl file has 2 rows (1 DONE, 1 PARTIAL) → in-memory total would
        // be 2.
        std::fs::write(
            impl_dir.join("impl-gamma.md"),
            "| Task | Status | Notes |\n\
             | --- | --- | --- |\n\
             | T-101 | DONE | done |\n\
             | T-102 | PARTIAL | half |\n",
        )
        .unwrap();

        // Matching build-site defines 7 tasks → that must drive Progress.total.
        std::fs::write(
            plans_dir.join("build-site-gamma.md"),
            "| T-101 | a | S |\n\
             | T-102 | b | S |\n\
             | T-103 | c | S |\n\
             | T-104 | d | S |\n\
             | T-105 | e | S |\n\
             | T-106 | f | S |\n\
             | T-107 | g | S |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let events = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Progress { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        let progress = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Progress { done, total, .. } => Some((*done, *total)),
                _ => None,
            })
            .expect("expected a Progress");
        // done = 1 DONE + floor(0.5 * 1 PARTIAL) = 1; total from build-site = 7.
        assert_eq!(progress, (1, 7));

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    /// T-078 fallback: when no build-site file exists, Progress.total falls
    /// back to the in-memory row count (preserves T-077 behaviour).
    #[tokio::test]
    async fn progress_total_falls_back_to_in_memory_without_build_site() {
        let tmp = TempDir::new().unwrap();
        let impl_dir = tmp.path().join("context").join("impl");
        std::fs::create_dir_all(&impl_dir).unwrap();
        // Deliberately no context/plans/ directory.
        std::fs::write(
            impl_dir.join("impl-delta.md"),
            "| Task | Status | Notes |\n\
             | --- | --- | --- |\n\
             | T-201 | DONE | one |\n\
             | T-202 | BLOCKED | two |\n",
        )
        .unwrap();

        let (tx, mut rx) = channel(32);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id.clone(),
            tx.clone(),
            cancel.clone(),
            true,
        ));

        let events = wait_for(
            &mut rx,
            |e| matches!(e, AgentEvent::Progress { .. }),
            StdDuration::from_secs(2),
        )
        .await;

        let progress = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Progress { done, total, .. } => Some((*done, *total)),
                _ => None,
            })
            .expect("expected a Progress");
        // In-memory fallback: 2 rows.
        assert_eq!(progress, (1, 2));

        cancel.cancel();
        let _ = tokio::time::timeout(StdDuration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn cancel_returns_ok() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("context").join("impl")).unwrap();
        let (tx, _rx) = channel(16);
        let cancel = CancellationToken::new();
        let id = make_id();

        let handle = tokio::spawn(watch_impl_tracking(
            tmp.path().to_path_buf(),
            id,
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

    // ---- parser-level tests ----

    #[test]
    fn parse_rows_happy_path() {
        let md = "| Task | Status | Notes |\n\
                  | --- | --- | --- |\n\
                  | T-001 | DONE | shipped it |\n\
                  | T-002 | partial | wip |\n\
                  | T-003 | In Progress | doing |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "T-001");
        assert_eq!(rows[0].1, "DONE");
        assert_eq!(rows[0].2.as_deref(), Some("shipped it"));
        assert_eq!(rows[1].1, "PARTIAL");
        assert_eq!(rows[2].1, "IN PROGRESS");
    }

    #[test]
    fn parse_rows_skips_unknown_status() {
        let md = "| T-001 | WHAT | note |\n";
        assert!(parse_rows(md).is_empty());
    }

    /// T-121: a 5-row table with only valid statuses → 5 rows parsed.
    #[test]
    fn parse_rows_valid_five_row_table() {
        let md = "| Task | Status | Notes |\n\
                  | --- | --- | --- |\n\
                  | T-001 | DONE | a |\n\
                  | T-002 | PARTIAL | b |\n\
                  | T-003 | BLOCKED | c |\n\
                  | T-004 | IN PROGRESS | d |\n\
                  | T-005 | PENDING | e |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 5);
        let statuses: Vec<_> = rows.iter().map(|(_, s, _)| s.as_str()).collect();
        assert!(statuses.contains(&"DONE"));
        assert!(statuses.contains(&"PARTIAL"));
        assert!(statuses.contains(&"BLOCKED"));
        assert!(statuses.contains(&"IN PROGRESS"));
        assert!(statuses.contains(&"PENDING"));
    }

    /// T-121: extra unknown columns past the notes column are silently ignored
    /// (the parser only looks at the first three cells).
    #[test]
    fn parse_rows_ignores_extra_unknown_columns() {
        let md = "| T-010 | DONE | notes here | owner | tier | when |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "T-010");
        assert_eq!(rows[0].1, "DONE");
        assert_eq!(rows[0].2.as_deref(), Some("notes here"));
    }

    /// T-121: row with only task_id + status (notes column missing entirely)
    /// yields `notes = None` but still parses.
    #[test]
    fn parse_rows_notes_column_missing_yields_none() {
        let md = "| T-020 | DONE |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "T-020");
        assert_eq!(rows[0].1, "DONE");
        assert!(rows[0].2.is_none(), "notes absent → None");
    }

    /// T-121: an empty notes cell (present but blank) also yields `None`.
    #[test]
    fn parse_rows_empty_notes_cell_is_none() {
        let md = "| T-030 | DONE |  |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].2.is_none());
    }

    /// T-121: malformed rows (missing trailing pipe, too few cells, non-task
    /// leading cell) are skipped without affecting neighbouring valid rows.
    #[test]
    fn parse_rows_malformed_rows_skipped() {
        let md = "no pipes at all\n\
                  | only one cell\n\
                  | T-001 | DONE | ok |\n\
                  | not-a-task | DONE | nope |\n\
                  | T-002 | DONE | ok2 |\n";
        let rows = parse_rows(md);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "T-001");
        assert_eq!(rows[1].0, "T-002");
    }

    /// T-121: `normalize_status` canonicalises case and rejects unknowns.
    #[test]
    fn normalize_status_canonicalises_all_variants() {
        assert_eq!(normalize_status("done").as_deref(), Some("DONE"));
        assert_eq!(normalize_status("DONE").as_deref(), Some("DONE"));
        assert_eq!(normalize_status("Partial").as_deref(), Some("PARTIAL"));
        assert_eq!(normalize_status("blocked").as_deref(), Some("BLOCKED"));
        assert_eq!(
            normalize_status("in progress").as_deref(),
            Some("IN PROGRESS")
        );
        assert_eq!(normalize_status("PENDING").as_deref(), Some("PENDING"));
        assert_eq!(normalize_status("REVIEWED"), None);
        assert_eq!(normalize_status(""), None);
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        let s = "hello world";
        assert_eq!(truncate(s, 100), "hello world");
        assert_eq!(truncate(s, 5), "hello…");
        // Multi-byte boundary.
        let emoji = "a".to_string() + &"é".repeat(50);
        let t = truncate(&emoji, 10);
        assert!(t.is_char_boundary(t.len() - "…".len()));
    }
}
